use ::image::{ImageBuffer, Rgba};
use piston_window::*;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
};

use crate::memory::*;

const SCREEN_WIDTH: u32 = 640;
const SCREEN_HEIGHT: u32 = 480;
// Purpose: scale the host window without changing logical resolution.
// Invariants: buffer remains FRAME_WIDTH x FRAME_HEIGHT.
const DISPLAY_SCALE: u32 = 2;
const WINDOW_WIDTH: u32 = SCREEN_WIDTH * DISPLAY_SCALE;
const WINDOW_HEIGHT: u32 = SCREEN_HEIGHT * DISPLAY_SCALE;

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

// Purpose: convert a guest keycode into the 16-bit PS/2 MMIO event value.
// Inputs: base guest keycode plus press/release state.
// Outputs: low byte = guest keycode, bit 8 = release when applicable.
fn encode_guest_key_event(code: u8, state: ButtonState) -> u16 {
    match state {
        ButtonState::Press => code as u16,
        ButtonState::Release => 0x0100 | code as u16,
    }
}

// Purpose: translate the windowing library's logical key enum into the guest
// keycode contract described above.
// Inputs: `piston_window::Key`.
// Outputs: `Some(keycode)` when the key has a stable guest encoding.
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
        Key::Minus | Key::NumPadMinus => Some(b'-'),
        Key::Equals | Key::NumPadEquals | Key::NumPadEqualsAS400 => Some(b'='),
        Key::LeftBracket => Some(b'['),
        Key::RightBracket => Some(b']'),
        Key::Backslash => Some(b'\\'),
        Key::Semicolon => Some(b';'),
        Key::Quote => Some(b'\''),
        Key::Backquote => Some(b'`'),
        Key::Comma | Key::NumPadComma => Some(b','),
        Key::Period | Key::NumPadPeriod | Key::NumPadDecimal => Some(b'.'),
        Key::Slash | Key::NumPadDivide => Some(b'/'),
        Key::Asterisk | Key::NumPadMultiply => Some(b'*'),
        Key::Plus | Key::NumPadPlus => Some(b'+'),
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

// Purpose: recover the unshifted base key identity from the text event that
// follows a backend `Key::Unknown` press.
// Inputs: composed host character.
// Outputs: base guest keycode for the originating key when it is representable.
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

// Purpose: translate host keyboard input events into the guest PS/2 key-event
// stream while preserving press/release ordering.
// Invariants:
// - `pending_unknown_press_scancodes` holds host scancodes for unresolved
//   `Key::Unknown` press events waiting for the following text event.
// - `fallback_keycodes_by_scancode` remembers the resolved guest keycode for
//   those keys so the matching release event can emit the same low byte.
struct GuestKeyboardMapper {
    pending_unknown_press_scancodes: VecDeque<i32>,
    fallback_keycodes_by_scancode: HashMap<i32, u8>,
}

impl GuestKeyboardMapper {
    fn new() -> Self {
        Self {
            pending_unknown_press_scancodes: VecDeque::new(),
            fallback_keycodes_by_scancode: HashMap::new(),
        }
    }

    fn clear(&mut self) {
        self.pending_unknown_press_scancodes.clear();
        self.fallback_keycodes_by_scancode.clear();
    }

    fn translate_button(
        &mut self,
        key: Key,
        state: ButtonState,
        scancode: Option<i32>,
    ) -> Option<u16> {
        if let Some(code) = guest_keycode_for_key(key) {
            return Some(encode_guest_key_event(code, state));
        }

        if key != Key::Unknown {
            return None;
        }

        let scancode = scancode?;
        match state {
            ButtonState::Press => {
                self.pending_unknown_press_scancodes.push_back(scancode);
                None
            }
            ButtonState::Release => {
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
    }

    fn translate_text(&mut self, text: &str) -> Option<u16> {
        let scancode = self.pending_unknown_press_scancodes.pop_front()?;
        let mut chars = text.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }

        let code = guest_keycode_from_text_char(ch)?;
        self.fallback_keycodes_by_scancode.insert(scancode, code);
        Some(encode_guest_key_event(code, ButtonState::Press))
    }
}

// Purpose: expand an 8-bit sprite/tile color into 4-bit RGB channels.
// Inputs: 8-bit color in RGB332 format.
// Outputs: (r4, g4, b4) in 0..=15.
fn expand_rgb332(color: u8) -> (u8, u8, u8) {
    let r3 = (color >> 5) & 0x7;
    let g3 = (color >> 2) & 0x7;
    let b2 = color & 0x3;
    let r4 = (r3 << 1) | (r3 >> 2);
    let g4 = (g3 << 1) | (g3 >> 2);
    let b4 = (b2 << 2) | b2;
    (r4, g4, b4)
}

// Purpose: decode a signed 16-bit scroll offset from two MMIO bytes.
// Inputs: (low, high) bytes in little-endian order.
// Outputs: signed pixel offset.
fn decode_scroll_offset(pair: (u8, u8)) -> i32 {
    i32::from(i16::from_le_bytes([pair.0, pair.1]))
}

pub struct Graphics {
    window: PistonWindow,
    buffer: ImageBuffer<Rgba<u8>, Vec<u8>>,
    texture: G2dTexture,
    pixel_frame_buffer: Arc<RwLock<PixelFrameBuffer>>,
    tile_frame_buffer: Arc<RwLock<TileFrameBuffer>>,
    tile_map: Arc<RwLock<TileMap>>,
    io_buffer: Arc<RwLock<VecDeque<u16>>>,
    input_pending: Arc<AtomicBool>,
    tile_vscroll_register: Arc<RwLock<(u8, u8)>>,
    tile_hscroll_register: Arc<RwLock<(u8, u8)>>,
    pixel_vscroll_register: Arc<RwLock<(u8, u8)>>,
    pixel_hscroll_register: Arc<RwLock<(u8, u8)>>,
    tile_scale_register: Arc<RwLock<u8>>,
    pixel_scale_register: Arc<RwLock<u8>>,
    sprite_scale_registers: Arc<RwLock<Vec<u8>>>,
    vga_status_register: Arc<RwLock<u8>>,
    vga_frame_register: Arc<RwLock<(u8, u8, u8, u8)>>,
    pending_interrupt: Arc<AtomicU32>,
    sprite_map: Arc<RwLock<SpriteMap>>,
    keyboard_mapper: GuestKeyboardMapper,
}

impl Graphics {
    pub fn new(
        pixel_frame_buffer: Arc<RwLock<PixelFrameBuffer>>,
        tile_frame_buffer: Arc<RwLock<TileFrameBuffer>>,
        tile_map: Arc<RwLock<TileMap>>,
        io_buffer: Arc<RwLock<VecDeque<u16>>>,
        input_pending: Arc<AtomicBool>,
        tile_vscroll_register: Arc<RwLock<(u8, u8)>>,
        tile_hscroll_register: Arc<RwLock<(u8, u8)>>,
        pixel_vscroll_register: Arc<RwLock<(u8, u8)>>,
        pixel_hscroll_register: Arc<RwLock<(u8, u8)>>,
        sprite_map: Arc<RwLock<SpriteMap>>,
        tile_scale_register: Arc<RwLock<u8>>,
        pixel_scale_register: Arc<RwLock<u8>>,
        sprite_scale_registers: Arc<RwLock<Vec<u8>>>,
        vga_status_register: Arc<RwLock<u8>>,
        vga_frame_register: Arc<RwLock<(u8, u8, u8, u8)>>,
        pending_interrupt: Arc<AtomicU32>,
    ) -> Graphics {
        let mut window: PistonWindow =
            WindowSettings::new("Dioptase", [WINDOW_WIDTH, WINDOW_HEIGHT])
                .exit_on_esc(true)
                .build()
                .unwrap();
        window.set_max_fps(60);
        window.set_ups(60);

        let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(FRAME_WIDTH, FRAME_HEIGHT);
        let texture = Texture::from_image(
            &mut window.create_texture_context(),
            &buffer,
            &TextureSettings::new().filter(Filter::Nearest),
        )
        .unwrap();

        Graphics {
            window,
            buffer,
            texture,
            pixel_frame_buffer,
            tile_frame_buffer,
            tile_map,
            io_buffer,
            input_pending,
            tile_vscroll_register,
            tile_hscroll_register,
            pixel_vscroll_register,
            pixel_hscroll_register,
            sprite_map,
            tile_scale_register,
            pixel_scale_register,
            sprite_scale_registers,
            vga_status_register,
            vga_frame_register,
            pending_interrupt,
            keyboard_mapper: GuestKeyboardMapper::new(),
        }
    }

    pub fn start(&mut self, finished: Arc<Mutex<bool>>, stay_open: bool) {
        while let Some(event) = self.window.next() {
            match event {
                Event::Loop(Loop::Update(_args)) => {
                    // Automatically closes window on program finish
                    if !stay_open && *finished.lock().unwrap() {
                        self.window.set_should_close(true);
                    }
                    self.update();
                }
                Event::Loop(Loop::Render(_args)) => {
                    self.window.draw_2d(&event, |context, graphics, _| {
                        clear([0.0; 4], graphics); // black background
                        let scale = DISPLAY_SCALE as f64;
                        image(
                            &self.texture,
                            context.transform.scale(scale, scale),
                            graphics,
                        );
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
                    if let Some(event_code) =
                        self.keyboard_mapper.translate_button(key, state, scancode)
                    {
                        self.io_buffer.write().unwrap().push_back(event_code);
                        self.input_pending.store(true, Ordering::SeqCst);
                    }
                }
                Event::Input(Input::Text(text), _) => {
                    if let Some(event_code) = self.keyboard_mapper.translate_text(&text) {
                        self.io_buffer.write().unwrap().push_back(event_code);
                        self.input_pending.store(true, Ordering::SeqCst);
                    }
                }
                Event::Input(Input::Focus(false), _) => {
                    self.keyboard_mapper.clear();
                }
                _ => {}
            }
        }
    }

    fn tile_layer_update(&mut self) {
        // draw the tile layer over the pixel layer
        let fb = self.tile_frame_buffer.read().unwrap();
        let tile_map = self.tile_map.read().unwrap();
        let scale = 1 << (*self.tile_scale_register.read().unwrap() as u32);
        for x in 0..fb.width_tiles {
            for y in 0..fb.height_tiles {
                let (tile_ptr, tile_color) = fb.get_tile_entry(x, y);
                let tile = &tile_map.tiles[tile_ptr as usize];
                for px in 0..TILE_WIDTH {
                    for py in 0..TILE_WIDTH {
                        let addr = (2 * (px + py * TILE_WIDTH)) as usize;
                        let tile_pixel_low = tile.pixels[addr];
                        let tile_pixel_high = tile.pixels[addr + 1];
                        // 0xFXXX pixels are transparent in the tile layer.
                        let transparent = (tile_pixel_high & 0xf0) == 0xf0;
                        if transparent {
                            continue;
                        }
                        let use_tile_color = (tile_pixel_high & 0xf0) == 0xc0;
                        let (red, green, blue) = if use_tile_color {
                            let (r4, g4, b4) = expand_rgb332(tile_color);
                            (r4 * 16, g4 * 16, b4 * 16)
                        } else {
                            (
                                (tile_pixel_low & 0x0f) as u8 * 16,
                                ((tile_pixel_low & 0xf0) >> 4) as u8 * 16,
                                (tile_pixel_high & 0x0f) as u8 * 16,
                            )
                        };
                        let pixel = Rgba([red, green, blue, 255]);

                        // positions in the logical screen
                        let scroll_x_pair = *self.tile_hscroll_register.read().unwrap();
                        let scroll_y_pair = *self.tile_vscroll_register.read().unwrap();
                        let scroll_x = decode_scroll_offset(scroll_x_pair);
                        let scroll_y = decode_scroll_offset(scroll_y_pair);
                        let raw_x: i32 = (x * TILE_WIDTH) as i32 + px as i32 + scroll_x;
                        let raw_y: i32 = (y * TILE_WIDTH) as i32 + py as i32 + scroll_y;
                        // Scroll registers are signed; use Euclidean modulo so large negative
                        // offsets continue wrapping correctly after many screens of scroll.
                        let final_x: u32 = raw_x.rem_euclid(FRAME_WIDTH as i32) as u32;
                        let final_y: u32 = raw_y.rem_euclid(FRAME_HEIGHT as i32) as u32;

                        // print the pixel rgba in the physical screen
                        for i in 0..scale {
                            for j in 0..scale {
                                let screen_x: u32 = final_x * scale + i;
                                let screen_y: u32 = final_y * scale + j;

                                if screen_x < SCREEN_WIDTH && screen_y < SCREEN_HEIGHT {
                                    self.buffer.put_pixel(screen_x, screen_y, pixel);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn pixel_layer_update(&mut self) {
        // draw the pixel layer as the background
        let fb = self.pixel_frame_buffer.read().unwrap();
        // Pixel layer uses an exponent with an implicit +1 so that:
        // n=0 -> 2x, n=1 -> 4x, matching 320x240 -> 640x480 at n=0.
        let scale = 1 << ((*self.pixel_scale_register.read().unwrap() as u32) + 1);
        for x in 0..fb.width_pixels {
            for y in 0..fb.height_pixels {
                let pixel = fb.get_pixel(x, y);
                let red = (pixel & 0x0F) as u8 * 16;
                let green = ((pixel & 0xF0) >> 4) as u8 * 16;
                let blue = ((pixel & 0xF00) >> 8) as u8 * 16;
                let pixel = Rgba([red, green, blue, 255]);

                // positions in the logical screen
                let scroll_x_pair = *self.pixel_hscroll_register.read().unwrap();
                let scroll_y_pair = *self.pixel_vscroll_register.read().unwrap();
                let scroll_x = decode_scroll_offset(scroll_x_pair);
                let scroll_y = decode_scroll_offset(scroll_y_pair);
                let raw_x: i32 = x as i32 + scroll_x;
                let raw_y: i32 = y as i32 + scroll_y;
                // Scroll registers are signed; use Euclidean modulo so large negative
                // offsets continue wrapping correctly after many screens of scroll.
                let final_x: u32 = raw_x.rem_euclid(FRAME_WIDTH as i32) as u32;
                let final_y: u32 = raw_y.rem_euclid(FRAME_HEIGHT as i32) as u32;

                // print the pixel rgba in the physical screen
                for i in 0..scale {
                    for j in 0..scale {
                        let screen_x: u32 = final_x * scale + i;
                        let screen_y: u32 = final_y * scale + j;

                        if screen_x < SCREEN_WIDTH && screen_y < SCREEN_HEIGHT {
                            self.buffer.put_pixel(screen_x, screen_y, pixel);
                        }
                    }
                }
            }
        }
    }

    fn update(&mut self) {
        // set status to busy
        *self.vga_status_register.write().unwrap() = 0;

        // Updates buffer from emulated frame buffers and tile map.
        self.pixel_layer_update();
        self.tile_layer_update();

        // draw the sprites of the sprite map
        let sprite_map = self.sprite_map.read().unwrap();
        let sprite_scales = self.sprite_scale_registers.read().unwrap();
        for (sprite_index, sprite) in sprite_map.sprites.iter().enumerate() {
            let scale = 1 << (sprite_scales.get(sprite_index).copied().unwrap_or(0) as u32);
            // Sprite coordinates are signed 16-bit little-endian MMIO values.
            let sprite_x = i32::from(i16::from_le_bytes([sprite.x.0, sprite.x.1]));
            let sprite_y = i32::from(i16::from_le_bytes([sprite.y.0, sprite.y.1]));
            for px in 0..SPRITE_WIDTH {
                for py in 0..SPRITE_WIDTH {
                    let addr = (2 * (px + py * SPRITE_WIDTH)) as usize;
                    let tile_pixel_low = sprite.pixels[addr];
                    let tile_pixel_high = sprite.pixels[addr + 1];
                    let red = (tile_pixel_low & 0x0f) as u8 * 16;
                    let green = ((tile_pixel_low & 0xf0) >> 4) as u8 * 16;
                    let blue = (tile_pixel_high & 0x0f) as u8 * 16;
                    let transparent = (tile_pixel_high & 0xf0) == 0xf0;
                    if transparent {
                        continue;
                    }

                    let pixel = Rgba([red, green, blue, 255]);
                    // Reconstruct the full coordinate before adding the per-pixel offset so carry
                    // from the low byte is preserved (the previous bytewise OR math dropped carry).
                    let final_x = sprite_x + px as i32;
                    let final_y = sprite_y + py as i32;
                    if final_x < 0 || final_y < 0 {
                        continue;
                    }
                    let final_x = final_x as u32;
                    let final_y = final_y as u32;

                    // print the pixel rgba in the physical screen
                    for i in 0..scale {
                        for j in 0..scale {
                            let screen_x: u32 = final_x * scale + i;
                            let screen_y: u32 = final_y * scale + j;

                            if screen_x < SCREEN_WIDTH && screen_y < SCREEN_HEIGHT {
                                self.buffer.put_pixel(screen_x, screen_y, pixel);
                            }
                        }
                    }
                }
            }
        }

        // increment frame register
        let mut vga_frame_register = self.vga_frame_register.write().unwrap();
        vga_frame_register.0 = vga_frame_register.0.wrapping_add(1);
        if vga_frame_register.0 == 0 {
            vga_frame_register.1 = vga_frame_register.1.wrapping_add(1);
            if vga_frame_register.1 == 0 {
                vga_frame_register.2 = vga_frame_register.2.wrapping_add(1);
                if vga_frame_register.2 == 0 {
                    vga_frame_register.3 = vga_frame_register.3.wrapping_add(1);
                }
            }
        }

        // Updates texture from buffer
        self.texture = Texture::from_image(
            &mut self.window.create_texture_context(),
            &self.buffer,
            &TextureSettings::new().filter(Filter::Nearest),
        )
        .unwrap();

        // set status to idle
        *self.vga_status_register.write().unwrap() = 3;

        // send vblank interrupt
        self.pending_interrupt
            .fetch_or(VGA_INTERRUPT_BIT, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn text_fallback_recovers_base_key_from_shifted_punctuation() {
        assert_eq!(guest_keycode_from_text_char('!'), Some(b'1'));
        assert_eq!(guest_keycode_from_text_char('"'), Some(b'\''));
        assert_eq!(guest_keycode_from_text_char('~'), Some(b'`'));
        assert_eq!(guest_keycode_from_text_char('|'), Some(b'\\'));
    }

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
}
