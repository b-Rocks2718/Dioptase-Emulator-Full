use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::memory::AUDIO_SAMPLE_RATE_HZ;

// Flush in larger chunks because the emulator produces bursty audio writes and
// ffplay is more reliable when it can buffer a modest amount of PCM.
const AUDIO_FLUSH_INTERVAL_SAMPLES: usize = 2048;
const AUDIO_BUFFERED_BATCH_QUEUE: usize = 4;

struct AudioSinkState {
    writer: BufWriter<ChildStdin>,
    samples_since_flush: usize,
    failed: bool,
    last_player_error: Arc<Mutex<Option<String>>>,
}

struct BufferedAudioSink {
    sender: SyncSender<Vec<i16>>,
    failed: Arc<AtomicBool>,
    last_player_error: Arc<Mutex<Option<String>>>,
}

enum AudioSinkInner {
    Direct(Mutex<AudioSinkState>),
    Buffered(BufferedAudioSink),
}

// Purpose: serialize mixed guest audio samples into the host player stdin pipe.
// Inputs/outputs: emulator code writes signed 16-bit mono samples; the sink
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

    pub fn write_sample(&self, sample: i16) {
        self.write_samples(&[sample]);
    }

    // Purpose: serialize a contiguous batch of guest audio samples with one sink lock.
    // Inputs/outputs: preserves sample ordering and writes each sample as exactly
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

// Purpose: owns the `ffplay` child process used for host audio playback.
// Inputs/outputs: callers clone `shared_sink()` and push guest PCM samples into it.
// Drop behavior closes stdin and waits for the player to exit.
pub struct AudioOutput {
    sink: Option<Arc<AudioSink>>,
    child: Option<Child>,
    writer_thread: Option<thread::JoinHandle<()>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
}

impl AudioOutput {
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

    pub fn shared_sink(&self) -> Arc<AudioSink> {
        Arc::clone(
            self.sink
                .as_ref()
                .expect("audio sink must exist while output is alive"),
        )
    }
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        let buffered = self.writer_thread.is_some();
        self.sink.take();
        if buffered {
            if let Some(child) = self.child.as_mut() {
                let _ = child.kill();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffplay_args_match_guest_audio_format() {
        let args = ffplay_args();
        assert!(args.contains(&"s16le".to_string()));
        assert!(args.contains(&AUDIO_SAMPLE_RATE_HZ.to_string()));
        assert!(args.contains(&"1".to_string()));
        assert!(args.contains(&"pipe:0".to_string()));
    }

    #[test]
    fn sample_encoding_is_little_endian() {
        assert_eq!(i16::from_le_bytes([0x34, 0x12]), 0x1234);
        assert_eq!((-2i16).to_le_bytes(), [0xFE, 0xFF]);
    }
}
