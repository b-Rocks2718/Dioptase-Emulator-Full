// Host window for the VGA device (`--vga`), host keyboard translation into
// the guest PS/2 key-event stream (docs/mem_map.md "PS/2 keyboard"), and host
// mouse translation into the guest PS/2 mouse stream ("PS/2 mouse").

use ::image::{ImageBuffer, Rgba};
use piston_window::*;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU16, Ordering},
    },
};

use crate::memory::*;
use crate::mouse::{MOUSE_BUTTON_LEFT, MOUSE_BUTTON_MIDDLE, MOUSE_BUTTON_RIGHT};

// Scale the host window without changing logical resolution.
const DISPLAY_SCALE: u32 = 2;
const WINDOW_WIDTH: u32 = FRAME_WIDTH * DISPLAY_SCALE;
const WINDOW_HEIGHT: u32 = FRAME_HEIGHT * DISPLAY_SCALE;

// Guest-visible PS/2 keycode contract:
// - bit 8 is the release flag
// - printable keys use their unshifted base-key ASCII identity
// - modifiers keep distinct left/right codes
// - common non-printable navigation/function keys live in a reserved 0x80+
//   range so they do not collide with printable ASCII
const KEY_INSERT: u8 = 0x80;
const KEY_HOME: u8 = 0x81;
const KEY_PAGE_UP: u8 = 0x82;
const KEY_END: u8 = 0x83;
const KEY_PAGE_DOWN: u8 = 0x84;
const KEY_RIGHT: u8 = 0x85;
const KEY_LEFT: u8 = 0x86;
const KEY_DOWN: u8 = 0x87;
const KEY_UP: u8 = 0x88;
const KEY_F1: u8 = 0x90;
const KEY_F2: u8 = 0x91;
const KEY_F3: u8 = 0x92;
const KEY_F4: u8 = 0x93;
const KEY_F5: u8 = 0x94;
const KEY_F6: u8 = 0x95;
const KEY_F7: u8 = 0x96;
const KEY_F8: u8 = 0x97;
const KEY_F9: u8 = 0x98;
const KEY_F10: u8 = 0x99;
const KEY_F11: u8 = 0x9A;
const KEY_F12: u8 = 0x9B;
const KEY_LEFT_CTRL: u8 = 0xE0;
const KEY_LEFT_SHIFT: u8 = 0xE1;
const KEY_LEFT_ALT: u8 = 0xE2;
const KEY_RIGHT_CTRL: u8 = 0xE4;
const KEY_RIGHT_SHIFT: u8 = 0xE5;
const KEY_RIGHT_ALT: u8 = 0xE6;

// Convert a guest keycode into the 16-bit PS/2 MMIO event value.
// Low byte = guest keycode, bit 8 = release when applicable.
fn encode_guest_key_event(code: u8, state: ButtonState) -> u16 {
    match state {
        ButtonState::Press => code as u16,
        ButtonState::Release => 0x0100 | code as u16,
    }
}

// Translate the windowing library's logical key enum into the guest
// keycode contract described above.
// `Some(keycode)` when the key has a stable guest encoding.
// Notes:
// - Printable keys use the unshifted base-key identity.
// - Numpad digits/operators are normalized to the corresponding base keycodes.
// - Keys that the backend reports as `Unknown` are handled separately through
//   text fallback because the backend drops their dedicated logical key.
fn guest_keycode_for_key(key: Key) -> Option<u8> {
    match key {
        Key::Backspace => Some(0x08),
        Key::Tab | Key::NumPadTab => Some(0x09),
        Key::Return | Key::Return2 | Key::NumPadEnter => Some(0x0D),
        Key::Escape => Some(0x1B),
        Key::Space | Key::NumPadSpace => Some(b' '),
        Key::Exclaim => Some(b'1'),
        Key::Quotedbl => Some(b'\''),
        Key::Hash => Some(b'3'),
        Key::Dollar => Some(b'4'),
        Key::Percent => Some(b'5'),
        Key::Ampersand => Some(b'7'),
        Key::LeftParen => Some(b'9'),
        Key::RightParen => Some(b'0'),
        Key::D0 | Key::NumPad0 => Some(b'0'),
        Key::D1 | Key::NumPad1 => Some(b'1'),
        Key::D2 | Key::NumPad2 => Some(b'2'),
        Key::D3 | Key::NumPad3 => Some(b'3'),
        Key::D4 | Key::NumPad4 => Some(b'4'),
        Key::D5 | Key::NumPad5 => Some(b'5'),
        Key::D6 | Key::NumPad6 => Some(b'6'),
        Key::D7 | Key::NumPad7 => Some(b'7'),
        Key::D8 | Key::NumPad8 => Some(b'8'),
        Key::D9 | Key::NumPad9 => Some(b'9'),
        Key::A => Some(b'a'),
        Key::B => Some(b'b'),
        Key::C => Some(b'c'),
        Key::D => Some(b'd'),
        Key::E => Some(b'e'),
        Key::F => Some(b'f'),
        Key::G => Some(b'g'),
        Key::H => Some(b'h'),
        Key::I => Some(b'i'),
        Key::J => Some(b'j'),
        Key::K => Some(b'k'),
        Key::L => Some(b'l'),
        Key::M => Some(b'm'),
        Key::N => Some(b'n'),
        Key::O => Some(b'o'),
        Key::P => Some(b'p'),
        Key::Q => Some(b'q'),
        Key::R => Some(b'r'),
        Key::S => Some(b's'),
        Key::T => Some(b't'),
        Key::U => Some(b'u'),
        Key::V => Some(b'v'),
        Key::W => Some(b'w'),
        Key::X => Some(b'x'),
        Key::Y => Some(b'y'),
        Key::Z => Some(b'z'),
        Key::Colon => Some(b';'),
        Key::Less | Key::NumPadLess => Some(b','),
        Key::Minus | Key::NumPadMinus => Some(b'-'),
        Key::Underscore => Some(b'-'),
        Key::Equals | Key::NumPadEquals | Key::NumPadEqualsAS400 => Some(b'='),
        Key::Greater | Key::NumPadGreater => Some(b'.'),
        Key::Question => Some(b'/'),
        Key::At | Key::NumPadAt => Some(b'2'),
        Key::LeftBracket => Some(b'['),
        Key::RightBracket => Some(b']'),
        Key::Backslash => Some(b'\\'),
        Key::Caret => Some(b'6'),
        Key::Semicolon => Some(b';'),
        Key::Quote => Some(b'\''),
        Key::Backquote => Some(b'`'),
        Key::Comma | Key::NumPadComma => Some(b','),
        Key::Period | Key::NumPadPeriod | Key::NumPadDecimal => Some(b'.'),
        Key::Slash | Key::NumPadDivide => Some(b'/'),
        Key::Asterisk | Key::NumPadMultiply => Some(b'8'),
        Key::Plus | Key::NumPadPlus => Some(b'='),
        Key::Delete | Key::NumPadBackspace => Some(0x7F),
        Key::Insert => Some(KEY_INSERT),
        Key::Home | Key::AcHome => Some(KEY_HOME),
        Key::PageUp => Some(KEY_PAGE_UP),
        Key::End => Some(KEY_END),
        Key::PageDown => Some(KEY_PAGE_DOWN),
        Key::Right => Some(KEY_RIGHT),
        Key::Left => Some(KEY_LEFT),
        Key::Down => Some(KEY_DOWN),
        Key::Up => Some(KEY_UP),
        Key::F1 => Some(KEY_F1),
        Key::F2 => Some(KEY_F2),
        Key::F3 => Some(KEY_F3),
        Key::F4 => Some(KEY_F4),
        Key::F5 => Some(KEY_F5),
        Key::F6 => Some(KEY_F6),
        Key::F7 => Some(KEY_F7),
        Key::F8 => Some(KEY_F8),
        Key::F9 => Some(KEY_F9),
        Key::F10 => Some(KEY_F10),
        Key::F11 => Some(KEY_F11),
        Key::F12 => Some(KEY_F12),
        Key::LCtrl => Some(KEY_LEFT_CTRL),
        Key::LShift => Some(KEY_LEFT_SHIFT),
        Key::LAlt => Some(KEY_LEFT_ALT),
        Key::RCtrl => Some(KEY_RIGHT_CTRL),
        Key::RShift => Some(KEY_RIGHT_SHIFT),
        Key::RAlt => Some(KEY_RIGHT_ALT),
        _ => None,
    }
}

// Recover the unshifted base key identity from the text event that
// follows a backend `Key::Unknown` press.
// Base guest keycode for the originating key when it is representable.
// Notes:
// - This is primarily needed for keys like apostrophe and grave accent because
//   the current `piston_window` backend drops their dedicated logical key.
// - Shifted punctuation maps back to the unshifted base key so releases remain
//   unambiguous.
fn guest_keycode_from_text_char(ch: char) -> Option<u8> {
    match ch {
        'a'..='z' => Some(ch as u8),
        'A'..='Z' => Some(ch.to_ascii_lowercase() as u8),
        '0'..='9' => Some(ch as u8),
        ' ' => Some(b' '),
        '-' | '_' => Some(b'-'),
        '=' | '+' => Some(b'='),
        '[' | '{' => Some(b'['),
        ']' | '}' => Some(b']'),
        '\\' | '|' => Some(b'\\'),
        ';' | ':' => Some(b';'),
        '\'' | '"' => Some(b'\''),
        '`' | '~' => Some(b'`'),
        ',' | '<' => Some(b','),
        '.' | '>' => Some(b'.'),
        '/' | '?' => Some(b'/'),
        '!' => Some(b'1'),
        '@' => Some(b'2'),
        '#' => Some(b'3'),
        '$' => Some(b'4'),
        '%' => Some(b'5'),
        '^' => Some(b'6'),
        '&' => Some(b'7'),
        '*' => Some(b'8'),
        '(' => Some(b'9'),
        ')' => Some(b'0'),
        _ => None,
    }
}

// Translate host keyboard input events into the guest PS/2 key-event
// stream while preserving press/release ordering.
// Invariants:
// - `pending_unknown_press_scancodes` holds host scancodes for unresolved
//   `Key::Unknown` press events waiting for the following text event.
// - `fallback_keycodes_by_scancode` remembers the resolved guest keycode for
//   those keys so the matching release event can emit the same low byte.
// - `pending_text_press_code` remembers a make event emitted directly from a
//   text event until the matching unknown button press arrives, if any.
// - `recent_button_press_code` suppresses the duplicate text event many
//   backends send immediately after a normal printable button press.
struct GuestKeyboardMapper {
    pending_unknown_press_scancodes: VecDeque<i32>,
    fallback_keycodes_by_scancode: HashMap<i32, u8>,
    pending_text_press_code: Option<u8>,
    recent_button_press_code: Option<u8>,
}

impl GuestKeyboardMapper {
    // Create a keyboard mapper with no pending host-key state.
    fn new() -> Self {
        Self {
            pending_unknown_press_scancodes: VecDeque::new(),
            fallback_keycodes_by_scancode: HashMap::new(),
            pending_text_press_code: None,
            recent_button_press_code: None,
        }
    }

    // Clear the stored state.
    fn clear(&mut self) {
        self.pending_unknown_press_scancodes.clear();
        self.fallback_keycodes_by_scancode.clear();
        self.pending_text_press_code = None;
        self.recent_button_press_code = None;
    }

    // Translate a host button event into a guest PS/2 key event.
    fn translate_button(
        &mut self,
        key: Key,
        state: ButtonState,
        scancode: Option<i32>,
    ) -> Option<u16> {
        if key == Key::Unknown {
            match state {
                ButtonState::Press => {
                    // Some backends deliver the text event before the matching
                    // unknown button press. When that happens, the text path
                    // already emitted the guest make event, so only remember the
                    // scancode for the later break event.
                    if let Some(code) = self.pending_text_press_code.take() {
                        if let Some(scancode) = scancode {
                            self.fallback_keycodes_by_scancode.insert(scancode, code);
                        }
                        self.recent_button_press_code = None;
                        return None;
                    }

                    let scancode = scancode?;
                    self.pending_unknown_press_scancodes.push_back(scancode);
                    self.recent_button_press_code = None;
                    None
                }
                ButtonState::Release => {
                    self.pending_text_press_code = None;
                    self.recent_button_press_code = None;

                    let scancode = scancode?;
                    if let Some(code) = self.fallback_keycodes_by_scancode.remove(&scancode) {
                        return Some(encode_guest_key_event(code, ButtonState::Release));
                    }

                    if let Some(index) = self
                        .pending_unknown_press_scancodes
                        .iter()
                        .position(|pending| *pending == scancode)
                    {
                        self.pending_unknown_press_scancodes.remove(index);
                    }
                    None
                }
            }
        } else if let Some(code) = guest_keycode_for_key(key) {
            self.pending_text_press_code = None;
            self.recent_button_press_code = match state {
                ButtonState::Press => Some(code),
                ButtonState::Release => None,
            };
            Some(encode_guest_key_event(code, state))
        } else {
            self.pending_text_press_code = None;
            self.recent_button_press_code = None;
            None
        }
    }

    // Translate a text event when no stable host scancode is available.
    fn translate_text(&mut self, text: &str) -> Option<u16> {
        let mut chars = text.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }

        let code = guest_keycode_from_text_char(ch)?;
        if let Some(scancode) = self.pending_unknown_press_scancodes.pop_front() {
            self.fallback_keycodes_by_scancode.insert(scancode, code);
            self.pending_text_press_code = None;
            self.recent_button_press_code = None;
            return Some(encode_guest_key_event(code, ButtonState::Press));
        }

        // Most backends emit both a logical button press and a text event for
        // printable keys. Ignore the follow-up text when we already emitted the
        // guest make event from the button path.
        if self.recent_button_press_code == Some(code) {
            self.pending_text_press_code = None;
            self.recent_button_press_code = None;
            return None;
        }

        // If the backend exposes only a text event or delivers it before the
        // corresponding unknown button press, still emit the guest make event so
        // interactive text entry keeps working.
        self.pending_text_press_code = Some(code);
        self.recent_button_press_code = None;
        Some(encode_guest_key_event(code, ButtonState::Press))
    }
}

// Button state and accumulated motion to hand to `Memory::push_mouse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GuestMouseEvent {
    buttons: u8,
    dx: i32,
    dy: i32,
    wheel: i32,
}

// Translate host mouse input into guest PS/2 mouse events.
// - Host cursor positions are logical window coordinates, which are the guest
//   640x480 screen scaled by DISPLAY_SCALE; deltas are divided back down.
//   Fractional guest pixels are carried in `motion_remainder` so slow
//   movement still accumulates instead of rounding away.
// - Deltas are computed here rather than taken from the backend's relative
//   motion, because the backend keeps its last position across the cursor
//   leaving the window and would report a jump on re-entry.
// - Host scroll units are treated as wheel detents. Backends report wheel
//   notches as 1.0 per detent with positive y meaning "scroll up", so the
//   sign is inverted for the guest's positive-is-down WHEEL field.
//   Touchpad pixel scrolling arrives through the same host event and will
//   therefore scroll quickly; the backend does not let us tell them apart.
struct GuestMouseMapper {
    buttons: u8,
    last_cursor: Option<[f64; 2]>,
    motion_remainder: [f64; 2],
    wheel_remainder: f64,
}

impl GuestMouseMapper {
    // Create a mapper with every button released and no cursor history.
    fn new() -> Self {
        Self { buttons: 0, last_cursor: None, motion_remainder: [0.0; 2], wheel_remainder: 0.0 }
    }

    // Track a button press or release. Buttons beyond left/right/middle are
    // not part of the guest contract and are ignored.
    fn button(&mut self, button: MouseButton, state: ButtonState) -> Option<GuestMouseEvent> {
        let bit = match button {
            MouseButton::Left => MOUSE_BUTTON_LEFT,
            MouseButton::Right => MOUSE_BUTTON_RIGHT,
            MouseButton::Middle => MOUSE_BUTTON_MIDDLE,
            _ => return None,
        };
        let buttons = match state {
            ButtonState::Press => self.buttons | bit,
            ButtonState::Release => self.buttons & !bit,
        };
        if buttons == self.buttons {
            return None;
        }
        self.buttons = buttons;
        Some(GuestMouseEvent { buttons, dx: 0, dy: 0, wheel: 0 })
    }

    // Convert an absolute host cursor position into guest-pixel motion.
    fn cursor(&mut self, pos: [f64; 2]) -> Option<GuestMouseEvent> {
        let last = self.last_cursor.replace(pos)?;
        let mut whole = [0i32; 2];
        for axis in 0..2 {
            let motion = (pos[axis] - last[axis]) / DISPLAY_SCALE as f64 + self.motion_remainder[axis];
            whole[axis] = motion.trunc() as i32;
            self.motion_remainder[axis] = motion - whole[axis] as f64;
        }
        if whole == [0, 0] {
            return None;
        }
        Some(GuestMouseEvent { buttons: self.buttons, dx: whole[0], dy: whole[1], wheel: 0 })
    }

    // Convert a host scroll delta into guest wheel detents.
    fn scroll(&mut self, delta: [f64; 2]) -> Option<GuestMouseEvent> {
        let wheel = -delta[1] + self.wheel_remainder;
        let whole = wheel.trunc() as i32;
        self.wheel_remainder = wheel - whole as f64;
        if whole == 0 {
            return None;
        }
        Some(GuestMouseEvent { buttons: self.buttons, dx: 0, dy: 0, wheel: whole })
    }

    // The cursor left the window: forget its position so re-entry at a
    // different edge is not reported as motion.
    fn cursor_left(&mut self) {
        self.last_cursor = None;
        self.motion_remainder = [0.0; 2];
    }

    // The window lost focus, so releases may never arrive; report every held
    // button as released so the guest does not see a stuck button.
    fn focus_lost(&mut self) -> Option<GuestMouseEvent> {
        self.cursor_left();
        self.wheel_remainder = 0.0;
        if self.buttons == 0 {
            return None;
        }
        self.buttons = 0;
        Some(GuestMouseEvent { buttons: 0, dx: 0, dy: 0, wheel: 0 })
    }
}

// Expand an 8-bit RGB332 tile color into 4-bit RGB channels (0..=15).
fn expand_rgb332(color: u8) -> (u8, u8, u8) {
    let r3 = (color >> 5) & 0x7;
    let g3 = (color >> 2) & 0x7;
    let b2 = color & 0x3;
    ((r3 << 1) | (r3 >> 2), (g3 << 1) | (g3 >> 2), (b2 << 2) | b2)
}

// Convert a packed 12-bit `0x0BGR` pixel into an opaque host pixel.
fn bgr12_to_rgba(pixel: u16) -> Rgba<u8> {
    let channel = |shift: u16| ((pixel >> shift) & 0xF) as u8 * 16;
    Rgba([channel(0), channel(4), channel(8), 255])
}

// Read a signed 16-bit VGA scroll register.
fn scroll_offset(reg: &AtomicU16) -> i32 {
    i32::from(reg.load(Ordering::SeqCst) as i16)
}

// Write a logical pixel as a `scale` x `scale` block, clipped to the screen.
fn put_scaled(buffer: &mut ImageBuffer<Rgba<u8>, Vec<u8>>, x: u32, y: u32, scale: u32, pixel: Rgba<u8>) {
    for dy in 0..scale {
        for dx in 0..scale {
            let (sx, sy) = (x * scale + dx, y * scale + dy);
            if sx < FRAME_WIDTH && sy < FRAME_HEIGHT {
                buffer.put_pixel(sx, sy, pixel);
            }
        }
    }
}

// Owns the host window and composites the guest VGA layers into it. Runs on
// the main thread; reads VGA state from `memory` while cores keep running,
// so a frame may mix state from slightly different instants (as on hardware).
pub struct Graphics {
    window: PistonWindow,
    buffer: ImageBuffer<Rgba<u8>, Vec<u8>>,
    texture_context: G2dTextureContext,
    texture: G2dTexture,
    memory: Arc<Memory>,
    keyboard_mapper: GuestKeyboardMapper,
    ps2_debug: bool,
    mouse_mapper: GuestMouseMapper,
}

impl Graphics {
    // Open the host window for `memory`'s VGA device.
    pub fn new(memory: Arc<Memory>) -> Graphics {
        let mut window: PistonWindow =
            WindowSettings::new("Dioptase", [WINDOW_WIDTH, WINDOW_HEIGHT])
                .exit_on_esc(true)
                .build()
                .unwrap();
        window.set_max_fps(60);
        window.set_ups(60);

        let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(FRAME_WIDTH, FRAME_HEIGHT);
        let mut texture_context = window.create_texture_context();
        let texture = Texture::from_image(
            &mut texture_context,
            &buffer,
            &TextureSettings::new().filter(Filter::Nearest),
        )
        .unwrap();

        Graphics {
            window,
            buffer,
            texture_context,
            texture,
            memory,
            keyboard_mapper: GuestKeyboardMapper::new(),
            ps2_debug: std::env::var_os("PS2_DEBUG").is_some(),
            mouse_mapper: GuestMouseMapper::new(),
        }
    }

    // Run the window event/render loop until the user closes the window or
    // the emulator sets `stop`.
    pub fn start(&mut self, stop: Arc<AtomicBool>) {
        while let Some(event) = self.window.next() {
            match event {
                Event::Loop(Loop::Update(_)) => {
                    if stop.load(Ordering::SeqCst) {
                        self.window.set_should_close(true);
                    }
                    self.update();
                }
                Event::Loop(Loop::Render(_)) => {
                    self.window.draw_2d(&event, |context, graphics, device| {
                        // Submit the texture upload queued by `update`.
                        self.texture_context.encoder.flush(device);
                        clear([0.0; 4], graphics);
                        let scale = DISPLAY_SCALE as f64;
                        image(&self.texture, context.transform.scale(scale, scale), graphics);
                    });
                }
                Event::Input(
                    Input::Button(ButtonArgs {
                        button: Button::Keyboard(key),
                        state,
                        scancode,
                    }),
                    _,
                ) => {
                    if self.ps2_debug {
                        eprintln!("ps2 host button: key={key:?} state={state:?} scancode={scancode:?}");
                    }
                    let event_code = self.keyboard_mapper.translate_button(key, state, scancode);
                    self.deliver_key(event_code);
                }
                Event::Input(Input::Text(text), _) => {
                    if self.ps2_debug {
                        eprintln!("ps2 host text: {text:?}");
                    }
                    let event_code = self.keyboard_mapper.translate_text(&text);
                    self.deliver_key(event_code);
                }
                Event::Input(
                    Input::Button(ButtonArgs { button: Button::Mouse(button), state, .. }),
                    _,
                ) => {
                    let event = self.mouse_mapper.button(button, state);
                    self.deliver_mouse(event);
                }
                Event::Input(Input::Move(Motion::MouseCursor(pos)), _) => {
                    let event = self.mouse_mapper.cursor(pos);
                    self.deliver_mouse(event);
                }
                Event::Input(Input::Move(Motion::MouseScroll(delta)), _) => {
                    let event = self.mouse_mapper.scroll(delta);
                    self.deliver_mouse(event);
                }
                Event::Input(Input::Cursor(false), _) => self.mouse_mapper.cursor_left(),
                Event::Input(Input::Focus(false), _) => {
                    self.keyboard_mapper.clear();
                    let event = self.mouse_mapper.focus_lost();
                    self.deliver_mouse(event);
                }
                _ => {}
            }
        }
    }

    // Queue a translated key event for the guest.
    fn deliver_key(&self, event_code: Option<u16>) {
        if let Some(event_code) = event_code {
            if self.ps2_debug {
                eprintln!("ps2 guest event: 0x{event_code:04X}");
            }
            self.memory.push_input(event_code);
        }
    }

    // Queue a translated mouse event for the guest.
    fn deliver_mouse(&self, event: Option<GuestMouseEvent>) {
        if let Some(event) = event {
            if self.ps2_debug {
                eprintln!("ps2 guest mouse: {event:?}");
            }
            self.memory.push_mouse(event.buttons, event.dx, event.dy, event.wheel);
        }
    }

    // Draw the pixel layer (background). Pixel scale has an implicit +1 so
    // n = 0 doubles 320x240 to fill 640x480.
    fn draw_pixel_layer(&mut self) {
        let vga = self.memory.vga();
        let scale = 1 << (u32::from(vga.pixel_scale.load(Ordering::SeqCst)) + 1);
        let (scroll_x, scroll_y) = (scroll_offset(&vga.pixel_hscroll), scroll_offset(&vga.pixel_vscroll));
        let fb = vga.pixel_frame_buffer.read().unwrap();
        for y in 0..PIXEL_FRAME_HEIGHT {
            for x in 0..PIXEL_FRAME_WIDTH {
                let idx = 2 * (x + y * PIXEL_FRAME_WIDTH) as usize;
                let pixel = bgr12_to_rgba(u16::from_le_bytes([fb[idx], fb[idx + 1]]));
                // Signed scroll wraps with Euclidean modulo so large negative
                // offsets keep wrapping correctly.
                let fx = (x as i32 + scroll_x).rem_euclid(FRAME_WIDTH as i32) as u32;
                let fy = (y as i32 + scroll_y).rem_euclid(FRAME_HEIGHT as i32) as u32;
                put_scaled(&mut self.buffer, fx, fy, scale, pixel);
            }
        }
    }

    // Draw the tile layer over the pixel layer. 0xFXXX tile pixels are
    // transparent and 0xCXXX pixels take the entry's RGB332 tile color.
    fn draw_tile_layer(&mut self) {
        let vga = self.memory.vga();
        let scale = 1 << u32::from(vga.tile_scale.load(Ordering::SeqCst));
        let (scroll_x, scroll_y) = (scroll_offset(&vga.tile_hscroll), scroll_offset(&vga.tile_vscroll));
        let fb = vga.tile_frame_buffer.read().unwrap();
        let tile_map = vga.tile_map.read().unwrap();
        for ty in 0..TILE_FB_HEIGHT_TILES {
            for tx in 0..TILE_FB_WIDTH_TILES {
                let entry = 2 * (tx + ty * TILE_FB_WIDTH_TILES) as usize;
                let (tile_index, tile_color) = (fb[entry] as usize, fb[entry + 1]);
                let tile = &tile_map[tile_index * TILE_SIZE as usize..][..TILE_SIZE as usize];
                for py in 0..TILE_WIDTH {
                    for px in 0..TILE_WIDTH {
                        let addr = (2 * (px + py * TILE_WIDTH)) as usize;
                        let raw = u16::from_le_bytes([tile[addr], tile[addr + 1]]);
                        let pixel = match raw >> 12 {
                            0xF => continue,
                            0xC => {
                                let (r, g, b) = expand_rgb332(tile_color);
                                Rgba([r * 16, g * 16, b * 16, 255])
                            }
                            _ => bgr12_to_rgba(raw),
                        };
                        let raw_x = (tx * TILE_WIDTH + px) as i32 + scroll_x;
                        let raw_y = (ty * TILE_WIDTH + py) as i32 + scroll_y;
                        let fx = raw_x.rem_euclid(FRAME_WIDTH as i32) as u32;
                        let fy = raw_y.rem_euclid(FRAME_HEIGHT as i32) as u32;
                        put_scaled(&mut self.buffer, fx, fy, scale, pixel);
                    }
                }
            }
        }
    }

    // Draw the sprites on top; later sprites overlap earlier ones. Sprite
    // coordinates are signed and pixels left of / above the screen are clipped.
    fn draw_sprites(&mut self) {
        let vga = self.memory.vga();
        let sprites = vga.sprite_pixels.read().unwrap();
        for index in 0..SPRITE_COUNT {
            let scale = 1 << u32::from(vga.sprite_scales[index].load(Ordering::SeqCst));
            let coords = vga.sprite_coords[index].load(Ordering::SeqCst);
            let (sprite_x, sprite_y) = (i32::from(coords as u16 as i16), i32::from((coords >> 16) as u16 as i16));
            let pixels = &sprites[index * SPRITE_SIZE as usize..][..SPRITE_SIZE as usize];
            for py in 0..SPRITE_WIDTH {
                for px in 0..SPRITE_WIDTH {
                    let addr = (2 * (px + py * SPRITE_WIDTH)) as usize;
                    let raw = u16::from_le_bytes([pixels[addr], pixels[addr + 1]]);
                    let (x, y) = (sprite_x + px as i32, sprite_y + py as i32);
                    if raw >> 12 == 0xF || x < 0 || y < 0 {
                        continue;
                    }
                    put_scaled(&mut self.buffer, x as u32, y as u32, scale, bgr12_to_rgba(raw));
                }
            }
        }
    }

    // Composite one frame, advance the frame counter, and raise vblank.
    fn update(&mut self) {
        let vga = self.memory.vga();
        // VGA status: 0 = drawing, 3 = idle (vblank).
        vga.status.store(0, Ordering::SeqCst);
        self.draw_pixel_layer();
        self.draw_tile_layer();
        self.draw_sprites();
        let vga = self.memory.vga();
        vga.frame.fetch_add(1, Ordering::SeqCst);
        self.texture
            .update(&mut self.texture_context, &self.buffer)
            .unwrap();
        vga.status.store(3, Ordering::SeqCst);
        self.memory.raise_pending_interrupt(VGA_INTERRUPT_BIT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Preserve the base printable key identity for an unshifted key event.
    #[test]
    fn guest_keycode_preserves_unshifted_printable_identity() {
        assert_eq!(guest_keycode_for_key(Key::A), Some(b'a'));
        assert_eq!(guest_keycode_for_key(Key::D1), Some(b'1'));
        assert_eq!(guest_keycode_for_key(Key::Minus), Some(b'-'));
        assert_eq!(guest_keycode_for_key(Key::LShift), Some(KEY_LEFT_SHIFT));
        assert_eq!(guest_keycode_for_key(Key::RShift), Some(KEY_RIGHT_SHIFT));
        assert_eq!(guest_keycode_for_key(Key::Left), Some(KEY_LEFT));
        assert_eq!(guest_keycode_for_key(Key::F12), Some(KEY_F12));
    }

    // Normalize shifted symbols to the base key used for make/break identity.
    #[test]
    fn guest_keycode_normalizes_shifted_symbol_variants_to_base_keys() {
        assert_eq!(guest_keycode_for_key(Key::Exclaim), Some(b'1'));
        assert_eq!(guest_keycode_for_key(Key::At), Some(b'2'));
        assert_eq!(guest_keycode_for_key(Key::Hash), Some(b'3'));
        assert_eq!(guest_keycode_for_key(Key::Question), Some(b'/'));
        assert_eq!(guest_keycode_for_key(Key::Asterisk), Some(b'8'));
        assert_eq!(guest_keycode_for_key(Key::Plus), Some(b'='));
        assert_eq!(guest_keycode_for_key(Key::Colon), Some(b';'));
        assert_eq!(guest_keycode_for_key(Key::Underscore), Some(b'-'));
        assert_eq!(guest_keycode_for_key(Key::Quotedbl), Some(b'\''));
        assert_eq!(guest_keycode_for_key(Key::Less), Some(b','));
        assert_eq!(guest_keycode_for_key(Key::Greater), Some(b'.'));
        assert_eq!(guest_keycode_for_key(Key::Caret), Some(b'6'));
    }

    // Recover a base key from shifted punctuation supplied only as text.
    #[test]
    fn text_fallback_recovers_base_key_from_shifted_punctuation() {
        assert_eq!(guest_keycode_from_text_char('!'), Some(b'1'));
        assert_eq!(guest_keycode_from_text_char('"'), Some(b'\''));
        assert_eq!(guest_keycode_from_text_char('~'), Some(b'`'));
        assert_eq!(guest_keycode_from_text_char('|'), Some(b'\\'));
    }

    // Pair make and break events when an unknown key is identified through text.
    #[test]
    fn unknown_key_uses_text_fallback_for_make_and_break() {
        let mut mapper = GuestKeyboardMapper::new();

        assert_eq!(
            mapper.translate_button(Key::Unknown, ButtonState::Press, Some(41)),
            None
        );
        assert_eq!(mapper.translate_text("\""), Some(b'\'' as u16));
        assert_eq!(
            mapper.translate_button(Key::Unknown, ButtonState::Release, Some(41)),
            Some(0x0100 | b'\'' as u16)
        );
    }

    // Preserve the eventual break event when text arrives before an unknown key press.
    #[test]
    fn text_before_unknown_key_press_still_preserves_break_event() {
        let mut mapper = GuestKeyboardMapper::new();

        assert_eq!(mapper.translate_text("!"), Some(b'1' as u16));
        assert_eq!(
            mapper.translate_button(Key::Unknown, ButtonState::Press, Some(2)),
            None
        );
        assert_eq!(
            mapper.translate_button(Key::Unknown, ButtonState::Release, Some(2)),
            Some(0x0100 | b'1' as u16)
        );
    }

    // Ignore text that duplicates an already decoded physical key press.
    #[test]
    fn text_after_known_button_press_is_ignored_as_duplicate() {
        let mut mapper = GuestKeyboardMapper::new();

        assert_eq!(
            mapper.translate_button(Key::D3, ButtonState::Press, Some(4)),
            Some(b'3' as u16)
        );
        assert_eq!(mapper.translate_text("#"), None);
        assert_eq!(
            mapper.translate_button(Key::D3, ButtonState::Release, Some(4)),
            Some(0x0100 | b'3' as u16)
        );
    }

    // Emit a text-derived make event even when no physical scancode is available.
    #[test]
    fn unknown_key_without_scancode_can_still_emit_text_make_event() {
        let mut mapper = GuestKeyboardMapper::new();

        assert_eq!(
            mapper.translate_button(Key::Unknown, ButtonState::Press, None),
            None
        );
        assert_eq!(mapper.translate_text("?"), Some(b'/' as u16));
        assert_eq!(
            mapper.translate_button(Key::Unknown, ButtonState::Release, None),
            None
        );
    }

    // Window coordinates are DISPLAY_SCALE times guest pixels; sub-pixel
    // motion must accumulate rather than round away.
    #[test]
    fn mouse_motion_scales_to_guest_pixels_and_keeps_remainder() {
        let mut mapper = GuestMouseMapper::new();
        assert_eq!(mapper.cursor([100.0, 100.0]), None);
        let scale = DISPLAY_SCALE as f64;
        assert_eq!(
            mapper.cursor([100.0 + 4.0 * scale, 100.0 - 2.0 * scale]),
            Some(GuestMouseEvent { buttons: 0, dx: 4, dy: -2, wheel: 0 })
        );
        let half = 0.5 * scale;
        let base = [100.0 + 4.0 * scale, 100.0 - 2.0 * scale];
        assert_eq!(mapper.cursor([base[0] + half, base[1]]), None);
        assert_eq!(
            mapper.cursor([base[0] + 2.0 * half, base[1]]),
            Some(GuestMouseEvent { buttons: 0, dx: 1, dy: 0, wheel: 0 })
        );
    }

    // Re-entering the window at another edge must not look like motion.
    #[test]
    fn mouse_cursor_leaving_window_resets_origin() {
        let mut mapper = GuestMouseMapper::new();
        mapper.cursor([0.0, 0.0]);
        mapper.cursor_left();
        assert_eq!(mapper.cursor([1000.0, 900.0]), None);
    }

    // Motion and scroll carry the held buttons; repeated presses are not
    // new events.
    #[test]
    fn mouse_buttons_track_state_and_ride_along_with_motion() {
        let mut mapper = GuestMouseMapper::new();
        assert_eq!(
            mapper.button(MouseButton::Left, ButtonState::Press),
            Some(GuestMouseEvent { buttons: MOUSE_BUTTON_LEFT, dx: 0, dy: 0, wheel: 0 })
        );
        assert_eq!(mapper.button(MouseButton::Left, ButtonState::Press), None);
        assert_eq!(mapper.button(MouseButton::X1, ButtonState::Press), None);
        assert_eq!(
            mapper.button(MouseButton::Middle, ButtonState::Press),
            Some(GuestMouseEvent { buttons: MOUSE_BUTTON_LEFT | MOUSE_BUTTON_MIDDLE, dx: 0, dy: 0, wheel: 0 })
        );
        assert_eq!(
            mapper.scroll([0.0, 1.0]),
            Some(GuestMouseEvent { buttons: MOUSE_BUTTON_LEFT | MOUSE_BUTTON_MIDDLE, dx: 0, dy: 0, wheel: -1 })
        );
        assert_eq!(
            mapper.button(MouseButton::Left, ButtonState::Release),
            Some(GuestMouseEvent { buttons: MOUSE_BUTTON_MIDDLE, dx: 0, dy: 0, wheel: 0 })
        );
    }

    // Host "scroll up" (positive y) is guest WHEEL negative; fractional
    // deltas accumulate into whole detents.
    #[test]
    fn mouse_scroll_inverts_sign_and_accumulates_fractions() {
        let mut mapper = GuestMouseMapper::new();
        assert_eq!(mapper.scroll([0.0, -0.5]), None);
        assert_eq!(mapper.scroll([0.0, -0.5]), Some(GuestMouseEvent { buttons: 0, dx: 0, dy: 0, wheel: 1 }));
        assert_eq!(mapper.scroll([3.0, 0.0]), None);
    }

    // Losing focus can swallow the release, so held buttons are released.
    #[test]
    fn mouse_focus_loss_releases_held_buttons() {
        let mut mapper = GuestMouseMapper::new();
        assert_eq!(mapper.focus_lost(), None);
        mapper.button(MouseButton::Right, ButtonState::Press);
        assert_eq!(mapper.focus_lost(), Some(GuestMouseEvent { buttons: 0, dx: 0, dy: 0, wheel: 0 }));
        assert_eq!(mapper.focus_lost(), None);
    }
}
