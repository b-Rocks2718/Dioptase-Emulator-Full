use std::collections::HashMap;
use std::collections::VecDeque;

use std::u16;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, RwLock};

pub const PHYSMEM_MAX: u32 = 0x7FFFFFF;

pub const FRAME_WIDTH: u32 = 640;
pub const FRAME_HEIGHT: u32 = 480;
pub const TILE_WIDTH: u32 = 8;
pub const PIXEL_FRAME_WIDTH: u32 = FRAME_WIDTH / 2;
pub const PIXEL_FRAME_HEIGHT: u32 = FRAME_HEIGHT / 2;
// const TILES_NUM: u32 = 256;
const TILE_SIZE: u32 = TILE_WIDTH * TILE_WIDTH * 2;
pub const SPRITE_WIDTH: u32 = 32;
// const SPRITES_NUM: u32 = 16;
const SPRITE_SIZE: u32 = SPRITE_WIDTH * SPRITE_WIDTH * 2;

// SD card DMA engine (no data buffer). DMA transfers 4 bytes per device tick.
const SD_BLOCK_SIZE: usize = 512;
const SD_DMA_BYTES_PER_TICK: u32 = 4;
pub const SD_INTERRUPT_BIT: u32 = 1 << 3;
pub const SD2_INTERRUPT_BIT: u32 = 1 << 6;
pub const VGA_INTERRUPT_BIT: u32 = 1 << 4;

const PIXEL_FRAME_BUFFER_START: u32 = 0x7FC0000;
const PIXEL_FRAME_BUFFER_SIZE: u32 = PIXEL_FRAME_WIDTH * PIXEL_FRAME_HEIGHT * 2;
const TILE_FRAME_BUFFER_WIDTH_TILES: u32 = FRAME_WIDTH / TILE_WIDTH;
const TILE_FRAME_BUFFER_HEIGHT_TILES: u32 = FRAME_HEIGHT / TILE_WIDTH;
// Two bytes per tile entry (index + color) in an 80x60 grid.
const TILE_FRAME_BUFFER_SIZE: u32 =
    TILE_FRAME_BUFFER_WIDTH_TILES * TILE_FRAME_BUFFER_HEIGHT_TILES * 2;
// Align the tile framebuffer to the 4KB page size for TLB mappings.
const TILE_FRAME_BUFFER_START: u32 =
    (PIXEL_FRAME_BUFFER_START - TILE_FRAME_BUFFER_SIZE) & !0xFFF;
const IO_START: u32 = TILE_FRAME_BUFFER_START;

const PS2_STREAM : u32 = 0x7FE5800;
const UART_TX : u32 = 0x7FE5802;
const UART_RX : u32 = 0x7FE5803;
pub const PIT_START : u32 = 0x7FE5804;

const SD_DMA_MEM_ADDR: u32 = 0x7FE5810;
const SD2_DMA_MEM_ADDR: u32 = 0x7FE5828;

const SD_DMA_OFFSET_MEM_ADDR: u32 = 0x0;
const SD_DMA_OFFSET_SD_BLOCK: u32 = 0x4;
const SD_DMA_OFFSET_LEN: u32 = 0x8;
const SD_DMA_OFFSET_CTRL: u32 = 0xC;
const SD_DMA_OFFSET_STATUS: u32 = 0x10;
const SD_DMA_OFFSET_ERR: u32 = 0x14;
const SD_DMA_RANGE_SIZE: u32 = 0x18;

const SD_DMA_CTRL_START: u32 = 1 << 0;
const SD_DMA_CTRL_DIR_RAM_TO_SD: u32 = 1 << 1;
const SD_DMA_CTRL_IRQ_ENABLE: u32 = 1 << 2;

const SD_DMA_STATUS_BUSY: u32 = 1 << 0;
const SD_DMA_STATUS_DONE: u32 = 1 << 1;
const SD_DMA_STATUS_ERR: u32 = 1 << 2;

const SD_DMA_ERR_NONE: u32 = 0;
const SD_DMA_ERR_BUSY: u32 = 1;
const SD_DMA_ERR_ZERO_LEN: u32 = 2;

const SPRITE_COUNT: u32 = 16;
const SPRITE_REGISTERS_START : u32 = 0x7FE5B00;  // every consecutive pair of words correspond to
const SPRITE_REGISTERS_SIZE : u32 = 0x40;     // the y and x coordinates, respectively of a sprite

const TILE_H_SCROLL_START: u32 = 0x7FE5B40;
const TILE_V_SCROLL_START: u32 = 0x7FE5B42;
const TILE_SCALE_REGISTER_START: u32 = 0x7FE5B44; // each tile pixel is repeated 2^n times

const PIXEL_H_SCROLL_START: u32 = 0x7FE5B50;
const PIXEL_V_SCROLL_START: u32 = 0x7FE5B52;
const PIXEL_SCALE_REGISTER_START: u32 = 0x7FE5B54; // each pixel is repeated 2^(n+1) times

const SPRITE_SCALE_START: u32 = 0x7FE5B60;
const SPRITE_SCALE_SIZE: u32 = SPRITE_COUNT;
const VGA_STATUS_REGISTER_START : u32 = 0x7FE5B46;
const VGA_FRAME_REGISTER_START : u32 = 0x7FE5B48;

pub const CLK_REG_START : u32 = 0x7FE5B4C;

const TILE_MAP_START : u32 = 0x7FE8000;
const TILE_MAP_SIZE : u32 = 0x8000;

const SPRITE_MAP_START : u32 = 0x7FF0000;
const SPRITE_MAP_SIZE : u32 = 0x8000;

pub struct Memory {
  ram: Mutex<HashMap<u32, u8>>,
  pixel_frame_buffer: Arc<RwLock<PixelFrameBuffer>>,
  tile_frame_buffer: Arc<RwLock<TileFrameBuffer>>,
  tile_map: Arc<RwLock<TileMap>>, 
  io_buffer: Arc<RwLock<VecDeque<u16>>>,
  tile_vscroll_register: Arc<RwLock<(u8, u8)>>,
  tile_hscroll_register: Arc<RwLock<(u8, u8)>>,
  pixel_vscroll_register: Arc<RwLock<(u8, u8)>>,
  pixel_hscroll_register: Arc<RwLock<(u8, u8)>>,
  tile_scale_register: Arc<RwLock<u8>>,
  pixel_scale_register: Arc<RwLock<u8>>,
  sprite_scale_registers: Arc<RwLock<Vec<u8>>>,
  vga_status_register: Arc<RwLock<u8>>,
  vga_frame_register: Arc<RwLock<(u8, u8, u8, u8)>>,
  clk_register: Arc<RwLock<(u8, u8, u8, u8)>>,
  pit: Arc<RwLock<(u8, u8, u8, u8)>>,
  pit_countdown: Arc<Mutex<u32>>,
  sprite_map: Arc<RwLock<SpriteMap>>,
  sd_card: Arc<RwLock<SdCard>>,
  sd_card2: Arc<RwLock<SdCard>>,
  pending_interrupt: Arc<RwLock<u32>>,
  use_uart_rx: bool
}

// Purpose: tile layer for the VGA output (two bytes per tile entry).
// Inputs/outputs: MMIO reads/writes map to raw bytes; rendering uses tile index + color.
// Invariants: entries length matches the MMIO-mapped byte size; width/height in tiles.
pub struct TileFrameBuffer {
    pub width_tiles: u32, // number of tiles in the x direction
    pub height_tiles: u32, // number of tiles in the y direction
    entries: Vec<u8>,
}

// Purpose: pixel layer for the VGA output (16-bit little-endian pixels).
// Inputs/outputs: MMIO reads/writes map to raw bytes; rendering reads u16 pixels.
// Invariants: byte length == width_pixels * height_pixels * 2.
pub struct PixelFrameBuffer {
    pub width_pixels: u32,
    pub height_pixels: u32,
    bytes: Vec<u8>,
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

// Purpose: identify which SD card device should receive a host image.
#[derive(Clone, Copy)]
pub enum SdSlot {
    Sd0,
    Sd1,
}

// Purpose: SD card storage indexed by block address, plus DMA register state.
// Inputs/outputs: storage is read/written by DMA; registers mirror MMIO state.
// Invariants: dma_remaining > 0 while dma_active is true; dma_status BUSY implies dma_active.
struct SdCard {
    storage: HashMap<u32, Vec<u8>>,
    dma_mem_addr: u32,
    dma_sd_block: u32,
    dma_len: u32,
    dma_ctrl: u32,
    dma_status: u32,
    dma_err: u32,
    dma_active: bool,
    dma_mem_cursor: u32,
    dma_sd_byte_cursor: u64,
    dma_remaining: u32,
    dma_ticks_per_word: u32,
    dma_tick_countdown: u32,
}

impl SdCard {
    fn new(dma_ticks_per_word: u32) -> Self {
        let ticks_per_word = dma_ticks_per_word.max(1);
        SdCard {
            storage: HashMap::new(),
            dma_mem_addr: 0,
            dma_sd_block: 0,
            dma_len: 0,
            dma_ctrl: 0,
            dma_status: 0,
            dma_err: SD_DMA_ERR_NONE,
            dma_active: false,
            dma_mem_cursor: 0,
            dma_sd_byte_cursor: 0,
            dma_remaining: 0,
            dma_ticks_per_word: ticks_per_word,
            dma_tick_countdown: 0,
        }
    }

    // Purpose: start a DMA transfer using the current register values.
    // Inputs: DMA registers already written by MMIO.
    // Outputs: updates DMA state and returns true if an immediate interrupt is needed.
    fn start_dma(&mut self) -> bool {
        let irq_enable = (self.dma_ctrl & SD_DMA_CTRL_IRQ_ENABLE) != 0;
        let is_busy = (self.dma_status & SD_DMA_STATUS_BUSY) != 0;
        if is_busy {
            self.dma_err = SD_DMA_ERR_BUSY;
            self.dma_status |= SD_DMA_STATUS_ERR;
            return false;
        }

        let mem_addr = self.dma_mem_addr & !0x3;
        let len = self.dma_len & !0x3;
        if len == 0 {
            self.dma_err = SD_DMA_ERR_ZERO_LEN;
            self.dma_status = SD_DMA_STATUS_DONE | SD_DMA_STATUS_ERR;
            self.dma_active = false;
            return irq_enable;
        }

        self.dma_mem_cursor = mem_addr;
        self.dma_sd_byte_cursor = (self.dma_sd_block as u64) * (SD_BLOCK_SIZE as u64);
        self.dma_remaining = len;
        self.dma_err = SD_DMA_ERR_NONE;
        self.dma_status = SD_DMA_STATUS_BUSY;
        self.dma_active = true;
        self.dma_tick_countdown = 0;
        false
    }

    // Purpose: clear DONE/ERR status and reset the error code.
    // Inputs/outputs: updates status bits and dma_err in-place.
    fn clear_status(&mut self) {
        self.dma_status &= !SD_DMA_STATUS_DONE;
        self.dma_status &= !SD_DMA_STATUS_ERR;
        self.dma_err = SD_DMA_ERR_NONE;
    }

    // Purpose: read a byte from SD storage without allocating missing blocks.
    // Inputs: byte_offset in SD address space.
    // Outputs: stored byte value or 0 if unmapped.
    fn read_storage_byte(&self, byte_offset: u64) -> u8 {
        let block_index = (byte_offset / (SD_BLOCK_SIZE as u64)) as u32;
        let block_offset = (byte_offset % (SD_BLOCK_SIZE as u64)) as usize;
        self.storage
            .get(&block_index)
            .and_then(|block| block.get(block_offset))
            .copied()
            .unwrap_or(0)
    }

    // Purpose: write a byte to SD storage, allocating blocks as needed.
    // Inputs: byte_offset in SD address space and value to store.
    // Outputs: updates storage contents.
    fn write_storage_byte(&mut self, byte_offset: u64, value: u8) {
        let block_index = (byte_offset / (SD_BLOCK_SIZE as u64)) as u32;
        let block_offset = (byte_offset % (SD_BLOCK_SIZE as u64)) as usize;
        let block = self
            .storage
            .entry(block_index)
            .or_insert_with(|| vec![0; SD_BLOCK_SIZE]);
        block[block_offset] = value;
    }

    // Purpose: load a raw SD image into storage starting at block 0.
    // Inputs: image bytes, where offset 0 corresponds to block 0 byte 0.
    // Outputs: storage is cleared and replaced with the provided image.
    fn load_image(&mut self, image: &[u8]) {
        self.storage.clear();
        for (index, chunk) in image.chunks(SD_BLOCK_SIZE).enumerate() {
            let mut block = vec![0u8; SD_BLOCK_SIZE];
            block[..chunk.len()].copy_from_slice(chunk);
            self.storage.insert(index as u32, block);
        }
    }
}

// Purpose: extract a little-endian register byte from a 32-bit value.
// Inputs: full register value, byte address, base register address.
// Outputs: the addressed byte.
fn read_reg_byte(value: u32, addr: u32, base: u32) -> u8 {
    let shift = ((addr - base) * 8) as u32;
    ((value >> shift) & 0xFF) as u8
}

// Purpose: update one byte of a 32-bit MMIO register in little-endian order.
// Inputs: register, byte address, base register address, and the new byte value.
// Outputs: updates the register in-place.
fn write_reg_byte(reg: &mut u32, addr: u32, base: u32, value: u8) {
    let shift = ((addr - base) * 8) as u32;
    let mask = 0xFFu32 << shift;
    *reg = (*reg & !mask) | ((value as u32) << shift);
}

// Purpose: read a byte from an SD DMA MMIO block.
// Inputs: address, base address, and SD card state.
// Outputs: Some(byte) if within the SD block, else None.
fn read_sd_dma_mmio(addr: u32, base: u32, sd: &SdCard) -> Option<u8> {
    if addr < base || addr >= base + SD_DMA_RANGE_SIZE {
        return None;
    }
    if addr >= base + SD_DMA_OFFSET_MEM_ADDR && addr < base + SD_DMA_OFFSET_MEM_ADDR + 4 {
        return Some(read_reg_byte(sd.dma_mem_addr, addr, base + SD_DMA_OFFSET_MEM_ADDR));
    }
    if addr >= base + SD_DMA_OFFSET_SD_BLOCK && addr < base + SD_DMA_OFFSET_SD_BLOCK + 4 {
        return Some(read_reg_byte(sd.dma_sd_block, addr, base + SD_DMA_OFFSET_SD_BLOCK));
    }
    if addr >= base + SD_DMA_OFFSET_LEN && addr < base + SD_DMA_OFFSET_LEN + 4 {
        return Some(read_reg_byte(sd.dma_len, addr, base + SD_DMA_OFFSET_LEN));
    }
    if addr >= base + SD_DMA_OFFSET_CTRL && addr < base + SD_DMA_OFFSET_CTRL + 4 {
        return Some(read_reg_byte(sd.dma_ctrl, addr, base + SD_DMA_OFFSET_CTRL));
    }
    if addr >= base + SD_DMA_OFFSET_STATUS && addr < base + SD_DMA_OFFSET_STATUS + 4 {
        let mut status = sd.dma_status;
        if sd.dma_err != SD_DMA_ERR_NONE {
            status |= SD_DMA_STATUS_ERR;
        } else {
            status &= !SD_DMA_STATUS_ERR;
        }
        return Some(read_reg_byte(status, addr, base + SD_DMA_OFFSET_STATUS));
    }
    Some(read_reg_byte(sd.dma_err, addr, base + SD_DMA_OFFSET_ERR))
}

impl Memory {
    pub fn new(ram: HashMap<u32, u8>, use_uart_rx: bool, sd_dma_ticks_per_word: u32) -> Memory {
        let ticks_per_word = sd_dma_ticks_per_word.max(1);

        Memory {
            ram: Mutex::new(ram),
            pixel_frame_buffer: Arc::new(RwLock::new(PixelFrameBuffer::new(
                PIXEL_FRAME_WIDTH,
                PIXEL_FRAME_HEIGHT,
                PIXEL_FRAME_BUFFER_SIZE,
            ))),
            tile_frame_buffer: Arc::new(RwLock::new(TileFrameBuffer::new(
                FRAME_WIDTH,
                FRAME_HEIGHT,
                TILE_FRAME_BUFFER_SIZE,
            ))),
            tile_map: Arc::new(RwLock::new(TileMap::new(TILE_MAP_SIZE))),
            io_buffer: Arc::new(RwLock::new(VecDeque::new())),
            tile_vscroll_register: Arc::new(RwLock::new((0, 0))),
            tile_hscroll_register: Arc::new(RwLock::new((0, 0))),
            pixel_vscroll_register: Arc::new(RwLock::new((0, 0))),
            pixel_hscroll_register: Arc::new(RwLock::new((0, 0))),
            tile_scale_register: Arc::new(RwLock::new(0)),
            pixel_scale_register: Arc::new(RwLock::new(0)),
            sprite_scale_registers: Arc::new(RwLock::new(vec![0; SPRITE_COUNT as usize])),
            vga_status_register: Arc::new(RwLock::new(0)),
            vga_frame_register: Arc::new(RwLock::new((0, 0, 0, 0))),
            clk_register: Arc::new(RwLock::new((0, 0, 0, 0))),
            pit: Arc::new(RwLock::new((0, 0, 0, 0))),
            pit_countdown: Arc::new(Mutex::new(0)),
            sprite_map: Arc::new(RwLock::new(SpriteMap::new(SPRITE_MAP_SIZE))),
            sd_card: Arc::new(RwLock::new(SdCard::new(ticks_per_word))),
            sd_card2: Arc::new(RwLock::new(SdCard::new(ticks_per_word))),
            pending_interrupt: Arc::new(RwLock::new(0)),
            use_uart_rx: use_uart_rx
        }
    }

    pub fn get_pixel_frame_buffer(&self) -> Arc<RwLock<PixelFrameBuffer>> {
        Arc::clone(&self.pixel_frame_buffer)
    }
    pub fn get_tile_frame_buffer(&self) -> Arc<RwLock<TileFrameBuffer>> {
        Arc::clone(&self.tile_frame_buffer)
    }
    pub fn get_tile_map(&self) -> Arc<RwLock<TileMap>> { return Arc::clone(&self.tile_map)}
    pub fn get_io_buffer(&self) -> Arc<RwLock<VecDeque<u16>>> { return Arc::clone(&self.io_buffer) }
    pub fn get_tile_vscroll_register(&self) -> Arc<RwLock<(u8, u8)>> {
        Arc::clone(&self.tile_vscroll_register)
    }
    pub fn get_tile_hscroll_register(&self) -> Arc<RwLock<(u8, u8)>> {
        Arc::clone(&self.tile_hscroll_register)
    }
    pub fn get_pixel_vscroll_register(&self) -> Arc<RwLock<(u8, u8)>> {
        Arc::clone(&self.pixel_vscroll_register)
    }
    pub fn get_pixel_hscroll_register(&self) -> Arc<RwLock<(u8, u8)>> {
        Arc::clone(&self.pixel_hscroll_register)
    }
    pub fn get_tile_scale_register(&self) -> Arc<RwLock<u8>> {
        Arc::clone(&self.tile_scale_register)
    }
    pub fn get_pixel_scale_register(&self) -> Arc<RwLock<u8>> {
        Arc::clone(&self.pixel_scale_register)
    }
    pub fn get_sprite_scale_registers(&self) -> Arc<RwLock<Vec<u8>>> {
        Arc::clone(&self.sprite_scale_registers)
    }
    pub fn get_sprite_map(&self) -> Arc<RwLock<SpriteMap>> { return Arc::clone(&self.sprite_map) }
    pub fn get_vga_status_register(&self) -> Arc<RwLock<u8>> { return Arc::clone(&self.vga_status_register) }
    pub fn get_vga_frame_register(&self) -> Arc<RwLock<(u8, u8, u8, u8)>> { return Arc::clone(&self.vga_frame_register) }
    pub fn get_pending_interrupt(&self) -> Arc<RwLock<u32>> { return Arc::clone(&self.pending_interrupt) }

    pub fn read(&self, addr: u32) -> u8 {
        let mut ram = self.ram.lock().unwrap();
        self.read_internal(addr, &mut ram)
    }

    pub fn read_u16(&self, addr: u32) -> u16 {
        let addr = addr & 0xFFFFFFFE;
        let mut ram = self.ram.lock().unwrap();
        let lo = self.read_internal(addr, &mut ram);
        let hi = self.read_internal(addr + 1, &mut ram);
        (u16::from(hi) << 8) | u16::from(lo)
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        let addr = addr & 0xFFFFFFFC;
        let mut ram = self.ram.lock().unwrap();
        self.read_u32_internal(addr, &mut ram)
    }

    // Read specific physical addresses under one lock to avoid tearing.
    pub fn read_phys_bytes(&self, addrs: &[u32], out: &mut [u8]) {
        assert_eq!(addrs.len(), out.len());
        let mut ram = self.ram.lock().unwrap();
        for (slot, addr) in out.iter_mut().zip(addrs.iter()) {
            *slot = self.read_internal(*addr, &mut ram);
        }
    }

    pub fn atomic_swap_u32(&self, addr: u32, value: u32) -> u32 {
        let addr = addr & 0xFFFFFFFC;
        let mut ram = self.ram.lock().unwrap();
        let prev = self.read_u32_internal(addr, &mut ram);
        self.write_u32_internal(addr, value, &mut ram);
        prev
    }

    pub fn atomic_add_u32(&self, addr: u32, value: u32) -> u32 {
        let addr = addr & 0xFFFFFFFC;
        let mut ram = self.ram.lock().unwrap();
        let prev = self.read_u32_internal(addr, &mut ram);
        let next = u32::wrapping_add(prev, value);
        self.write_u32_internal(addr, next, &mut ram);
        prev
    }

    // Purpose: load a raw SD image into the selected SD device.
    // Inputs: slot selector and image bytes.
    // Outputs: replaces the SD storage contents for the chosen device.
    pub fn load_sd_image(&self, slot: SdSlot, image: &[u8]) {
        match slot {
            SdSlot::Sd0 => {
                let mut sd = self.sd_card.write().unwrap();
                sd.load_image(image);
            }
            SdSlot::Sd1 => {
                let mut sd = self.sd_card2.write().unwrap();
                sd.load_image(image);
            }
        }
    }

    // Purpose: handle SD DMA MMIO writes for a specific SD device.
    // Inputs: target address/data, base address, device handle, and interrupt bit.
    // Outputs: true if the address was handled, false otherwise.
    fn write_sd_dma_mmio(
        &self,
        addr: u32,
        data: u8,
        base: u32,
        sd: &Arc<RwLock<SdCard>>,
        interrupt_bit: u32,
    ) -> bool {
        if addr < base || addr >= base + SD_DMA_RANGE_SIZE {
            return false;
        }
        if addr >= base + SD_DMA_OFFSET_MEM_ADDR && addr < base + SD_DMA_OFFSET_MEM_ADDR + 4 {
            let mut sd = sd.write().unwrap();
            write_reg_byte(&mut sd.dma_mem_addr, addr, base + SD_DMA_OFFSET_MEM_ADDR, data);
            return true;
        }
        if addr >= base + SD_DMA_OFFSET_SD_BLOCK && addr < base + SD_DMA_OFFSET_SD_BLOCK + 4 {
            let mut sd = sd.write().unwrap();
            write_reg_byte(&mut sd.dma_sd_block, addr, base + SD_DMA_OFFSET_SD_BLOCK, data);
            return true;
        }
        if addr >= base + SD_DMA_OFFSET_LEN && addr < base + SD_DMA_OFFSET_LEN + 4 {
            let mut sd = sd.write().unwrap();
            write_reg_byte(&mut sd.dma_len, addr, base + SD_DMA_OFFSET_LEN, data);
            return true;
        }
        if addr >= base + SD_DMA_OFFSET_CTRL && addr < base + SD_DMA_OFFSET_CTRL + 4 {
            let mut sd = sd.write().unwrap();
            write_reg_byte(&mut sd.dma_ctrl, addr, base + SD_DMA_OFFSET_CTRL, data);
            sd.dma_ctrl &= SD_DMA_CTRL_START | SD_DMA_CTRL_DIR_RAM_TO_SD | SD_DMA_CTRL_IRQ_ENABLE;
            let should_start = (sd.dma_ctrl & SD_DMA_CTRL_START) != 0;
            if should_start {
                sd.dma_ctrl &= !SD_DMA_CTRL_START;
                let interrupt = sd.start_dma();
                if interrupt {
                    *self.pending_interrupt.write().unwrap() |= interrupt_bit;
                }
            }
            return true;
        }
        if addr >= base + SD_DMA_OFFSET_STATUS && addr < base + SD_DMA_OFFSET_STATUS + 4 {
            let mut sd = sd.write().unwrap();
            sd.clear_status();
            return true;
        }
        true
    }

    fn read_u32_internal(&self, addr: u32, ram: &mut HashMap<u32, u8>) -> u32 {
        let b0 = self.read_internal(addr, ram) as u32;
        let b1 = self.read_internal(addr + 1, ram) as u32;
        let b2 = self.read_internal(addr + 2, ram) as u32;
        let b3 = self.read_internal(addr + 3, ram) as u32;
        (b3 << 24) | (b2 << 16) | (b1 << 8) | b0
    }

    fn read_internal(&self, addr: u32, ram: &mut HashMap<u32, u8>) -> u8 {
        assert!(addr <= PHYSMEM_MAX, "Physical memory address out of bounds: 0x{:08X}", addr);

        if addr >= TILE_MAP_START && addr < TILE_MAP_START + TILE_MAP_SIZE {
            return self.tile_map.read().unwrap().get_tile_byte(addr - TILE_MAP_START);
        }
        else if addr >= TILE_FRAME_BUFFER_START && addr < TILE_FRAME_BUFFER_START + TILE_FRAME_BUFFER_SIZE {
            return self.tile_frame_buffer.read().unwrap().get_byte(addr - TILE_FRAME_BUFFER_START);
        }
        else if addr >= PIXEL_FRAME_BUFFER_START && addr < PIXEL_FRAME_BUFFER_START + PIXEL_FRAME_BUFFER_SIZE {
            return self.pixel_frame_buffer.read().unwrap().get_byte(addr - PIXEL_FRAME_BUFFER_START);
        }
        else if addr >= SD_DMA_MEM_ADDR && addr < SD_DMA_MEM_ADDR + SD_DMA_RANGE_SIZE {
            let sd = self.sd_card.read().unwrap();
            return read_sd_dma_mmio(addr, SD_DMA_MEM_ADDR, &sd).unwrap_or(0);
        }
        else if addr >= SD2_DMA_MEM_ADDR && addr < SD2_DMA_MEM_ADDR + SD_DMA_RANGE_SIZE {
            let sd = self.sd_card2.read().unwrap();
            return read_sd_dma_mmio(addr, SD2_DMA_MEM_ADDR, &sd).unwrap_or(0);
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
        else if addr == TILE_V_SCROLL_START {
            return self.tile_vscroll_register.read().unwrap().0;
        }
        else if addr == TILE_V_SCROLL_START + 1 {
            return self.tile_vscroll_register.read().unwrap().1;
        }
        else if addr == TILE_H_SCROLL_START {
            return self.tile_hscroll_register.read().unwrap().0;
        }
        else if addr == TILE_H_SCROLL_START + 1 {
            return self.tile_hscroll_register.read().unwrap().1;
        }
        else if addr == TILE_SCALE_REGISTER_START {
            return *self.tile_scale_register.read().unwrap();
        }
        else if addr == PIXEL_V_SCROLL_START {
            return self.pixel_vscroll_register.read().unwrap().0;
        }
        else if addr == PIXEL_V_SCROLL_START + 1 {
            return self.pixel_vscroll_register.read().unwrap().1;
        }
        else if addr == PIXEL_H_SCROLL_START {
            return self.pixel_hscroll_register.read().unwrap().0;
        }
        else if addr == PIXEL_H_SCROLL_START + 1 {
            return self.pixel_hscroll_register.read().unwrap().1;
        }
        else if addr == PIXEL_SCALE_REGISTER_START {
            return *self.pixel_scale_register.read().unwrap();
        }
        else if addr >= SPRITE_SCALE_START && addr < SPRITE_SCALE_START + SPRITE_SCALE_SIZE {
            let idx = (addr - SPRITE_SCALE_START) as usize;
            return self.sprite_scale_registers.read().unwrap()[idx];
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
        else if addr == CLK_REG_START {
            return self.clk_register.read().unwrap().0;
        }
        else if addr == CLK_REG_START + 1 {
            return self.clk_register.read().unwrap().1;
        }
        else if addr == CLK_REG_START + 2 {
            return self.clk_register.read().unwrap().2;
        }
        else if addr == CLK_REG_START + 3 {
            return self.clk_register.read().unwrap().3;
        }
        else if addr == 0 {
            println!("Warning: reading from physical address 0x00000000");
        }

        if addr >= IO_START {
            panic!("read from unmapped IO address 0x{:08X}", addr);
        }

        ram.get(&addr).copied().unwrap_or(0)
    }

    pub fn write(&self, addr: u32, data: u8) {
        let mut ram = self.ram.lock().unwrap();
        self.write_internal(addr, data, &mut ram);
    }

    pub fn write_u16(&self, addr: u32, data: u16) {
        let addr = addr & 0xFFFFFFFE;
        let mut ram = self.ram.lock().unwrap();
        self.write_internal(addr, data as u8, &mut ram);
        self.write_internal(addr + 1, (data >> 8) as u8, &mut ram);
    }

    pub fn write_u32(&self, addr: u32, data: u32) {
        let addr = addr & 0xFFFFFFFC;
        let mut ram = self.ram.lock().unwrap();
        self.write_u32_internal(addr, data, &mut ram);
    }

    // Write specific physical addresses under one lock to avoid tearing.
    pub fn write_phys_bytes(&self, addrs: &[u32], data: &[u8]) {
        assert_eq!(addrs.len(), data.len());
        let mut ram = self.ram.lock().unwrap();
        for (addr, byte) in addrs.iter().zip(data.iter()) {
            self.write_internal(*addr, *byte, &mut ram);
        }
    }

    fn write_u32_internal(&self, addr: u32, data: u32, ram: &mut HashMap<u32, u8>) {
        self.write_internal(addr, data as u8, ram);
        self.write_internal(addr + 1, (data >> 8) as u8, ram);
        self.write_internal(addr + 2, (data >> 16) as u8, ram);
        self.write_internal(addr + 3, (data >> 24) as u8, ram);
    }

    fn write_internal(&self, addr: u32, data: u8, ram: &mut HashMap<u32, u8>) {
        assert!(addr <= PHYSMEM_MAX, "Physical memory address out of bounds: 0x{:08X}", addr);

        let mut handled = false;

        if addr >= TILE_MAP_START && addr < TILE_MAP_START + TILE_MAP_SIZE {
            self.tile_map
                .write()
                .unwrap()
                .set_tile_byte((addr - TILE_MAP_START) as u32, data);
            handled = true;
        }
        else if addr >= TILE_FRAME_BUFFER_START && addr < TILE_FRAME_BUFFER_START + TILE_FRAME_BUFFER_SIZE {
            self.tile_frame_buffer
                .write()
                .unwrap()
                .set_byte((addr - TILE_FRAME_BUFFER_START) as u32, data);
            handled = true;
        }
        else if addr >= PIXEL_FRAME_BUFFER_START && addr < PIXEL_FRAME_BUFFER_START + PIXEL_FRAME_BUFFER_SIZE {
            self.pixel_frame_buffer
                .write()
                .unwrap()
                .set_byte((addr - PIXEL_FRAME_BUFFER_START) as u32, data);
            handled = true;
        }
        else if self.write_sd_dma_mmio(
            addr,
            data,
            SD_DMA_MEM_ADDR,
            &self.sd_card,
            SD_INTERRUPT_BIT,
        ) {
            return;
        }
        else if self.write_sd_dma_mmio(
            addr,
            data,
            SD2_DMA_MEM_ADDR,
            &self.sd_card2,
            SD2_INTERRUPT_BIT,
        ) {
            return;
        }
        else if addr == PS2_STREAM {
            panic!("attempting to write input port (address {:X})", PS2_STREAM);
        }
        else if addr == UART_TX {
            print!("{}", data as char);
            io::stdout().flush().unwrap();
            handled = true;
        }
        else if addr == UART_RX {
            panic!("attempting to write input port (address {:X})", UART_RX);
        }
        else if addr == TILE_V_SCROLL_START {
            self.tile_vscroll_register.write().unwrap().0 = data;
            handled = true;
        }
        else if addr == TILE_V_SCROLL_START + 1 {
            self.tile_vscroll_register.write().unwrap().1 = data;
            handled = true;
        }
        else if addr == TILE_H_SCROLL_START {
            self.tile_hscroll_register.write().unwrap().0 = data;
            handled = true;
        }
        else if addr == TILE_H_SCROLL_START + 1 {
            self.tile_hscroll_register.write().unwrap().1 = data;
            handled = true;
        }
        else if addr == TILE_SCALE_REGISTER_START {
            *self.tile_scale_register.write().unwrap() = data;
            handled = true;
        }
        else if addr == PIXEL_V_SCROLL_START {
            self.pixel_vscroll_register.write().unwrap().0 = data;
            handled = true;
        }
        else if addr == PIXEL_V_SCROLL_START + 1 {
            self.pixel_vscroll_register.write().unwrap().1 = data;
            handled = true;
        }
        else if addr == PIXEL_H_SCROLL_START {
            self.pixel_hscroll_register.write().unwrap().0 = data;
            handled = true;
        }
        else if addr == PIXEL_H_SCROLL_START + 1 {
            self.pixel_hscroll_register.write().unwrap().1 = data;
            handled = true;
        }
        else if addr == PIXEL_SCALE_REGISTER_START {
            *self.pixel_scale_register.write().unwrap() = data;
            handled = true;
        }
        else if addr >= SPRITE_SCALE_START && addr < SPRITE_SCALE_START + SPRITE_SCALE_SIZE {
            let idx = (addr - SPRITE_SCALE_START) as usize;
            self.sprite_scale_registers.write().unwrap()[idx] = data;
            handled = true;
        }
        else if addr >= SPRITE_MAP_START && addr < SPRITE_MAP_START + SPRITE_MAP_SIZE {
            self.sprite_map.write().unwrap().set_sprite_byte((addr - SPRITE_MAP_START) as u32, data);
            handled = true;
        }
        else if addr >= SPRITE_REGISTERS_START && addr < SPRITE_REGISTERS_START + SPRITE_REGISTERS_SIZE {
            self.sprite_map.write().unwrap().set_sprite_reg((addr - SPRITE_REGISTERS_START) as u32, data);
            handled = true;
        }
        else if addr == PIT_START {
            self.pit.write().unwrap().0 = data;
            handled = true;
        }
        else if addr == PIT_START + 1 {
            self.pit.write().unwrap().1 = data;
            handled = true;
        }
        else if addr == PIT_START + 2 {
            self.pit.write().unwrap().2 = data;
            handled = true;
        }
        else if addr == PIT_START + 3 {
            self.pit.write().unwrap().3 = data;
            handled = true;
        }
        else if addr == CLK_REG_START {
            self.clk_register.write().unwrap().0 = data;
            handled = true;
        }
        else if addr == CLK_REG_START + 1 {
            self.clk_register.write().unwrap().1 = data;
            handled = true;
        }
        else if addr == CLK_REG_START + 2 {
            self.clk_register.write().unwrap().2 = data;
            handled = true;
        }
        else if addr == CLK_REG_START + 3 {
            self.clk_register.write().unwrap().3 = data;
            handled = true;
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

        if addr >= IO_START && !handled {
            panic!("write to unmapped IO address 0x{:08X}", addr);
        }

        ram.insert(addr, data);
    }

    // Purpose: advance the SD DMA engines by one device tick.
    // Inputs: none (uses DMA register state and SD storage).
    // Outputs: updates RAM/storage and may raise SD interrupts.
    pub fn tick_sd_dma(&self) {
        self.tick_sd_dma_device(&self.sd_card, SD_INTERRUPT_BIT);
        self.tick_sd_dma_device(&self.sd_card2, SD2_INTERRUPT_BIT);
    }

    // Purpose: advance one SD DMA engine by one device tick.
    // Inputs: SD device handle and interrupt bit.
    // Outputs: updates RAM/storage and may raise the device interrupt.
    fn tick_sd_dma_device(&self, sd: &Arc<RwLock<SdCard>>, interrupt_bit: u32) {
        let (mem_addr, sd_offset, bytes, dir_ram_to_sd, done_after, irq_enable) = {
            let mut sd = sd.write().unwrap();
            if !sd.dma_active {
                return;
            }
            if sd.dma_tick_countdown > 0 {
                sd.dma_tick_countdown -= 1;
                return;
            }
            sd.dma_tick_countdown = sd.dma_ticks_per_word.saturating_sub(1);
            let bytes = if sd.dma_remaining < SD_DMA_BYTES_PER_TICK {
                sd.dma_remaining
            } else {
                SD_DMA_BYTES_PER_TICK
            };
            let mem_addr = sd.dma_mem_cursor;
            let sd_offset = sd.dma_sd_byte_cursor;
            let dir_ram_to_sd = (sd.dma_ctrl & SD_DMA_CTRL_DIR_RAM_TO_SD) != 0;
            sd.dma_mem_cursor = sd.dma_mem_cursor.wrapping_add(bytes);
            sd.dma_sd_byte_cursor = sd.dma_sd_byte_cursor.wrapping_add(bytes as u64);
            sd.dma_remaining = sd.dma_remaining.wrapping_sub(bytes);
            let done_after = sd.dma_remaining == 0;
            if done_after {
                sd.dma_active = false;
                sd.dma_status &= !SD_DMA_STATUS_BUSY;
                sd.dma_status |= SD_DMA_STATUS_DONE;
                if sd.dma_err != SD_DMA_ERR_NONE {
                    sd.dma_status |= SD_DMA_STATUS_ERR;
                }
            }
            let irq_enable = (sd.dma_ctrl & SD_DMA_CTRL_IRQ_ENABLE) != 0;
            (mem_addr, sd_offset, bytes, dir_ram_to_sd, done_after, irq_enable)
        };

        if bytes == 0 {
            return;
        }

        if dir_ram_to_sd {
            let mut buf = [0u8; SD_DMA_BYTES_PER_TICK as usize];
            {
                let mut ram = self.ram.lock().unwrap();
                for i in 0..bytes {
                    buf[i as usize] = self.read_internal(mem_addr + i, &mut ram);
                }
            }
            let mut sd = sd.write().unwrap();
            for i in 0..bytes {
                sd.write_storage_byte(sd_offset + i as u64, buf[i as usize]);
            }
        } else {
            let mut buf = [0u8; SD_DMA_BYTES_PER_TICK as usize];
            {
                let sd = sd.read().unwrap();
                for i in 0..bytes {
                    buf[i as usize] = sd.read_storage_byte(sd_offset + i as u64);
                }
            }
            let mut ram = self.ram.lock().unwrap();
            for i in 0..bytes {
                self.write_internal(mem_addr + i, buf[i as usize], &mut ram);
            }
        }

        if done_after && irq_enable {
            *self.pending_interrupt.write().unwrap() |= interrupt_bit;
        }
    }

    // Purpose: advance the shared PIT countdown by one core-0 tick.
    // Inputs: none.
    // Outputs: true if a timer interrupt should be raised this tick.
    pub fn tick_pit(&self) -> bool {
        let mut countdown = self.pit_countdown.lock().unwrap();
        if *countdown == 0 {
            let reload = self.read_u32(PIT_START);
            if reload != 0 {
                *countdown = reload;
                return true;
            }
        } else {
            *countdown -= 1;
        }
        false
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

impl TileFrameBuffer {
    // Purpose: initialize the tile framebuffer with a fixed MMIO byte size.
    // Inputs: screen dimensions in pixels and the total MMIO byte size.
    // Outputs: a zeroed tile buffer; panics if the buffer is too small.
    pub fn new(width_pixels: u32, height_pixels: u32, size_bytes: u32) -> Self {
        let width_tiles = width_pixels / TILE_WIDTH;
        let height_tiles = height_pixels / TILE_WIDTH;
        let tiles_needed = width_tiles * height_tiles;
        let bytes_needed = tiles_needed * 2;
        assert!(
            size_bytes == bytes_needed,
            "Tile framebuffer size mismatch: expected {} bytes, got {}",
            bytes_needed,
            size_bytes
        );
        TileFrameBuffer {
            width_tiles,
            height_tiles,
            entries: vec![0; size_bytes as usize],
        }
    }

    // Purpose: store one MMIO byte into the tile framebuffer backing store.
    // Inputs: byte offset and value.
    // Outputs: updates tile_indices at the given offset.
    pub fn set_byte(&mut self, offset: u32, value: u8) {
        if offset < self.entries.len() as u32 {
            self.entries[offset as usize] = value;
        } else {
            panic!("Tile framebuffer offset out of bounds: {}", offset);
        }
    }

    // Purpose: read one MMIO byte from the tile framebuffer backing store.
    // Inputs: byte offset.
    // Outputs: stored byte value at the given offset.
    pub fn get_byte(&self, offset: u32) -> u8 {
        if offset < self.entries.len() as u32 {
            self.entries[offset as usize]
        } else {
            panic!("Tile framebuffer offset out of bounds: {}", offset);
        }
    }

    // Purpose: fetch the tile index at a tile coordinate.
    // Inputs: tile-space coordinates.
    // Outputs: 8-bit tile index for the tilemap lookup.
    // Purpose: fetch the tile entry (index + color) at a tile coordinate.
    // Inputs: tile-space coordinates.
    // Outputs: (tile index, color byte).
    pub fn get_tile_entry(&self, x: u32, y: u32) -> (u8, u8) {
        if x < self.width_tiles && y < self.height_tiles {
            let idx: usize = (x + y * self.width_tiles) as usize;
            let entry_offset = idx * 2;
            let tile_index = self.entries[entry_offset];
            let tile_color = self.entries[entry_offset + 1];
            (tile_index, tile_color)
        } else {
            panic!("Tile coordinates out of bounds: ({}, {})", x, y);
        }
    }
}

impl PixelFrameBuffer {
    // Purpose: initialize the pixel framebuffer with a fixed MMIO byte size.
    // Inputs: logical pixel dimensions and the total MMIO byte size.
    // Outputs: a zeroed pixel buffer; panics if the size doesn't match.
    pub fn new(width_pixels: u32, height_pixels: u32, size_bytes: u32) -> Self {
        let expected = width_pixels * height_pixels * 2;
        assert!(
            size_bytes == expected,
            "Pixel framebuffer size mismatch: expected {} bytes, got {}",
            expected,
            size_bytes
        );
        PixelFrameBuffer {
            width_pixels,
            height_pixels,
            bytes: vec![0; size_bytes as usize],
        }
    }

    // Purpose: store one MMIO byte into the pixel framebuffer backing store.
    // Inputs: byte offset and value.
    // Outputs: updates bytes at the given offset.
    pub fn set_byte(&mut self, offset: u32, value: u8) {
        if offset < self.bytes.len() as u32 {
            self.bytes[offset as usize] = value;
        } else {
            panic!("Pixel framebuffer offset out of bounds: {}", offset);
        }
    }

    // Purpose: read one MMIO byte from the pixel framebuffer backing store.
    // Inputs: byte offset.
    // Outputs: stored byte value at the given offset.
    pub fn get_byte(&self, offset: u32) -> u8 {
        if offset < self.bytes.len() as u32 {
            self.bytes[offset as usize]
        } else {
            panic!("Pixel framebuffer offset out of bounds: {}", offset);
        }
    }

    // Purpose: fetch the 16-bit pixel at a logical pixel coordinate.
    // Inputs: pixel-space coordinates.
    // Outputs: packed 12-bit RGB value stored in 16 bits (little-endian).
    pub fn get_pixel(&self, x: u32, y: u32) -> u16 {
        if x < self.width_pixels && y < self.height_pixels {
            let idx: usize = (x + y * self.width_pixels) as usize;
            ((u16::from(self.bytes[2 * idx + 1])) << 8) | u16::from(self.bytes[2 * idx])
        } else {
            panic!("Pixel coordinates out of bounds: ({}, {})", x, y);
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
