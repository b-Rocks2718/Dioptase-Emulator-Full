use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::memory::{AUDIO_SAMPLE_RATE_HZ, Memory};

// Flush in larger chunks because the emulator produces bursty audio writes and
// ffplay is more reliable when it can buffer a modest amount of PCM.
const AUDIO_FLUSH_INTERVAL_SAMPLES: usize = 2048;
// Samples the wall-clock worker may have handed to the writer thread but not
// yet written into the player pipe (250 ms). Beyond this the worker stops
// consuming the MMIO ring until the host catches up, so a slow or bursty host
// player delays audio instead of dropping it.
const AUDIO_BUFFERED_MAX_QUEUED_SAMPLES: usize = (AUDIO_SAMPLE_RATE_HZ as usize) / 4;
// Channel slots; the sample budget above is the real bound, so this only has
// to exceed the number of batches that budget can hold.
const AUDIO_BUFFERED_BATCH_QUEUE: usize = 64;
// With a player that reports playback progress, the most samples sent beyond
// what it has reported played (320 ms). The Windows player queues six 40 ms
// buffers (240 ms), so this keeps it fed while bounding the gap between the
// guest device position and what is audible.
const PLAYER_PROGRESS_LEAD_SAMPLES: u64 = (AUDIO_SAMPLE_RATE_HZ as u64) * 32 / 100;

// Shares host-audio error state and the queue consumed by the writer thread.
struct AudioSinkState {
    writer: BufWriter<ChildStdin>,
    samples_since_flush: usize,
    failed: bool,
    last_player_error: Arc<Mutex<Option<String>>>,
}

// Queues guest PCM samples and forwards them to the host audio process.
struct BufferedAudioSink {
    sender: SyncSender<Vec<i16>>,
    // Samples sent but not yet written to the pipe by the writer thread.
    queued_samples: Arc<AtomicUsize>,
    // Samples ever accepted by sender.
    sent_samples: AtomicU64,
    // PCM bytes the player reports as played, if it reports progress.
    player_played_bytes: Option<Arc<AtomicU64>>,
    failed: Arc<AtomicBool>,
    last_player_error: Arc<Mutex<Option<String>>>,
}

// Stores either a synchronous player connection or a buffered audio worker.
enum AudioSinkInner {
    Direct(Mutex<AudioSinkState>),
    Buffered(BufferedAudioSink),
}

// Serialize guest PCM samples into the host player stdin pipe.
// Emulator code writes signed 16-bit mono samples; the sink
// writes little-endian bytes to the child process and periodically flushes them.
// Invariants:
// - writes preserve guest sample ordering
// - each sample is emitted as exactly two little-endian bytes
// - direct mode applies host backpressure to guest audio output
// - buffered mode never blocks the caller; the wall-clock worker checks
//   `has_room_for` before consuming the device so samples are only dropped if
//   a caller ignores that check
// - once the host pipe fails, subsequent writes become no-ops to avoid log spam
pub struct AudioSink {
    inner: AudioSinkInner,
}

impl AudioSink {
    // Record a host-player error without losing the buffered sink state.
    fn report_host_audio_error(state: &mut AudioSinkState, operation: &str, err: &std::io::Error) {
        state.failed = true;
        if let Some(player_error) = state.last_player_error.lock().unwrap().clone() {
            eprintln!(
                "Warning: host audio stream {} failed: {} (ffplay: {})",
                operation, err, player_error
            );
        } else {
            eprintln!("Warning: host audio stream {} failed: {}", operation, err);
        }
    }

    // Append a batch of PCM samples while holding the sink queue lock once.
    fn write_samples_locked(state: &mut AudioSinkState, samples: &[i16]) {
        if state.failed {
            return;
        }

        for sample in samples {
            if let Err(err) = state.writer.write_all(&sample.to_le_bytes()) {
                Self::report_host_audio_error(state, "write", &err);
                return;
            }
        }

        state.samples_since_flush += samples.len();
        if state.samples_since_flush >= AUDIO_FLUSH_INTERVAL_SAMPLES {
            if let Err(err) = state.writer.flush() {
                Self::report_host_audio_error(state, "flush", &err);
                return;
            }
            state.samples_since_flush = 0;
        }
    }

    // Record an error reported by the buffered writer thread.
    fn report_buffered_audio_error(buffered: &BufferedAudioSink) {
        if buffered.failed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(player_error) = buffered.last_player_error.lock().unwrap().clone() {
            eprintln!(
                "Warning: host audio stream queue disconnected (ffplay: {})",
                player_error
            );
        } else {
            eprintln!("Warning: host audio stream queue disconnected");
        }
    }

    // Append one PCM sample to the synchronized host-audio queue.
    pub fn write_sample(&self, sample: i16) {
        self.write_samples(&[sample]);
    }

    // Serialize a contiguous batch of guest PCM samples with one sink lock.
    // Preserves sample ordering and writes each sample as exactly
    // two little-endian bytes to the host player stdin pipe.
    pub fn write_samples(&self, samples: &[i16]) {
        match &self.inner {
            AudioSinkInner::Direct(inner) => {
                let mut state = inner.lock().unwrap();
                Self::write_samples_locked(&mut state, samples);
            }
            AudioSinkInner::Buffered(buffered) => {
                if buffered.failed.load(Ordering::SeqCst) {
                    return;
                }
                match buffered.sender.try_send(samples.to_vec()) {
                    Ok(()) => {
                        buffered.queued_samples.fetch_add(samples.len(), Ordering::SeqCst);
                        buffered.sent_samples.fetch_add(samples.len() as u64, Ordering::SeqCst);
                    }
                    Err(TrySendError::Full(_)) => {
                        /*
                         * Unreachable for the wall-clock worker, which checks
                         * has_room_for() first. Never block here: blocking on a
                         * stalled player would also block emulator shutdown.
                         */
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        Self::report_buffered_audio_error(buffered);
                    }
                }
            }
        }
    }

    // Whether `samples` more samples may be sent now. Direct mode applies
    // backpressure by blocking, so it always has room. Buffered mode stays
    // within the writer-queue budget and, when the player reports progress,
    // within PLAYER_PROGRESS_LEAD_SAMPLES of what it has actually played, so
    // the guest device never runs ahead of audible output by more than that.
    // Only one producer (the wall-clock worker) calls this, so room reported
    // here cannot be taken by another sender before it writes.
    pub fn has_room_for(&self, samples: usize) -> bool {
        match &self.inner {
            AudioSinkInner::Direct(_) => true,
            AudioSinkInner::Buffered(buffered) => {
                if buffered.queued_samples.load(Ordering::SeqCst) + samples
                    > AUDIO_BUFFERED_MAX_QUEUED_SAMPLES
                {
                    return false;
                }
                match &buffered.player_played_bytes {
                    None => true,
                    Some(played_bytes) => {
                        let played = played_bytes.load(Ordering::SeqCst) / 2;
                        buffered.sent_samples.load(Ordering::SeqCst) + samples as u64
                            <= played + PLAYER_PROGRESS_LEAD_SAMPLES
                    }
                }
            }
        }
    }
}

// Owns the `ffplay` child process used for host audio playback.
// Callers clone `shared_sink()` and push guest PCM samples into it.
// Drop behavior closes stdin and waits for the player to exit.
pub struct AudioOutput {
    sink: Option<Arc<AudioSink>>,
    child: Option<Child>,
    writer_thread: Option<thread::JoinHandle<()>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
    progress_thread: Option<thread::JoinHandle<()>>,
}

impl AudioOutput {
    // Spawn the host audio writer and return its shared output handle.
    pub fn start(buffered: bool) -> Result<Self, String> {
        let last_player_error = Arc::new(Mutex::new(None));
        let player = HostPlayer::select()?;
        let reports_progress = player.reports_progress();
        let mut child = player
            .command()
            .stdin(Stdio::piped())
            .stdout(if reports_progress { Stdio::piped() } else { Stdio::null() })
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("failed to start {}: {}", player.name(), err))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ffplay stdin pipe was not available".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "ffplay stderr pipe was not available".to_string())?;
        let stderr_thread = Some(spawn_ffplay_stderr_thread(
            stderr,
            Arc::clone(&last_player_error),
        ));
        // Only the wall-clock worker can pace by progress; direct mode is
        // already paced by blocking writes.
        let mut progress_thread = None;
        let player_played_bytes = if reports_progress {
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| "audio player stdout pipe was not available".to_string())?;
            let played = Arc::new(AtomicU64::new(0));
            progress_thread = Some(spawn_player_progress_thread(stdout, Arc::clone(&played)));
            buffered.then_some(played)
        } else {
            None
        };
        let mut writer_thread = None;
        let sink = if buffered {
            let (sender, receiver) = sync_channel(AUDIO_BUFFERED_BATCH_QUEUE);
            let failed = Arc::new(AtomicBool::new(false));
            let queued_samples = Arc::new(AtomicUsize::new(0));
            writer_thread = Some(spawn_buffered_audio_writer(
                stdin,
                Arc::clone(&last_player_error),
                Arc::clone(&failed),
                Arc::clone(&queued_samples),
                receiver,
            ));
            Arc::new(AudioSink {
                inner: AudioSinkInner::Buffered(BufferedAudioSink {
                    sender,
                    queued_samples,
                    sent_samples: AtomicU64::new(0),
                    player_played_bytes,
                    failed,
                    last_player_error,
                }),
            })
        } else {
            Arc::new(AudioSink {
                inner: AudioSinkInner::Direct(Mutex::new(AudioSinkState {
                    writer: BufWriter::new(stdin),
                    samples_since_flush: 0,
                    failed: false,
                    last_player_error,
                })),
            })
        };

        Ok(AudioOutput {
            sink: Some(sink),
            child: Some(child),
            writer_thread,
            stderr_thread,
            progress_thread,
        })
    }

    // Clone the shared queue used to submit guest PCM samples.
    pub fn shared_sink(&self) -> Arc<AudioSink> {
        Arc::clone(
            self.sink
                .as_ref()
                .expect("audio sink must exist while output is alive"),
        )
    }
}

impl Drop for AudioOutput {
    // Close the sink, terminate a buffered player, and join its writer thread.
    fn drop(&mut self) {
        let buffered = self.writer_thread.is_some();
        self.sink.take();
        if buffered
            && let Some(child) = self.child.as_mut() {
                let _ = child.kill();
            }
        if let Some(writer_thread) = self.writer_thread.take() {
            let _ = writer_thread.join();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        if let Some(stderr_thread) = self.stderr_thread.take() {
            let _ = stderr_thread.join();
        }
        if let Some(progress_thread) = self.progress_thread.take() {
            let _ = progress_thread.join();
        }
    }
}

// Drain queued guest samples into the player's buffered stdin on a worker thread.
fn spawn_buffered_audio_writer(
    stdin: ChildStdin,
    last_player_error: Arc<Mutex<Option<String>>>,
    failed: Arc<AtomicBool>,
    queued_samples: Arc<AtomicUsize>,
    receiver: Receiver<Vec<i16>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut state = AudioSinkState {
            writer: BufWriter::new(stdin),
            samples_since_flush: 0,
            failed: false,
            last_player_error,
        };

        while let Ok(samples) = receiver.recv() {
            AudioSink::write_samples_locked(&mut state, &samples);
            // Flush every batch: the worker paces by player progress, and PCM
            // held back in this BufWriter would read as a stalled player.
            if !state.failed && state.samples_since_flush > 0 {
                if let Err(err) = state.writer.flush() {
                    AudioSink::report_host_audio_error(&mut state, "flush", &err);
                }
                state.samples_since_flush = 0;
            }
            // Release the budget only once the pipe has accepted the batch, so
            // a stalled player holds the worker off the MMIO ring.
            queued_samples.fetch_sub(samples.len(), Ordering::SeqCst);
            if state.failed {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        let _ = state.writer.flush();
    })
}

// Track the player's "p <bytes>" progress reports (see wsl_pcm_player.ps1).
// Ends when the player exits and closes its stdout.
fn spawn_player_progress_thread(
    stdout: ChildStdout,
    played_bytes: Arc<AtomicU64>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { return };
            if let Some(total) = line.trim().strip_prefix("p ").and_then(|n| n.parse::<u64>().ok()) {
                played_bytes.store(total, Ordering::SeqCst);
            }
        }
    })
}

// Capture the latest nonempty ffplay diagnostic without blocking sample writes.
fn spawn_ffplay_stderr_thread(
    stderr: ChildStderr,
    last_player_error: Arc<Mutex<Option<String>>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        *last_player_error.lock().unwrap() = Some(line);
                    }
                }
                Err(_) => return,
            }
        }
    })
}

// Environment variable that overrides host player selection: `ffplay`,
// `windows`, or `auto` (the default).
const AUDIO_PLAYER_ENV: &str = "DIOPTASE_AUDIO_PLAYER";
// Windows PowerShell path used when powershell.exe is not on PATH under WSL.
const WSL_POWERSHELL_FALLBACK: &str = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";
// waveOut player run through WSL interop; see the script header for why.
const WSL_PCM_PLAYER_SCRIPT: &str = include_str!("wsl_pcm_player.ps1");

// Host process that receives the guest PCM stream on stdin. Every player
// reads raw mono s16le at AUDIO_SAMPLE_RATE_HZ and exits at stdin EOF.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HostPlayer {
    Ffplay,
    // Windows waveOut via powershell.exe at the given path (WSL only).
    WindowsWaveOut(String),
}

impl HostPlayer {
    // Honor DIOPTASE_AUDIO_PLAYER, else prefer Windows playback under WSL,
    // because WSLg's PulseAudio sink cannot sustain long streams.
    fn select() -> Result<Self, String> {
        let requested = std::env::var(AUDIO_PLAYER_ENV).unwrap_or_default();
        match requested.as_str() {
            "ffplay" => Ok(HostPlayer::Ffplay),
            "windows" => find_wsl_powershell().map(HostPlayer::WindowsWaveOut).ok_or_else(|| {
                format!(
                    "{}=windows requires WSL interop with powershell.exe on PATH or at {}",
                    AUDIO_PLAYER_ENV, WSL_POWERSHELL_FALLBACK
                )
            }),
            "" | "auto" => Ok(if running_under_wsl() {
                find_wsl_powershell().map_or(HostPlayer::Ffplay, HostPlayer::WindowsWaveOut)
            } else {
                HostPlayer::Ffplay
            }),
            other => Err(format!(
                "{} must be `ffplay`, `windows`, or `auto`, got `{}`",
                AUDIO_PLAYER_ENV, other
            )),
        }
    }

    // Whether the player writes "p <bytes>" progress lines to stdout. Its
    // stdout must then be drained even when nothing paces by it, or the
    // player would block once the pipe fills.
    fn reports_progress(&self) -> bool {
        matches!(self, HostPlayer::WindowsWaveOut(_))
    }

    fn name(&self) -> &'static str {
        match self {
            HostPlayer::Ffplay => "ffplay",
            HostPlayer::WindowsWaveOut(_) => "Windows audio player (powershell.exe)",
        }
    }

    fn command(&self) -> Command {
        match self {
            HostPlayer::Ffplay => {
                let mut command = Command::new("ffplay");
                command.args(ffplay_args());
                command
            }
            HostPlayer::WindowsWaveOut(powershell) => {
                let mut command = Command::new(powershell);
                command.args(windows_player_args());
                command
            }
        }
    }
}

// WSL exposes Windows interop through this binfmt entry.
fn running_under_wsl() -> bool {
    std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
        || std::env::var_os("WSL_DISTRO_NAME").is_some()
}

// Locate powershell.exe on PATH or at its standard Windows location.
fn find_wsl_powershell() -> Option<String> {
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("powershell.exe"))
            .find(|candidate| candidate.is_file())
    });
    on_path
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| {
            std::path::Path::new(WSL_POWERSHELL_FALLBACK)
                .is_file()
                .then(|| WSL_POWERSHELL_FALLBACK.to_string())
        })
}

// Pass the embedded script with -EncodedCommand (base64 of UTF-16LE), so no
// script file has to be visible from Windows. stdin stays free for PCM.
fn windows_player_args() -> Vec<String> {
    let script = format!("$Rate = {}\n{}", AUDIO_SAMPLE_RATE_HZ, WSL_PCM_PLAYER_SCRIPT);
    let utf16le: Vec<u8> = script.encode_utf16().flat_map(|unit| unit.to_le_bytes()).collect();
    vec![
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-ExecutionPolicy".to_string(),
        "Bypass".to_string(),
        "-EncodedCommand".to_string(),
        base64_encode(&utf16le),
    ]
}

// Standard base64 with padding (RFC 4648), as -EncodedCommand expects.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// Build the ffplay arguments for the guest's mono PCM stream.
fn ffplay_args() -> Vec<String> {
    vec![
        "-loglevel".to_string(),
        "error".to_string(),
        "-nodisp".to_string(),
        "-autoexit".to_string(),
        "-f".to_string(),
        "s16le".to_string(),
        "-ar".to_string(),
        AUDIO_SAMPLE_RATE_HZ.to_string(),
        "-ac".to_string(),
        "1".to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
    ]
}

// Host audio policy for emulator runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioMode {
    // Do not start a host audio player. The MMIO audio device still advances on
    // emulated device ticks so guest-visible timing stays intact.
    Disabled,
    // Mirror the emulated device output to the host player as core 0 advances
    // the shared device tick.
    Emulated,
    // Drive the MMIO audio device from wall-clock time on a helper thread so
    // host playback stays intelligible even when emulation is slow. This is an
    // opt-in debugging mode because it changes guest-visible timing.
    Fast,
}

// Wall-clock audio is consumed in 10 ms batches.
const FAST_AUDIO_BATCH_SAMPLES: usize = (AUDIO_SAMPLE_RATE_HZ as usize) / 100;
// Cap catch-up after a host stall so the worker never builds unbounded work.
const FAST_AUDIO_MAX_CATCH_UP_BATCHES: usize = 8;
// Longest the worker holds the device still for a lagging host player. Past
// this the worker consumes and discards samples so the guest's playback
// progress timeout (kernel/audio.c) never mistakes a host stall for a dead
// device.
const FAST_AUDIO_MAX_HOST_STALL: Duration = Duration::from_secs(1);

// Owns the host player for a run and, in fast mode, the worker thread that
// consumes the MMIO audio device on wall-clock time. Dropping it stops and
// joins the worker before closing the player.
pub struct AudioPlayback {
    output: AudioOutput,
    worker: Option<(Arc<AtomicBool>, thread::JoinHandle<()>)>,
}

impl AudioPlayback {
    // Start host playback for `requested_mode`. Returns the mode actually in
    // effect: if the player cannot start, audio falls back to Disabled.
    pub fn start(requested_mode: AudioMode, memory: Arc<Memory>) -> (AudioMode, Option<Self>) {
        if requested_mode == AudioMode::Disabled {
            return (AudioMode::Disabled, None);
        }
        let fast = requested_mode == AudioMode::Fast;
        let output = match AudioOutput::start(fast) {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Warning: failed to start host audio output: {}", err);
                return (AudioMode::Disabled, None);
            }
        };
        let worker = fast.then(|| {
            let stop = Arc::new(AtomicBool::new(false));
            let handle = spawn_fast_audio_worker(memory, output.shared_sink(), Arc::clone(&stop));
            (stop, handle)
        });
        (requested_mode, Some(AudioPlayback { output, worker }))
    }

    // Sink that core 0 feeds on emulated ticks; None in wall-clock mode.
    pub fn emulated_sink(&self) -> Option<Arc<AudioSink>> {
        if self.worker.is_some() {
            None
        } else {
            Some(self.output.shared_sink())
        }
    }
}

impl Drop for AudioPlayback {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.worker.take() {
            stop.store(true, Ordering::SeqCst);
            let _ = handle.join();
        }
    }
}

// Duration of `batch_count` wall-clock audio batches.
fn audio_batch_duration(batch_count: usize) -> Duration {
    Duration::from_nanos(
        ((FAST_AUDIO_BATCH_SAMPLES as u64) * (batch_count as u64) * 1_000_000_000u64)
            / (AUDIO_SAMPLE_RATE_HZ as u64),
    )
}

// Consume MMIO audio samples in 10 ms wall-clock batches until `stop`.
// Invariants:
// - only this thread advances the audio consumer in fast mode (core 0 skips
//   its emulated-tick audio path while this runs)
// - catch-up is capped at FAST_AUDIO_MAX_CATCH_UP_BATCHES per wake-up
// - while the host queue is full the device is not consumed at all (READ_IDX
//   holds still), so short host stalls pause playback rather than losing
//   samples; the deadline restarts afterwards so there is no catch-up burst
// - a stall longer than FAST_AUDIO_MAX_HOST_STALL discards samples instead,
//   keeping device progress visible to the guest
fn spawn_fast_audio_worker(
    memory: Arc<Memory>,
    sink: Arc<AudioSink>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let batch_duration = audio_batch_duration(1);
        let mut next_deadline = Instant::now() + batch_duration;
        let mut batch =
            Vec::with_capacity(FAST_AUDIO_BATCH_SAMPLES * FAST_AUDIO_MAX_CATCH_UP_BATCHES);
        // When the host queue first had no room in the current stall.
        let mut host_stall_start: Option<Instant> = None;
        // Report the first discarding stall only, to avoid log spam.
        let mut stall_reported = false;

        while !stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now < next_deadline {
                thread::sleep(next_deadline.duration_since(now));
                continue;
            }
            let late_batches = (now.duration_since(next_deadline).as_nanos()
                / batch_duration.as_nanos().max(1)) as usize;
            let mut batch_count = 1 + late_batches;
            if batch_count > FAST_AUDIO_MAX_CATCH_UP_BATCHES {
                batch_count = FAST_AUDIO_MAX_CATCH_UP_BATCHES;
                next_deadline = now + batch_duration;
            } else {
                next_deadline += audio_batch_duration(batch_count);
            }
            let sample_count = batch_count * FAST_AUDIO_BATCH_SAMPLES;
            if !sink.has_room_for(sample_count) {
                let stalled_since = *host_stall_start.get_or_insert(now);
                if now.duration_since(stalled_since) < FAST_AUDIO_MAX_HOST_STALL {
                    next_deadline = now + batch_duration;
                    continue;
                }
                if !stall_reported {
                    eprintln!(
                        "Warning: host audio player made no progress for {:?}; discarding audio until it catches up",
                        FAST_AUDIO_MAX_HOST_STALL
                    );
                    stall_reported = true;
                }
                memory.consume_audio_wallclock_samples(sample_count, &mut batch);
                continue;
            }
            host_stall_start = None;
            memory.consume_audio_wallclock_samples(sample_count, &mut batch);
            if !batch.is_empty() {
                sink.write_samples(&batch);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Configure ffplay for the guest's signed 16-bit mono PCM stream.
    #[test]
    fn ffplay_args_match_guest_audio_format() {
        let args = ffplay_args();
        assert!(args.contains(&"s16le".to_string()));
        assert!(args.contains(&AUDIO_SAMPLE_RATE_HZ.to_string()));
        assert!(args.contains(&"1".to_string()));
        assert!(args.contains(&"pipe:0".to_string()));
    }

    // The wall-clock worker relies on has_room_for() to pause device
    // consumption, instead of dropping samples, while the player lags.
    #[test]
    fn buffered_sink_room_tracks_unwritten_samples() {
        let (sender, receiver) = sync_channel(AUDIO_BUFFERED_BATCH_QUEUE);
        let queued_samples = Arc::new(AtomicUsize::new(0));
        let sink = AudioSink {
            inner: AudioSinkInner::Buffered(BufferedAudioSink {
                sender,
                queued_samples: Arc::clone(&queued_samples),
                sent_samples: AtomicU64::new(0),
                player_played_bytes: None,
                failed: Arc::new(AtomicBool::new(false)),
                last_player_error: Arc::new(Mutex::new(None)),
            }),
        };

        assert!(sink.has_room_for(AUDIO_BUFFERED_MAX_QUEUED_SAMPLES));
        sink.write_samples(&vec![0; AUDIO_BUFFERED_MAX_QUEUED_SAMPLES]);
        assert!(
            !sink.has_room_for(1),
            "a full budget of unwritten samples must hold the worker off the device"
        );

        // Model the writer thread draining that batch into the pipe.
        let batch = receiver.recv().unwrap();
        queued_samples.fetch_sub(batch.len(), Ordering::SeqCst);
        assert!(
            sink.has_room_for(AUDIO_BUFFERED_MAX_QUEUED_SAMPLES),
            "budget must be released once the writer has written the batch"
        );
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    // The Windows player script reads its sample rate from a prepended `$Rate`.
    #[test]
    fn windows_player_script_receives_sample_rate() {
        let args = windows_player_args();
        assert_eq!(args[args.len() - 2], "-EncodedCommand");
        assert!(WSL_PCM_PLAYER_SCRIPT.contains("[DioptasePcmPlayer]::Run($Rate)"));
    }

    // With a progress-reporting player, the worker may run at most
    // PLAYER_PROGRESS_LEAD_SAMPLES ahead of what the player says it played.
    #[test]
    fn progress_reports_bound_samples_sent_ahead_of_playback() {
        let (sender, receiver) = sync_channel(AUDIO_BUFFERED_BATCH_QUEUE);
        let played_bytes = Arc::new(AtomicU64::new(0));
        let sink = AudioSink {
            inner: AudioSinkInner::Buffered(BufferedAudioSink {
                sender,
                queued_samples: Arc::new(AtomicUsize::new(0)),
                sent_samples: AtomicU64::new(0),
                player_played_bytes: Some(Arc::clone(&played_bytes)),
                failed: Arc::new(AtomicBool::new(false)),
                last_player_error: Arc::new(Mutex::new(None)),
            }),
        };
        let lead = PLAYER_PROGRESS_LEAD_SAMPLES as usize;

        // Send the lead in two halves, draining the writer queue after each
        // (as the writer thread does), so only the progress bound applies.
        for half in [lead / 2, lead - lead / 2] {
            assert!(sink.has_room_for(half));
            sink.write_samples(&vec![0; half]);
            let batch = receiver.recv().unwrap();
            if let AudioSinkInner::Buffered(buffered) = &sink.inner {
                buffered.queued_samples.fetch_sub(batch.len(), Ordering::SeqCst);
            }
        }
        assert!(
            !sink.has_room_for(1),
            "a full lead of unplayed samples must hold the worker off the device"
        );

        played_bytes.store(1000 * 2, Ordering::SeqCst);
        assert!(sink.has_room_for(1000), "played samples must free the same amount of lead");
        assert!(!sink.has_room_for(1001));
    }

    // Encode each signed sample as two little-endian bytes.
    #[test]
    fn sample_encoding_is_little_endian() {
        assert_eq!(i16::from_le_bytes([0x34, 0x12]), 0x1234);
        assert_eq!((-2i16).to_le_bytes(), [0xFE, 0xFF]);
    }
}
