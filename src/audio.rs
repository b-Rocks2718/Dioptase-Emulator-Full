use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::memory::{AUDIO_SAMPLE_RATE_HZ, Memory};

// Flush in larger chunks because the emulator produces bursty audio writes and
// ffplay is more reliable when it can buffer a modest amount of PCM.
const AUDIO_FLUSH_INTERVAL_SAMPLES: usize = 2048;
const AUDIO_BUFFERED_BATCH_QUEUE: usize = 4;

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
// - buffered mode never blocks the caller; it may drop host samples if the
//   player falls behind so audio-fast mode does not stall guest device time
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
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {
                        /*
                         * Audio-fast mode is a wall-clock debugging mode. If
                         * the host player stops draining, dropping host samples
                         * is preferable to stalling MMIO device time.
                         */
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        Self::report_buffered_audio_error(buffered);
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
}

impl AudioOutput {
    // Spawn the host audio writer and return its shared output handle.
    pub fn start(buffered: bool) -> Result<Self, String> {
        let last_player_error = Arc::new(Mutex::new(None));
        let mut child = Command::new("ffplay")
            .args(ffplay_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("failed to start ffplay: {}", err))?;
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
        let mut writer_thread = None;
        let sink = if buffered {
            let (sender, receiver) = sync_channel(AUDIO_BUFFERED_BATCH_QUEUE);
            let failed = Arc::new(AtomicBool::new(false));
            writer_thread = Some(spawn_buffered_audio_writer(
                stdin,
                Arc::clone(&last_player_error),
                Arc::clone(&failed),
                receiver,
            ));
            Arc::new(AudioSink {
                inner: AudioSinkInner::Buffered(BufferedAudioSink {
                    sender,
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
    }
}

// Drain queued guest samples into the player's buffered stdin on a worker thread.
fn spawn_buffered_audio_writer(
    stdin: ChildStdin,
    last_player_error: Arc<Mutex<Option<String>>>,
    failed: Arc<AtomicBool>,
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
            if state.failed {
                failed.store(true, Ordering::SeqCst);
                return;
            }
        }
        let _ = state.writer.flush();
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
            memory.consume_audio_wallclock_samples(batch_count * FAST_AUDIO_BATCH_SAMPLES, &mut batch);
            sink.write_samples(&batch);
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

    // Encode each signed sample as two little-endian bytes.
    #[test]
    fn sample_encoding_is_little_endian() {
        assert_eq!(i16::from_le_bytes([0x34, 0x12]), 0x1234);
        assert_eq!((-2i16).to_le_bytes(), [0xFE, 0xFF]);
    }
}
