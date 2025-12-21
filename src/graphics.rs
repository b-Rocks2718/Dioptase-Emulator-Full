use piston_window::*;
use ::image::{ImageBuffer, Rgba};
use std::{collections::VecDeque, sync::{Arc, Mutex, RwLock}};

use crate::memory::*;

const SCREEN_WIDTH: u32 = 640;
const SCREEN_HEIGHT: u32 = 480;

pub struct Graphics {
    window: PistonWindow,
    buffer: ImageBuffer<Rgba<u8>, Vec<u8>>,
    texture: G2dTexture,
    frame_buffer: Arc<RwLock<FrameBuffer>>,
    tile_map: Arc<RwLock<TileMap>>,
    io_buffer: Arc<RwLock<VecDeque<u16>>>,
    vscroll_register: Arc<RwLock<(u8, u8)>>,
    hscroll_register: Arc<RwLock<(u8, u8)>>,
    scale_register: Arc<RwLock<u8>>,
    vga_mode_register: Arc<RwLock<u8>>,
    vga_status_register: Arc<RwLock<u8>>,
    vga_frame_register: Arc<RwLock<(u8, u8, u8, u8)>>,
    pending_interrupt: Arc<RwLock<u32>>,
    sprite_map: Arc<RwLock<SpriteMap>>,
}

impl Graphics {

    pub fn new(
        frame_buffer: Arc<RwLock<FrameBuffer>>, 
        tile_map: Arc<RwLock<TileMap>>, 
        io_buffer: Arc<RwLock<VecDeque<u16>>>, 
        vscroll_register: Arc<RwLock<(u8, u8)>>,
        hscroll_register: Arc<RwLock<(u8, u8)>>,
        sprite_map: Arc<RwLock<SpriteMap>>,
        scale_register: Arc<RwLock<u8>>,
        vga_mode_register: Arc<RwLock<u8>>,
        vga_status_register: Arc<RwLock<u8>>,
        vga_frame_register: Arc<RwLock<(u8, u8, u8, u8)>>,
        pending_interrupt: Arc<RwLock<u32>>,
    ) -> Graphics {
        let mut window: PistonWindow = WindowSettings::new("Dioptase", [SCREEN_WIDTH, SCREEN_HEIGHT])
            .exit_on_esc(true)
            .build()
            .unwrap();
        window.set_max_fps(60);
        window.set_ups(60);

        let buffer: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(FRAME_WIDTH, FRAME_HEIGHT);
        let texture = Texture::from_image(
            &mut window.create_texture_context(),
            &buffer,
            &TextureSettings::new(),
        ).unwrap();

        Graphics { 
            window,
            buffer,
            texture,
            frame_buffer,
            tile_map,
            io_buffer,
            vscroll_register,
            hscroll_register,
            sprite_map,
            vga_mode_register,
            vga_status_register,
            vga_frame_register,
            scale_register,
            pending_interrupt
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
                        image(&self.texture, context.transform, graphics);
                    });
                }
                Event::Input(Input::Button(ButtonArgs { 
                    button: Button::Keyboard(key), 
                    state, .. }), _) => {
                    match state {
                        ButtonState::Press => {
                            // Handle key press here
                            self.io_buffer.write().unwrap().push_back(key as u16 & 0xFF);
                            //println!("Key pressed: {:?}", key);
                        }
                        ButtonState::Release => {
                            // Handle key release here
                            self.io_buffer.write().unwrap().push_back(key as u16 & 0xFF | 0x100);
                            //println!("Key released: {:?}", key);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn tile_mode_update(&mut self) {
        // draw the tiles of the frame buffer
        let fb = self.frame_buffer.read().unwrap();
        let tile_map = self.tile_map.read().unwrap();
        let scale = 1 << (*self.scale_register.read().unwrap() as u32);
        for x in 0..fb.width_tiles {
            for y in 0..fb.height_tiles {
                let tile_ptr = fb.get_tile(x, y);
                let tile = &tile_map.tiles[tile_ptr as usize];
                for px in 0..TILE_WIDTH {
                    for py in 0..TILE_WIDTH {
                        let addr = (2 * px + py * TILE_WIDTH) as usize;
                        let tile_pixel_low = tile.pixels[addr];
                        let tile_pixel_high = tile.pixels[addr + 1];
                        let red = (tile_pixel_low & 0x0f) as u8 * 16;
                        let green = ((tile_pixel_low & 0xf0) >> 4) as u8 * 16;
                        let blue = (tile_pixel_high & 0x0f) as u8 * 16;
                        let pixel = Rgba([red, green, blue, 255]);
                        
                        // positions in the logical screen
                        let scroll_x_pair = *self.hscroll_register.read().unwrap();
                        let scroll_y_pair = *self.vscroll_register.read().unwrap();
                        let scroll_x = (i32::from(scroll_x_pair.1) << 8) | i32::from(scroll_x_pair.0);
                        let scroll_y = (i32::from(scroll_y_pair.1) << 8) | i32::from(scroll_y_pair.0);
                        let raw_x: i32 = (x * TILE_WIDTH) as i32 + px as i32 + scroll_x;
                        let raw_y: i32 = (y * TILE_WIDTH) as i32 + py as i32 + scroll_y;
                        let final_x: u32 = (raw_x + FRAME_WIDTH as i32) as u32 % FRAME_WIDTH;
                        let final_y: u32 = (raw_y + FRAME_HEIGHT as i32) as u32 % FRAME_HEIGHT;

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

    fn pixel_mode_update(&mut self) {
        // draw the pixels of the frame buffer
        let fb = self.frame_buffer.read().unwrap();
        let scale = 1 << (*self.scale_register.read().unwrap() as u32);
        for x in 0..(fb.width_pixels/2) {
            for y in 0..(fb.height_pixels/2) {
                let pixel = fb.get_pixel(x, y);
                let red = (pixel & 0x0F) as u8 * 16;
                let green = ((pixel & 0xF0) >> 4) as u8 * 16;
                let blue = ((pixel & 0xF00) >> 8) as u8 * 16;
                let pixel = Rgba([red, green, blue, 255]);

                // positions in the logical screen
                let scroll_x_pair = *self.hscroll_register.read().unwrap();
                let scroll_y_pair = *self.vscroll_register.read().unwrap();
                let scroll_x = (i32::from(scroll_x_pair.1) << 8) | i32::from(scroll_x_pair.0);
                let scroll_y = (i32::from(scroll_y_pair.1) << 8) | i32::from(scroll_y_pair.0);
                let raw_x: i32 = x as i32 + scroll_x;
                let raw_y: i32 = y as i32 + scroll_y;
                let final_x: u32 = (raw_x + FRAME_WIDTH as i32) as u32 % FRAME_WIDTH;
                let final_y: u32 = (raw_y + FRAME_HEIGHT as i32) as u32 % FRAME_HEIGHT;

                // print the pixel rgba in the physical screen
                for i in 0..(scale+1) {
                    for j in 0..(scale+1) {
                        let screen_x: u32 = final_x * (scale + 1) + i;
                        let screen_y: u32 = final_y * (scale + 1) + j;

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

        // Updates buffer from emulated frame buffer and tile map
        
        if *self.vga_mode_register.read().unwrap() == 0 {
            // in tile mode
            self.tile_mode_update();
        } else if *self.vga_mode_register.read().unwrap() == 1 {
            // in pixel mode
            self.pixel_mode_update();
        } else {
            println!("Warning: unknown VGA mode {}", *self.vga_mode_register.read().unwrap());
            return;
        }

        let scale = 1 << (*self.scale_register.read().unwrap() as u32);

        // draw the sprites of the sprite map
        let sprite_map = self.sprite_map.read().unwrap();
        for sprite in &sprite_map.sprites {
            for px in 0..SPRITE_WIDTH {
                for py in 0..SPRITE_WIDTH {
                    let addr = (2 * px + py * SPRITE_WIDTH) as usize;
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
                    let final_x: u32 = (u32::from(sprite.x.1) << 8) | (u32::from(sprite.x.0) + px);
                    let final_y: u32 = (u32::from(sprite.y.1) << 8) | (u32::from(sprite.y.0) + py);

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
            &TextureSettings::new(),
        ).unwrap();

        // set status to idle
        *self.vga_status_register.write().unwrap() = 3;

        // send vblank interrupt
        *self.pending_interrupt.write().unwrap() |= VGA_INTERRUPT_BIT;
    }
}