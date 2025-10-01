use std::collections::HashMap;
use std::collections::VecDeque;

use std::u16;
use std::io::{self, Write};
use std::sync::{Arc, RwLock};

// TODO: add SD card interface

pub const STACK_START : usize = 0x10000;

pub const FRAME_WIDTH: u32 = 1024;
pub const FRAME_HEIGHT: u32 = 512;
pub const TILE_SIZE: u32 = 8;
// const TILES_NUM: u32 = 128;
const TILE_DATA_SIZE: u32 = TILE_SIZE * TILE_SIZE * 2;
pub const SPRITE_SIZE: u32 = 32;
// const SPRITES_NUM: u32 = 8;
const SPRITE_DATA_SIZE: u32 = SPRITE_SIZE * SPRITE_SIZE * 2;

const PS2_STREAM : u32 = 0x20000;
const UART_TX : u32 = 0x20002;
pub const PIT_START : u32 = 0x20004;

const SD_SEND_BYTE : u32 = 0x201F9;
const SD_CMD_BUF : u32  = 0x201FA;
const SD_BUF_START : u32 = 0x20200;

const TILE_MAP_START : u32 = 0x2A000;
const TILE_MAP_SIZE : u32 = 0x4000;
const FRAME_BUFFER_START : u32 = 0x2E000;
const FRAME_BUFFER_SIZE : u32 = 0x1FD0;
const V_SCROLL_START : u32 = 0x2FFFE;
const H_SCROLL_START : u32 = 0x2FFFC;
const SCALE_REGISTER_START : u32 = 0x2FFFB; // each pixel is repeated 2^n times
const SPRITE_MAP_START : u32 = 0x26000;
const SPRITE_MAP_SIZE : u32 = 0x4000;
const SPRITE_REGISTERS_START : u32 = 0x2FFD0;  // every consecutive pair of words correspond to 
const SPRITE_REGISTERS_SIZE : u32 = 0x20;     // the y and x coordinates, respectively of a sprite

// TODO: make sd card its own struct
// put clock method in there

pub struct Memory {
  ram: HashMap<u32, u8>,   
  frame_buffer: Arc<RwLock<FrameBuffer>>,
  tile_map: Arc<RwLock<TileMap>>, 
  io_buffer: Arc<RwLock<VecDeque<u16>>>,
  vscroll_register: Arc<RwLock<(u8, u8)>>,
  hscroll_register: Arc<RwLock<(u8, u8)>>,
  scale_register: Arc<RwLock<u8>>,
  pit: Arc<RwLock<(u8, u8, u8, u8)>>,
  sprite_map: Arc<RwLock<SpriteMap>>,
  sd_card: Arc<RwLock<HashMap<u32, Vec<u8>>>>,
  pending_interrupt: Arc<RwLock<bool>>
}

// an 80x60 framebuffer of 8-bit tile values
pub struct FrameBuffer {
    pub width: u32, // number of tiles in the x direction
    pub height: u32, // number of tiles in the y direction
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

impl Memory {
    pub fn new(ram: HashMap<u32, u8>) -> Memory {

        Memory {
            ram,
            frame_buffer: Arc::new(RwLock::new(FrameBuffer::new(FRAME_WIDTH, FRAME_HEIGHT))),
            tile_map: Arc::new(RwLock::new(TileMap::new(TILE_MAP_SIZE))),
            io_buffer: Arc::new(RwLock::new(VecDeque::new())),
            vscroll_register: Arc::new(RwLock::new((0, 0))),
            hscroll_register: Arc::new(RwLock::new((0, 0))),
            scale_register: Arc::new(RwLock::new(0)),
            pit: Arc::new(RwLock::new((0, 0, 0, 0))),
            sprite_map: Arc::new(RwLock::new(SpriteMap::new(SPRITE_MAP_SIZE))),
            sd_card: Arc::new(RwLock::new(HashMap::new())),
            pending_interrupt: Arc::new(RwLock::new(false))
        }
    }

    pub fn get_frame_buffer(&self) -> Arc<RwLock<FrameBuffer>> { return Arc::clone(&self.frame_buffer)}
    pub fn get_tile_map(&self) -> Arc<RwLock<TileMap>> { return Arc::clone(&self.tile_map)}
    pub fn get_io_buffer(&self) -> Arc<RwLock<VecDeque<u16>>> { return Arc::clone(&self.io_buffer) }
    pub fn get_vscroll_register(&self) -> Arc<RwLock<(u8, u8)>> { return Arc::clone(&self.vscroll_register) }
    pub fn get_hscroll_register(&self) -> Arc<RwLock<(u8, u8)>> { return Arc::clone(&self.hscroll_register) }
    pub fn get_scale_register(&self) -> Arc<RwLock<u8>> { return Arc::clone(&self.scale_register) }
    pub fn get_sprite_map(&self) -> Arc<RwLock<SpriteMap>> { return Arc::clone(&self.sprite_map) }

    pub fn read(&mut self, addr: u32) -> u8 {
        if addr >= TILE_MAP_START && addr < TILE_MAP_START + TILE_MAP_SIZE {
            return self.tile_map.read().unwrap().get_tile_byte(addr - TILE_MAP_START);
        }
        if addr >= FRAME_BUFFER_START && addr < FRAME_BUFFER_START + FRAME_BUFFER_SIZE {
            return self.frame_buffer.read().unwrap().get_tile_pair(addr - FRAME_BUFFER_START);
        }
        if addr == PS2_STREAM {
            // kind of a hack but this assumed people always read a double from ps2 stream
            return self.io_buffer.write().unwrap().front().unwrap_or(&0).clone() as u8;
        }
        if addr == PS2_STREAM + 1 {
            // read of upper byte will cause a pop
            return (self.io_buffer.write().unwrap().pop_front().unwrap_or(0).clone() >> 8) as u8;
        }
        if addr >= SPRITE_MAP_START && addr < SPRITE_MAP_START + SPRITE_MAP_SIZE {
            return self.sprite_map.read().unwrap().get_sprite_byte(addr - SPRITE_MAP_START);
        }
        if addr >= SPRITE_REGISTERS_START && addr < SPRITE_REGISTERS_START + SPRITE_REGISTERS_SIZE {
            return self.sprite_map.read().unwrap().get_sprite_reg((addr - SPRITE_REGISTERS_START) as u32);
        }
        if addr == V_SCROLL_START {
            return self.vscroll_register.read().unwrap().0;
        }
        if addr == V_SCROLL_START + 1 {
            return self.vscroll_register.read().unwrap().1;
        }
        if addr == H_SCROLL_START {
            return self.hscroll_register.read().unwrap().0;
        }
        if addr == H_SCROLL_START + 1 {
            return self.hscroll_register.read().unwrap().1;
        }
        if addr == SCALE_REGISTER_START {
            return *self.scale_register.read().unwrap();
        }
        if addr == UART_TX {
            panic!("attempting to read output port (address {:X})", UART_TX);
        }
        if addr == PIT_START {
            return self.pit.read().unwrap().0;
        }
        if addr == PIT_START + 1 {
            return self.pit.read().unwrap().1;
        }
        if addr == PIT_START + 2 {
            return self.pit.read().unwrap().2;
        }
        if addr == PIT_START + 3 {
            return self.pit.read().unwrap().3;
        }

        if self.ram.contains_key(&addr) {
            return self.ram[&addr];
        } else {
            return 0;
        }
    }

    pub fn write(&mut self, addr: u32, data: u8) {
        if addr >= TILE_MAP_START && addr < TILE_MAP_START + TILE_MAP_SIZE {
            self.tile_map.write().unwrap().set_tile_byte((addr - TILE_MAP_START) as u32, data);
        }
        if addr >= FRAME_BUFFER_START && addr < FRAME_BUFFER_START + FRAME_BUFFER_SIZE {
            self.frame_buffer.write().unwrap().set_tile_pair((addr - FRAME_BUFFER_START) as u32, data);
        }
        if addr == PS2_STREAM {
            self.io_buffer.write().unwrap().pop_front();
        }
        if addr == UART_TX {
            print!("{}", data as char);
            io::stdout().flush().unwrap();
        }
        if addr == V_SCROLL_START {
            self.vscroll_register.write().unwrap().0 = data;
        }
        if addr == V_SCROLL_START + 1 {
            self.vscroll_register.write().unwrap().1 = data;
        }
        if addr == H_SCROLL_START {
            self.hscroll_register.write().unwrap().0 = data;
        }
        if addr == H_SCROLL_START + 1 {
            self.hscroll_register.write().unwrap().1 = data;
        }
        if addr == SCALE_REGISTER_START {
            *self.scale_register.write().unwrap() = data;
        }
        if addr >= SPRITE_MAP_START && addr < SPRITE_MAP_START + SPRITE_MAP_SIZE {
            self.sprite_map.write().unwrap().set_sprite_byte((addr - SPRITE_MAP_START) as u32, data);
        }
        if addr >= SPRITE_REGISTERS_START && addr < SPRITE_REGISTERS_START + SPRITE_REGISTERS_SIZE {
            self.sprite_map.write().unwrap().set_sprite_reg((addr - SPRITE_REGISTERS_START) as u32, data);
        }
        if addr == PIT_START {
            self.pit.write().unwrap().0 = data;
        }
        if addr == PIT_START + 1 {
            self.pit.write().unwrap().1 = data;
        }
        if addr == PIT_START + 2 {
            self.pit.write().unwrap().2 = data;
        }
        if addr == PIT_START + 3 {
            self.pit.write().unwrap().3 = data;
        }
        if addr == 0 {
            println!("Writing to address 0x0000: 0x{:04X}", data);
        }
        self.ram.insert(addr, data);
    }

    pub fn clock() {
        // do stuff that should happen every clock cycle
        
    }

    pub fn check_interrupts(&self) -> bool {
        let result = self.pending_interrupt.read().unwrap();
        // clear interrupt
        *self.pending_interrupt.write().unwrap() = false;
        return *result;
    }
}

impl FrameBuffer {
    pub fn new(frame_width: u32, frame_height: u32) -> Self {
        let width = frame_width / TILE_SIZE;
        //let width = 128;
        // TODO: think about this
        let height = frame_height / TILE_SIZE;
        FrameBuffer {
            width,
            height,
            tile_ptrs: vec![0; (width * height) as usize],
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

    pub fn get_tile(&self, x: u32, y: u32) -> u8 {
        if x < self.width && y < self.height {
            let idx: usize = (x + y * self.width) as usize;
            return self.tile_ptrs[idx];
        } else {
            panic!("Tile coordinates out of bounds");
        }
    }
}

impl Tile {
    pub fn black() -> Tile {
        Tile {
            pixels: vec![0; TILE_DATA_SIZE as usize]
        }
    }
    pub fn white() -> Tile {
        Tile {
            pixels: vec![0xff; TILE_DATA_SIZE as usize]
        }
    }
}

impl TileMap {
    pub fn new(size: u32) -> TileMap {
        let tiles = vec![Tile::black(); size as usize];
        TileMap { 
            tiles
        }
    }

    pub fn get_tile_byte(&self, addr: u32) -> u8 {
        return self.tiles[(addr / TILE_DATA_SIZE) as usize].pixels[(addr % TILE_DATA_SIZE) as usize];
    }

    pub fn set_tile_byte(&mut self, addr: u32, data: u8) {
        self.tiles[(addr / TILE_DATA_SIZE) as usize].pixels[(addr % TILE_DATA_SIZE) as usize] = data;
    }
}

impl Sprite {
    pub fn invisible() -> Sprite {
        Sprite {
            x: (50, 0),
            y: (50, 0),
            pixels: vec![0xFF; SPRITE_DATA_SIZE as usize],
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
        return self.sprites[(addr / SPRITE_DATA_SIZE) as usize].pixels[(addr % SPRITE_DATA_SIZE) as usize];
    }

    pub fn set_sprite_byte(&mut self, addr: u32, data: u8) {
        self.sprites[(addr / SPRITE_DATA_SIZE) as usize].pixels[(addr % SPRITE_DATA_SIZE) as usize] = data;
    }

    // returns the either y or x coordinate of the sprite corresponding to the addr/4, addr%4
    pub fn get_sprite_reg(&self, addr: u32) -> u8 {
        let sprite = &self.sprites[(addr / 4) as usize];
        if addr % 4 == 0 {
            return sprite.x.0;
        }
        if addr % 4 == 1 {
            return sprite.x.1;
        } 
        if addr % 4 == 2 {
            return sprite.y.0;
        }
        else {
            return sprite.y.1;
        }
    }

    // sets the either y or x coordinate of the sprite corresponding to the addr/4, addr%4
    pub fn set_sprite_reg(&mut self, addr: u32, data: u8) {
        let sprite = &mut self.sprites[(addr / 4) as usize];
        if addr % 4 == 0 {
            sprite.x.0 = data;
        } 
        if addr % 4 == 1 {
            sprite.x.1 = data;
        }
        if addr % 4 == 2 {
            sprite.y.0 = data;
        } 
        else {
            sprite.y.1 = data;
        }
    }
}