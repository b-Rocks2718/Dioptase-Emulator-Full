use std::collections::HashMap;
use std::collections::VecDeque;

use std::u16;
use std::io::{self, Write};
use std::sync::{Arc, RwLock};

pub const PHYSMEM_MAX: u32 = 0x7FFFFFF;

pub const FRAME_WIDTH: u32 = 640;
pub const FRAME_HEIGHT: u32 = 480;
pub const TILE_WIDTH: u32 = 8;
// const TILES_NUM: u32 = 128;
const TILE_SIZE: u32 = TILE_WIDTH * TILE_WIDTH * 2;
pub const SPRITE_WIDTH: u32 = 32;
// const SPRITES_NUM: u32 = 8;
const SPRITE_SIZE: u32 = SPRITE_WIDTH * SPRITE_WIDTH * 2;

// SD card is memory-mapped:
// - SD_CMD_BUF..+5: command bytes (write-only; mirrored into RAM for visibility)
// - SD_BUF_START..+512: data buffer for single-block transfers
// - SD_SEND_BYTE: write to execute current command and copy response/data back into RAM
// Reads of SD_SEND_BYTE return busy status (1 while executing, else 0).
const SD_CMD_BUF_LEN: usize = 6;
const SD_BLOCK_SIZE: usize = 512;
pub const SD_INTERRUPT_BIT: u32 = 1 << 3;
pub const VGA_INTERRUPT_BIT: u32 = 1 << 4;

const FRAME_BUFFER_START : u32 = 0x7FC0000;
const FRAME_BUFFER_SIZE : u32 = 0x25800; // 320 * 240 * 2 bytes

const PS2_STREAM : u32 = 0x7FE5800;
const UART_TX : u32 = 0x7FE5802;
const UART_RX : u32 = 0x7FE5803;
pub const PIT_START : u32 = 0x7FE5804;

const SD_SEND_BYTE : u32 = 0x7FE58F9;
const SD_CMD_BUF : u32  = 0x7FE58FA;
const SD_BUF_START : u32 = 0x7FE5900;

const SPRITE_REGISTERS_START : u32 = 0x7FE5B00;  // every consecutive pair of words correspond to 
const SPRITE_REGISTERS_SIZE : u32 = 0x40;     // the y and x coordinates, respectively of a sprite

const H_SCROLL_START : u32 = 0x7FE5B40;
const V_SCROLL_START : u32 = 0x7FE5B42;

const SCALE_REGISTER_START : u32 = 0x7FE5B44; // each pixel is repeated 2^n times

const VGA_MODE_REGISTER_START : u32 = 0x7FE5B45;
const VGA_STATUS_REGISTER_START : u32 = 0x7FE5B46;
const VGA_FRAME_REGISTER_START : u32 = 0x7FE5B48;

const TILE_MAP_START : u32 = 0x7FE8000;
const TILE_MAP_SIZE : u32 = 0x8000;

const SPRITE_MAP_START : u32 = 0x7FF0000;
const SPRITE_MAP_SIZE : u32 = 0x8000;

pub struct Memory {
  ram: HashMap<u32, u8>,   
  frame_buffer: Arc<RwLock<FrameBuffer>>,
  tile_map: Arc<RwLock<TileMap>>, 
  io_buffer: Arc<RwLock<VecDeque<u16>>>,
  vscroll_register: Arc<RwLock<(u8, u8)>>,
  hscroll_register: Arc<RwLock<(u8, u8)>>,
  scale_register: Arc<RwLock<u8>>,
  vga_mode_register: Arc<RwLock<u8>>,
  vga_status_register: Arc<RwLock<u8>>,
  vga_frame_register: Arc<RwLock<(u8, u8, u8, u8)>>,
  pit: Arc<RwLock<(u8, u8, u8, u8)>>,
  sprite_map: Arc<RwLock<SpriteMap>>,
  sd_card: Arc<RwLock<SdCard>>,
  pending_interrupt: Arc<RwLock<u32>>,
  use_uart_rx: bool
}

// an 80x60 framebuffer of 8-bit tile values
pub struct FrameBuffer {
    pub width_pixels: u32,
    pub height_pixels: u32,
    pub width_tiles: u32, // number of tiles in the x direction
    pub height_tiles: u32, // number of tiles in the y direction
    tile_ptrs: Vec<u8>,
}

pub struct TileMap {
    pub tiles: Vec<Tile>
}

#[derive(Clone)]
pub struct Tile {
    pub pixels: Vec<u8>, // an 8x8 tile of pixels
}

pub struct SpriteMap {
    pub sprites: Vec<Sprite>,
}

#[derive(Clone)]
pub struct Sprite {
    pub x: (u8, u8),
    pub y: (u8, u8),
    pub pixels: Vec<u8>, // a 32x32 tile of pixels
}

struct SdCard {
    // Minimal single-block SD card emulation backing the memory map above.
    command: [u8; SD_CMD_BUF_LEN],
    response: [u8; SD_CMD_BUF_LEN],
    response_len: usize,
    data_buffer: [u8; SD_BLOCK_SIZE],
    storage: HashMap<u32, Vec<u8>>,
    idle: bool,
    initialized: bool,
    high_capacity: bool,
    awaiting_app_cmd: bool,
    ocr: u32,
    busy: bool,
}

struct SdCommandResult {
    response_len: usize,
    update_data_buffer: bool,
    interrupt: bool,
}

impl SdCard {
    fn new() -> Self {
        SdCard {
            command: [0; SD_CMD_BUF_LEN],
            response: [0; SD_CMD_BUF_LEN],
            response_len: 0,
            data_buffer: [0; SD_BLOCK_SIZE],
            storage: HashMap::new(),
            idle: true,
            initialized: false,
            high_capacity: false,
            awaiting_app_cmd: false,
            ocr: 0x00FF8000,
            busy: false,
        }
    }

    fn status(&self) -> u8 {
        if self.busy { 1 } else { 0 }
    }

    fn write_command_byte(&mut self, offset: usize, value: u8) {
        if offset < SD_CMD_BUF_LEN {
            self.command[offset] = value;
            self.response[offset] = value;
            if offset + 1 > self.response_len {
                self.response_len = offset + 1;
            }
        }
    }

    fn write_data_byte(&mut self, offset: usize, value: u8) {
        if offset < SD_BLOCK_SIZE {
            self.data_buffer[offset] = value;
        }
    }

    fn execute(&mut self) -> SdCommandResult {
        self.busy = true;
        let mut result = SdCommandResult {
            response_len: 1,
            update_data_buffer: false,
            interrupt: true,
        };

        self.response.fill(0);
        self.response_len = 0;

        let raw_cmd = self.command[0];
        let cmd_index = raw_cmd & 0x3F;
        let arg = ((self.command[1] as u32) << 24)
            | ((self.command[2] as u32) << 16)
            | ((self.command[3] as u32) << 8)
            | (self.command[4] as u32);

        let is_acmd = cmd_index == 41;
        if cmd_index != 55 && !is_acmd {
            self.awaiting_app_cmd = false;
        }

        match cmd_index {
            0 => {
                self.idle = true;
                self.initialized = false;
                self.high_capacity = false;
                self.awaiting_app_cmd = false;
                self.set_response(&[0x01]);
            }
            8 => {
                let status = if self.initialized { 0x00 } else { 0x01 };
                let resp = [
                    status,
                    self.command[1],
                    self.command[2],
                    self.command[3],
                    self.command[4],
                ];
                self.set_response(&resp);
            }
            55 => {
                self.awaiting_app_cmd = true;
                let status = if self.initialized { 0x00 } else { 0x01 };
                self.set_response(&[status]);
            }
            41 => {
                if !self.awaiting_app_cmd {
                    self.set_response(&[0x05]);
                } else {
                    self.awaiting_app_cmd = false;
                    self.initialized = true;
                    self.idle = false;
                    self.high_capacity = (arg & (1 << 30)) != 0;
                    self.set_response(&[0x00]);
                }
            }
            58 => {
                let status = if self.initialized { 0x00 } else { 0x01 };
                let mut ocr = self.ocr;
                if self.high_capacity {
                    ocr |= 1 << 30;
                } else {
                    ocr &= !(1 << 30);
                }
                let resp = [
                    status,
                    ((ocr >> 24) & 0xFF) as u8,
                    ((ocr >> 16) & 0xFF) as u8,
                    ((ocr >> 8) & 0xFF) as u8,
                    (ocr & 0xFF) as u8,
                ];
                self.set_response(&resp);
            }
            17 => {
                if !self.initialized {
                    self.set_response(&[0x05]);
                } else if !self.high_capacity && (arg % (SD_BLOCK_SIZE as u32) != 0) {
                    self.set_response(&[0x05]);
                } else {
                    let block_index = if self.high_capacity {
                        arg
                    } else {
                        arg / (SD_BLOCK_SIZE as u32)
                    };
                    let data = self
                        .storage
                        .entry(block_index)
                        .or_insert_with(|| vec![0; SD_BLOCK_SIZE]);
                    self.data_buffer.copy_from_slice(data.as_slice());
                    self.set_response(&[0x00]);
                    result.update_data_buffer = true;
                }
            }
            24 => {
                if !self.initialized {
                    self.set_response(&[0x05]);
                } else if !self.high_capacity && (arg % (SD_BLOCK_SIZE as u32) != 0) {
                    self.set_response(&[0x05]);
                } else {
                    let block_index = if self.high_capacity {
                        arg
                    } else {
                        arg / (SD_BLOCK_SIZE as u32)
                    };
                    let data = self
                        .storage
                        .entry(block_index)
                        .or_insert_with(|| vec![0; SD_BLOCK_SIZE]);
                    data.as_mut_slice()
                        .copy_from_slice(&self.data_buffer);
                    self.set_response(&[0x00]);
                }
            }
            _ => {
                self.set_response(&[0x05]);
            }
        }

        result.response_len = self.response_len;
        self.busy = false;
        result
    }

    fn set_response(&mut self, bytes: &[u8]) {
        self.response.fill(0);
        for (i, value) in bytes.iter().enumerate() {
            if i < SD_CMD_BUF_LEN {
                self.response[i] = *value;
            }
        }
        self.response_len = bytes.len().min(SD_CMD_BUF_LEN);
        if self.response_len == 0 {
            self.response_len = 1;
        }
    }
}

impl Memory {
    pub fn new(ram: HashMap<u32, u8>, use_uart_rx: bool) -> Memory {

        Memory {
            ram,
            frame_buffer: Arc::new(RwLock::new(FrameBuffer::new(FRAME_WIDTH, FRAME_HEIGHT))),
            tile_map: Arc::new(RwLock::new(TileMap::new(TILE_MAP_SIZE))),
            io_buffer: Arc::new(RwLock::new(VecDeque::new())),
            vscroll_register: Arc::new(RwLock::new((0, 0))),
            hscroll_register: Arc::new(RwLock::new((0, 0))),
            scale_register: Arc::new(RwLock::new(0)),
            vga_mode_register: Arc::new(RwLock::new(0)),
            vga_status_register: Arc::new(RwLock::new(0)),
            vga_frame_register: Arc::new(RwLock::new((0, 0, 0, 0))),
            pit: Arc::new(RwLock::new((0, 0, 0, 0))),
            sprite_map: Arc::new(RwLock::new(SpriteMap::new(SPRITE_MAP_SIZE))),
            sd_card: Arc::new(RwLock::new(SdCard::new())),
            pending_interrupt: Arc::new(RwLock::new(0)),
            use_uart_rx: use_uart_rx
        }
    }

    pub fn get_frame_buffer(&self) -> Arc<RwLock<FrameBuffer>> { return Arc::clone(&self.frame_buffer)}
    pub fn get_tile_map(&self) -> Arc<RwLock<TileMap>> { return Arc::clone(&self.tile_map)}
    pub fn get_io_buffer(&self) -> Arc<RwLock<VecDeque<u16>>> { return Arc::clone(&self.io_buffer) }
    pub fn get_vscroll_register(&self) -> Arc<RwLock<(u8, u8)>> { return Arc::clone(&self.vscroll_register) }
    pub fn get_hscroll_register(&self) -> Arc<RwLock<(u8, u8)>> { return Arc::clone(&self.hscroll_register) }
    pub fn get_scale_register(&self) -> Arc<RwLock<u8>> { return Arc::clone(&self.scale_register) }
    pub fn get_sprite_map(&self) -> Arc<RwLock<SpriteMap>> { return Arc::clone(&self.sprite_map) }
    pub fn get_vga_mode_register(&self) -> Arc<RwLock<u8>> { return Arc::clone(&self.vga_mode_register) }
    pub fn get_vga_status_register(&self) -> Arc<RwLock<u8>> { return Arc::clone(&self.vga_status_register) }
    pub fn get_vga_frame_register(&self) -> Arc<RwLock<(u8, u8, u8, u8)>> { return Arc::clone(&self.vga_frame_register) }
    pub fn get_pending_interrupt(&self) -> Arc<RwLock<u32>> { return Arc::clone(&self.pending_interrupt) }

    pub fn read(&mut self, addr: u32) -> u8 {
        assert!(addr <= PHYSMEM_MAX, "Physical memory address out of bounds: 0x{:08X}", addr);

        if addr >= TILE_MAP_START && addr < TILE_MAP_START + TILE_MAP_SIZE {
            return self.tile_map.read().unwrap().get_tile_byte(addr - TILE_MAP_START);
        }
        else if addr >= FRAME_BUFFER_START && addr < FRAME_BUFFER_START + FRAME_BUFFER_SIZE {
            return self.frame_buffer.read().unwrap().get_tile_pair(addr - FRAME_BUFFER_START);
        }
        else if addr == SD_SEND_BYTE {
            return self.sd_card.read().unwrap().status();
        }
        else if addr == PS2_STREAM {
            // kind of a hack but this assumed people always read a double from ps2 stream
            if self.use_uart_rx {return 0;}
            return self.io_buffer.write().unwrap().front().unwrap_or(&0).clone() as u8;
        }
        else if addr == PS2_STREAM + 1 {
            // read of upper byte will cause a pop
            if self.use_uart_rx {return 0;}
            return (self.io_buffer.write().unwrap().pop_front().unwrap_or(0).clone() >> 8) as u8;
        }
        else if addr >= SPRITE_MAP_START && addr < SPRITE_MAP_START + SPRITE_MAP_SIZE {
            return self.sprite_map.read().unwrap().get_sprite_byte(addr - SPRITE_MAP_START);
        }
        else if addr >= SPRITE_REGISTERS_START && addr < SPRITE_REGISTERS_START + SPRITE_REGISTERS_SIZE {
            return self.sprite_map.read().unwrap().get_sprite_reg((addr - SPRITE_REGISTERS_START) as u32);
        }
        else if addr == V_SCROLL_START {
            return self.vscroll_register.read().unwrap().0;
        }
        else if addr == V_SCROLL_START + 1 {
            return self.vscroll_register.read().unwrap().1;
        }
        else if addr == H_SCROLL_START {
            return self.hscroll_register.read().unwrap().0;
        }
        else if addr == H_SCROLL_START + 1 {
            return self.hscroll_register.read().unwrap().1;
        }
        else if addr == SCALE_REGISTER_START {
            return *self.scale_register.read().unwrap();
        }
        else if addr == VGA_MODE_REGISTER_START {
            return *self.vga_mode_register.read().unwrap();
        }
        else if addr == VGA_STATUS_REGISTER_START {
            return *self.vga_status_register.read().unwrap();
        }
        else if addr == VGA_FRAME_REGISTER_START {
            return self.vga_frame_register.read().unwrap().0;
        }
        else if addr == VGA_FRAME_REGISTER_START + 1 {
            return self.vga_frame_register.read().unwrap().1;
        }
        else if addr == VGA_FRAME_REGISTER_START + 2 {
            return self.vga_frame_register.read().unwrap().2;
        }
        else if addr == VGA_FRAME_REGISTER_START + 3 {
            return self.vga_frame_register.read().unwrap().3;
        }
        else if addr == UART_TX {
            panic!("attempting to read output port (address {:X})", UART_TX);
        }
        else if addr == UART_RX {
            // get value
            if self.use_uart_rx {
              let value = self.io_buffer.write().unwrap().pop_front().unwrap_or(0).clone();
              if value & 0xFF00 != 0 {
                return 0; // ignore keyup
              }
              return value as u8;
            } else {
              return 0;
            }
        }
        else if addr == PIT_START {
            return self.pit.read().unwrap().0;
        }
        else if addr == PIT_START + 1 {
            return self.pit.read().unwrap().1;
        }
        else if addr == PIT_START + 2 {
            return self.pit.read().unwrap().2;
        }
        else if addr == PIT_START + 3 {
            return self.pit.read().unwrap().3;
        }
        else if addr == 0 {
            println!("Warning: reading from physical address 0x00000000");
        }

        if self.ram.contains_key(&addr) {
            return self.ram[&addr];
        } else {
            return 0;
        }
    }

    pub fn write(&mut self, addr: u32, data: u8) {
        assert!(addr <= PHYSMEM_MAX, "Physical memory address out of bounds: 0x{:08X}", addr);
        
        if addr >= TILE_MAP_START && addr < TILE_MAP_START + TILE_MAP_SIZE {
            self.tile_map.write().unwrap().set_tile_byte((addr - TILE_MAP_START) as u32, data);
        }
        else if addr >= FRAME_BUFFER_START && addr < FRAME_BUFFER_START + FRAME_BUFFER_SIZE {
            self.frame_buffer.write().unwrap().set_tile_pair((addr - FRAME_BUFFER_START) as u32, data);
        }
        else if addr == SD_SEND_BYTE {
            let (response, response_len, updated_buffer, interrupt) = {
                let mut sd = self.sd_card.write().unwrap();
                let result = sd.execute();
                let response = sd.response;
                let response_len = result.response_len;
                let buffer = if result.update_data_buffer {
                    Some(sd.data_buffer.to_vec())
                } else {
                    None
                };
                (response, response_len, buffer, result.interrupt)
            };

            for i in 0..SD_CMD_BUF_LEN {
                let value = if i < response_len { response[i] } else { 0 };
                self.ram.insert(SD_CMD_BUF + i as u32, value);
            }

            if let Some(buffer) = updated_buffer {
                for (i, value) in buffer.iter().enumerate() {
                    self.ram.insert(SD_BUF_START + i as u32, *value);
                }
            }

            if interrupt {
                *self.pending_interrupt.write().unwrap() |= SD_INTERRUPT_BIT;
            }
            return;
        }
        else if addr >= SD_CMD_BUF && addr < SD_CMD_BUF + SD_CMD_BUF_LEN as u32 {
            let offset = (addr - SD_CMD_BUF) as usize;
            {
                let mut sd = self.sd_card.write().unwrap();
                sd.write_command_byte(offset, data);
            }
            self.ram.insert(addr, data);
            return;
        }
        else if addr >= SD_BUF_START && addr < SD_BUF_START + SD_BLOCK_SIZE as u32 {
            let offset = (addr - SD_BUF_START) as usize;
            {
                let mut sd = self.sd_card.write().unwrap();
                sd.write_data_byte(offset, data);
            }
            self.ram.insert(addr, data);
            return;
        }
        else if addr == PS2_STREAM {
            panic!("attempting to write input port (address {:X})", PS2_STREAM);
        }
        else if addr == UART_TX {
            print!("{}", data as char);
            io::stdout().flush().unwrap();
        }
        else if addr == UART_RX {
            panic!("attempting to write input port (address {:X})", UART_RX);
        }
        else if addr == V_SCROLL_START {
            self.vscroll_register.write().unwrap().0 = data;
        }
        else if addr == V_SCROLL_START + 1 {
            self.vscroll_register.write().unwrap().1 = data;
        }
        else if addr == H_SCROLL_START {
            self.hscroll_register.write().unwrap().0 = data;
        }
        else if addr == H_SCROLL_START + 1 {
            self.hscroll_register.write().unwrap().1 = data;
        }
        else if addr == SCALE_REGISTER_START {
            *self.scale_register.write().unwrap() = data;
        }
        else if addr >= SPRITE_MAP_START && addr < SPRITE_MAP_START + SPRITE_MAP_SIZE {
            self.sprite_map.write().unwrap().set_sprite_byte((addr - SPRITE_MAP_START) as u32, data);
        }
        else if addr >= SPRITE_REGISTERS_START && addr < SPRITE_REGISTERS_START + SPRITE_REGISTERS_SIZE {
            self.sprite_map.write().unwrap().set_sprite_reg((addr - SPRITE_REGISTERS_START) as u32, data);
        }
        else if addr == PIT_START {
            self.pit.write().unwrap().0 = data;
        }
        else if addr == PIT_START + 1 {
            self.pit.write().unwrap().1 = data;
        }
        else if addr == PIT_START + 2 {
            self.pit.write().unwrap().2 = data;
        }
        else if addr == PIT_START + 3 {
            self.pit.write().unwrap().3 = data;
        }
        else if addr == VGA_MODE_REGISTER_START {
            *self.vga_mode_register.write().unwrap() = data;
        }
        else if addr == VGA_STATUS_REGISTER_START {
            panic!("attempting to write read-only VGA status register (0x{:08X})", VGA_STATUS_REGISTER_START);
        }
        else if VGA_FRAME_REGISTER_START <= addr && addr < VGA_FRAME_REGISTER_START + 4 {
            panic!("attempting to write read-only VGA frame register (0x{:08X})", VGA_FRAME_REGISTER_START);
        }
        else if addr == 0 {
            println!("Warning: writing to physical address 0x00000000: 0x{:08X}", data);
        }

        self.ram.insert(addr, data);
    }

    pub fn clock() {
        // do stuff that should happen every clock cycle
        
    }

    pub fn check_interrupts(&self) -> u32 {
        let pending = { *self.pending_interrupt.read().unwrap() };
        if pending != 0 {
            *self.pending_interrupt.write().unwrap() = 0;
        }
        pending
    }
}

impl FrameBuffer {
    pub fn new(width_pixels: u32, height_pixels: u32) -> Self {
        let width_tiles = width_pixels / TILE_WIDTH;
        let height_tiles = height_pixels / TILE_WIDTH;
        FrameBuffer {
            width_pixels,
            height_pixels,
            width_tiles,
            height_tiles,
            tile_ptrs: vec![0; (width_pixels * height_pixels) as usize],
        }
    }

    pub fn set_tile_pair(&mut self, i: u32, tile_pair_value: u8) {
        // we're packing 2 tile_ptrs into 1 word
        if i < self.tile_ptrs.len() as u32 {
            self.tile_ptrs[i as usize] = tile_pair_value;
        } else {
            panic!("Tile coordinates out of bounds: {}", i);
        }
    }

    pub fn get_tile_pair(&self, i: u32) -> u8 {
        // we're packing 2 tile_ptrs into 1 word
        if i < self.tile_ptrs.len() as u32 {
            return self.tile_ptrs[i as usize];
        } else {
            panic!("Tile coordinates out of bounds");
        }
    }

    pub fn get_pixel(&self, x: u32, y: u32) -> u16 {
        if x < self.width_pixels && y < self.height_pixels {
            let idx: usize = (x + y * (self.width_pixels/ 2)) as usize;
            return ((u16::from(self.tile_ptrs[2 * idx + 1])) << 8) | u16::from(self.tile_ptrs[2 * idx]);
        } else {
            panic!("Tile coordinates out of bounds");
        }
    }

    pub fn get_tile(&self, x: u32, y: u32) -> u8 {
        if x < self.width_tiles && y < self.height_tiles {
            let idx: usize = (x + y * self.width_tiles) as usize;
            return self.tile_ptrs[idx];
        } else {
            panic!("Tile coordinates out of bounds");
        }
    }
}

impl Tile {
    pub fn black() -> Tile {
        Tile {
            pixels: vec![0; TILE_SIZE as usize]
        }
    }
    pub fn white() -> Tile {
        Tile {
            pixels: vec![0xff; TILE_SIZE as usize]
        }
    }
}

impl TileMap {
    pub fn new(size: u32) -> TileMap {
        let tiles = vec![Tile::black(); (size / TILE_SIZE) as usize];
        TileMap { 
            tiles
        }
    }

    pub fn get_tile_byte(&self, addr: u32) -> u8 {
        return self.tiles[(addr / TILE_SIZE) as usize].pixels[(addr % TILE_SIZE) as usize];
    }

    pub fn set_tile_byte(&mut self, addr: u32, data: u8) {
        self.tiles[(addr / TILE_SIZE) as usize].pixels[(addr % TILE_SIZE) as usize] = data;
    }
}

impl Sprite {
    pub fn invisible() -> Sprite {
        Sprite {
            x: (0, 0),
            y: (0, 0),
            pixels: vec![0xFF; SPRITE_SIZE as usize],
        }
    }
}

impl SpriteMap {
    pub fn new(size: u32) -> SpriteMap {
        let sprites = vec![Sprite::invisible(); size as usize];
        SpriteMap { 
            sprites
        }
    }

    // this will get a single corrsponding pixel
    pub fn get_sprite_byte(&self, addr: u32) -> u8 {
        return self.sprites[(addr / SPRITE_SIZE) as usize].pixels[(addr % SPRITE_SIZE) as usize];
    }

    pub fn set_sprite_byte(&mut self, addr: u32, data: u8) {
        self.sprites[(addr / SPRITE_SIZE) as usize].pixels[(addr % SPRITE_SIZE) as usize] = data;
    }

    // returns the either y or x coordinate of the sprite corresponding to the addr/4, addr%4
    pub fn get_sprite_reg(&self, addr: u32) -> u8 {
        let addr = addr as usize;
        let sprite = &self.sprites[addr / 4];
        if addr % 4 == 0 {
            return sprite.x.0;
        }
        else if addr % 4 == 1 {
            return sprite.x.1;
        } 
        else if addr % 4 == 2 {
            return sprite.y.0;
        }
        else {
            return sprite.y.1;
        }
    }

    // sets the either y or x coordinate of the sprite corresponding to the addr/4, addr%4
    pub fn set_sprite_reg(&mut self, addr: u32, data: u8) {
        let addr = addr as usize;
        let sprite = &mut self.sprites[addr / 4];
        if addr % 4 == 0 {
            sprite.x.0 = data;
        } 
        else if addr % 4 == 1 {
            sprite.x.1 = data;
        }
        else if addr % 4 == 2 {
            sprite.y.0 = data;
        } 
        else {
            sprite.y.1 = data;
        }
    }
}
