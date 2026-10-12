// Headless VGA display for automated testing of interactive guest programs
// (`--headless`). It stands in for the host window from `graphics.rs`:
//
// - Frames advance on wall-clock time at the same 60 Hz rate as the window,
//   so the guest-visible VGA status register, frame counter, and vblank
//   interrupt behave as they do with `--vga`.
// - Keyboard and mouse input come from a script (`--input-script`) instead
//   of the host, injected at fixed frame numbers through the same
//   `Memory::push_input` / `Memory::push_mouse` paths the window uses.
// - Frames are composited only when a screenshot is due and saved as PNG.
// - `--audio-capture` records the audio device to a raw s16le file, consuming
//   it on wall-clock time exactly like the default `--audio-fast` playback.
//
// Script format: one event per line, `#` starts a comment.
//
//   <frame> key <name> [tap|down|up]   key event (default tap: down, then up
//                                      two frames later)
//   <frame> type <text>                tap each character, 4 frames apart
//   <frame> mouse <dx> <dy> [wheel]    relative motion, current buttons held
//   <frame> button <left|right|middle> <down|up>
//   <frame> shot [name]                save a screenshot (default frame number)
//   <frame> exit                       stop the run
//
// Key names are single printable characters (lowercase identity, as the PS/2
// contract in docs/mem_map.md requires) or: enter, space, esc, backspace, tab,
// delete, up, down, left, right, lshift, rshift, lctrl, rctrl, lalt, ralt,
// f1..f12, or a 0x-prefixed guest keycode.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::graphics::Compositor;
use crate::memory::{AUDIO_SAMPLE_RATE_HZ, Memory, VGA_INTERRUPT_BIT};
use crate::mouse::{MOUSE_BUTTON_LEFT, MOUSE_BUTTON_MIDDLE, MOUSE_BUTTON_RIGHT};

// Matches the window's `set_ups(60)` update rate.
const FRAME_DURATION: Duration = Duration::from_nanos(1_000_000_000 / 60);
// Frames between the press and release of a scripted key tap.
const TAP_HOLD_FRAMES: u32 = 2;
// Frames between successive characters of a `type` command.
const TYPE_STRIDE_FRAMES: u32 = 4;
// PS/2 release flag (docs/mem_map.md "PS/2 keyboard input stream").
const KEY_RELEASE: u16 = 0x0100;
// Audio capture consumes the device in 10 ms batches, like `--audio-fast`.
const CAPTURE_BATCH_SAMPLES: usize = (AUDIO_SAMPLE_RATE_HZ as usize) / 100;

// One scripted action, applied at the start of its frame.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Key(u16),
    Mouse { dx: i32, dy: i32, wheel: i32 },
    Button { bit: u8, down: bool },
    Shot(Option<String>),
    Exit,
}

// Parsed headless options from the command line.
#[derive(Default)]
pub struct HeadlessOptions {
    pub enabled: bool,
    pub shot_dir: Option<String>,
    pub shot_every: u32,
    pub script_path: Option<String>,
    pub exit_frame: u32,
    pub audio_capture: Option<String>,
}

// Translate a script key name into a guest keycode.
fn keycode(name: &str) -> Result<u8, String> {
    let named = match name {
        "enter" => Some(0x0D),
        "space" => Some(b' '),
        "esc" => Some(0x1B),
        "backspace" => Some(0x08),
        "tab" => Some(0x09),
        "delete" => Some(0x7F),
        "right" => Some(0x85),
        "left" => Some(0x86),
        "down" => Some(0x87),
        "up" => Some(0x88),
        "lctrl" => Some(0xE0),
        "lshift" => Some(0xE1),
        "lalt" => Some(0xE2),
        "rctrl" => Some(0xE4),
        "rshift" => Some(0xE5),
        "ralt" => Some(0xE6),
        _ => None,
    };
    if let Some(code) = named {
        return Ok(code);
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=12).contains(&n)
    {
        return Ok(0x90 + n - 1);
    }
    if let Some(hex) = name.strip_prefix("0x") {
        return u8::from_str_radix(hex, 16).map_err(|_| format!("bad keycode '{}'", name));
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Ok(c.to_ascii_lowercase() as u8),
        _ => Err(format!("unknown key name '{}'", name)),
    }
}

// Parse a script into actions keyed by frame. Errors name the line.
fn parse_script(text: &str) -> Result<BTreeMap<u32, Vec<Action>>, String> {
    let mut actions: BTreeMap<u32, Vec<Action>> = BTreeMap::new();
    let mut push = |frame: u32, action: Action| actions.entry(frame).or_default().push(action);
    for (index, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let err = |msg: String| format!("input script line {}: {} ('{}')", index + 1, msg, raw.trim());
        let mut words = line.split_whitespace();
        let frame: u32 = words
            .next()
            .unwrap()
            .parse()
            .map_err(|_| err("expected a frame number first".to_string()))?;
        let command = words.next().ok_or_else(|| err("missing command".to_string()))?;
        let args: Vec<&str> = words.collect();
        let int = |s: &str| s.parse::<i32>().map_err(|_| err(format!("expected an integer, got '{}'", s)));
        match command {
            "key" => {
                let name = args.first().ok_or_else(|| err("missing key name".to_string()))?;
                let code = keycode(name).map_err(err)? as u16;
                match args.get(1).copied().unwrap_or("tap") {
                    "down" => push(frame, Action::Key(code)),
                    "up" => push(frame, Action::Key(code | KEY_RELEASE)),
                    "tap" => {
                        push(frame, Action::Key(code));
                        push(frame + TAP_HOLD_FRAMES, Action::Key(code | KEY_RELEASE));
                    }
                    other => return Err(err(format!("expected tap, down, or up, got '{}'", other))),
                }
            }
            "type" => {
                // Re-split the original text so repeated spaces survive.
                let text = line.splitn(3, char::is_whitespace).nth(2).unwrap_or("");
                for (i, c) in text.chars().enumerate() {
                    let code = keycode(&c.to_string()).or_else(|_| {
                        if c == ' ' { Ok(b' ') } else { Err(format!("cannot type '{}'", c)) }
                    });
                    let code = code.map_err(err)? as u16;
                    let at = frame + i as u32 * TYPE_STRIDE_FRAMES;
                    push(at, Action::Key(code));
                    push(at + TAP_HOLD_FRAMES, Action::Key(code | KEY_RELEASE));
                }
            }
            "mouse" => {
                if args.len() < 2 {
                    return Err(err("expected dx dy [wheel]".to_string()));
                }
                let wheel = args.get(2).map(|s| int(s)).transpose()?.unwrap_or(0);
                push(frame, Action::Mouse { dx: int(args[0])?, dy: int(args[1])?, wheel });
            }
            "button" => {
                let bit = match args.first().copied() {
                    Some("left") => MOUSE_BUTTON_LEFT,
                    Some("right") => MOUSE_BUTTON_RIGHT,
                    Some("middle") => MOUSE_BUTTON_MIDDLE,
                    _ => return Err(err("expected left, right, or middle".to_string())),
                };
                let down = match args.get(1).copied() {
                    Some("down") => true,
                    Some("up") => false,
                    _ => return Err(err("expected down or up".to_string())),
                };
                push(frame, Action::Button { bit, down });
            }
            "shot" => push(frame, Action::Shot(args.first().map(|s| s.to_string()))),
            "exit" => push(frame, Action::Exit),
            other => return Err(err(format!("unknown command '{}'", other))),
        }
    }
    Ok(actions)
}

// Record the audio device to `path` as raw s16le mono PCM at 25 kHz, consuming
// it on wall-clock time like `--audio-fast`. Runs until `stop` is set.
pub fn spawn_audio_capture(
    memory: Arc<Memory>,
    path: &str,
    stop: Arc<AtomicBool>,
) -> Result<thread::JoinHandle<()>, String> {
    let file = File::create(path).map_err(|e| format!("Failed to create audio capture {}: {}", path, e))?;
    let mut writer = BufWriter::new(file);
    let batch_duration = Duration::from_millis(10);
    Ok(thread::spawn(move || {
        let start = Instant::now();
        let mut batches: u64 = 0;
        let mut batch = Vec::with_capacity(CAPTURE_BATCH_SAMPLES);
        while !stop.load(Ordering::SeqCst) {
            let due = start + batch_duration * (batches as u32 + 1);
            let now = Instant::now();
            if now < due {
                thread::sleep(due - now);
                continue;
            }
            memory.consume_audio_wallclock_samples(CAPTURE_BATCH_SAMPLES, &mut batch);
            if batch.is_empty() {
                // Device disabled: record silence so the file tracks wall time.
                batch.resize(CAPTURE_BATCH_SAMPLES, 0);
            }
            for sample in &batch {
                let _ = writer.write_all(&sample.to_le_bytes());
            }
            batches += 1;
        }
        let _ = writer.flush();
    }))
}

// Drive the headless display on the calling thread until the run stops.
// Returns the frame number reached.
pub fn run_headless(memory: Arc<Memory>, stop: Arc<AtomicBool>, options: &HeadlessOptions) -> Result<u32, String> {
    let mut actions = match &options.script_path {
        Some(path) => {
            let text = fs::read_to_string(path).map_err(|e| format!("Failed to read input script {}: {}", path, e))?;
            parse_script(&text)?
        }
        None => BTreeMap::new(),
    };
    if options.exit_frame != 0 {
        actions.entry(options.exit_frame).or_default().push(Action::Exit);
    }
    let shot_dir = PathBuf::from(options.shot_dir.clone().unwrap_or_else(|| ".".to_string()));
    if options.shot_every != 0 || actions.values().flatten().any(|a| matches!(a, Action::Shot(_))) {
        fs::create_dir_all(&shot_dir).map_err(|e| format!("Failed to create {}: {}", shot_dir.display(), e))?;
    }

    let mut compositor = Compositor::new(Arc::clone(&memory));
    let mut buttons: u8 = 0;
    let start = Instant::now();
    let mut frame: u32 = 0;
    while !stop.load(Ordering::SeqCst) {
        let due = start + FRAME_DURATION * (frame + 1);
        let now = Instant::now();
        if now < due {
            thread::sleep(due - now);
        }
        frame += 1;

        let mut shots: Vec<String> = Vec::new();
        if options.shot_every != 0 && frame.is_multiple_of(options.shot_every) {
            shots.push(format!("frame{:06}", frame));
        }
        let mut exit = false;
        for action in actions.remove(&frame).unwrap_or_default() {
            match action {
                Action::Key(code) => memory.push_input(code),
                Action::Mouse { dx, dy, wheel } => memory.push_mouse(buttons, dx, dy, wheel),
                Action::Button { bit, down } => {
                    buttons = if down { buttons | bit } else { buttons & !bit };
                    memory.push_mouse(buttons, 0, 0, 0);
                }
                Action::Shot(name) => shots.push(name.unwrap_or_else(|| format!("frame{:06}", frame))),
                Action::Exit => exit = true,
            }
        }

        let vga = memory.vga();
        vga.status.store(0, Ordering::SeqCst);
        if !shots.is_empty() {
            compositor.compose();
            for name in shots {
                let path = shot_dir.join(format!("{}.png", name));
                compositor
                    .image()
                    .save(&path)
                    .map_err(|e| format!("Failed to save screenshot {}: {}", path.display(), e))?;
                eprintln!("headless: frame {} -> {}", frame, path.display());
            }
        }
        vga.frame.fetch_add(1, Ordering::SeqCst);
        vga.status.store(3, Ordering::SeqCst);
        memory.raise_pending_interrupt(VGA_INTERRUPT_BIT);

        if exit {
            eprintln!("headless: stopping at frame {}", frame);
            stop.store(true, Ordering::SeqCst);
        }
    }
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tap expands into a press and a delayed release of the same keycode.
    #[test]
    fn key_tap_expands_to_press_and_release() {
        let actions = parse_script("10 key w\n").unwrap();
        assert_eq!(actions[&10], vec![Action::Key(b'w' as u16)]);
        assert_eq!(actions[&(10 + TAP_HOLD_FRAMES)], vec![Action::Key(KEY_RELEASE | b'w' as u16)]);
    }

    // Typed text becomes one tap per character, spaced TYPE_STRIDE_FRAMES apart.
    #[test]
    fn type_spaces_characters() {
        let actions = parse_script("5 type ab\n").unwrap();
        assert_eq!(actions[&5], vec![Action::Key(b'a' as u16)]);
        assert_eq!(actions[&(5 + TYPE_STRIDE_FRAMES)], vec![Action::Key(b'b' as u16)]);
    }

    // Named keys use the guest keycode contract.
    #[test]
    fn named_keys_map_to_guest_codes() {
        assert_eq!(keycode("enter").unwrap(), 0x0D);
        assert_eq!(keycode("up").unwrap(), 0x88);
        assert_eq!(keycode("f12").unwrap(), 0x9B);
        assert_eq!(keycode("W").unwrap(), b'w');
        assert!(keycode("bogus").is_err());
    }

    // Errors identify the offending line.
    #[test]
    fn script_errors_name_the_line() {
        let err = parse_script("1 key a\nx key b\n").unwrap_err();
        assert!(err.contains("line 2"), "{}", err);
    }
}
