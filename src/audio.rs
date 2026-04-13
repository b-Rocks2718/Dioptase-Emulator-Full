use std::io::{BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStderr, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::memory::AUDIO_SAMPLE_RATE_HZ;

// Flush in larger chunks because the emulator produces bursty audio writes and
// ffplay is more reliable when it can buffer a modest amount of PCM.
const AUDIO_FLUSH_INTERVAL_SAMPLES: usize = 2048;

struct AudioSinkState {
    writer: BufWriter<ChildStdin>,
    samples_since_flush: usize,
    failed: bool,
    last_player_error: Arc<Mutex<Option<String>>>,
}

// Purpose: serialize guest PCM samples into the host audio player stdin pipe.
// Inputs/outputs: emulator code writes signed 16-bit mono PCM samples; the sink
// writes little-endian bytes to the child process and periodically flushes them.
// Invariants:
// - writes preserve guest sample ordering
// - each sample is emitted as exactly two little-endian bytes
// - once the host pipe fails, subsequent writes become no-ops to avoid log spam
pub struct AudioSink {
    inner: Mutex<AudioSinkState>,
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

    pub fn write_sample(&self, sample: i16) {
        let mut state = self.inner.lock().unwrap();
        Self::write_samples_locked(&mut state, &[sample]);
    }

    // Purpose: serialize a contiguous batch of guest PCM samples with one sink lock.
    // Inputs/outputs: preserves sample ordering and writes each sample as exactly
    // two little-endian bytes to the host player stdin pipe.
    pub fn write_samples(&self, samples: &[i16]) {
        let mut state = self.inner.lock().unwrap();
        Self::write_samples_locked(&mut state, samples);
    }
}

// Purpose: owns the `ffplay` child process used for host audio playback.
// Inputs/outputs: callers clone `shared_sink()` and push guest PCM samples into it.
// Drop behavior closes stdin and waits for the player to exit.
pub struct AudioOutput {
    sink: Option<Arc<AudioSink>>,
    child: Option<Child>,
    stderr_thread: Option<thread::JoinHandle<()>>,
}

impl AudioOutput {
    pub fn start() -> Result<Self, String> {
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
        let sink = Arc::new(AudioSink {
            inner: Mutex::new(AudioSinkState {
                writer: BufWriter::new(stdin),
                samples_since_flush: 0,
                failed: false,
                last_player_error,
            }),
        });

        Ok(AudioOutput {
            sink: Some(sink),
            child: Some(child),
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
        self.sink.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        if let Some(stderr_thread) = self.stderr_thread.take() {
            let _ = stderr_thread.join();
        }
    }
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
