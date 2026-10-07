// Guest physical memory: RAM below IO_START plus the MMIO devices described
// in docs/mem_map.md (VGA, PS/2 and UART, PIT, SD DMA, audio, clock divider).
//
// Concurrency model (sequentially consistent, per AGENTS.md):
// - RAM is an array of SeqCst `AtomicU32` words; guest byte i of a word is
//   bits 8i..8i+7 regardless of host endianness. Aligned word accesses are
//   single loads/stores, narrower stores are a compare-and-swap on the
//   containing word, so no access tears and cores never contend on a lock.
//   CPU accesses are naturally aligned, so none spans two words.
// - Every guest-visible MMIO access holds `mmio_lock` for its whole duration
//   (`mmio_transaction`), so multi-byte register accesses are not torn and
//   device side effects (PS/2 pops, DMA command strobes) are serialized.
// - Registers read every tick without `mmio_lock` (PIT reload, clock divider)
//   are atomics that each MMIO transaction updates with a single store, so
//   lock-free readers never see a partially written value.
// - The graphics thread reads `VgaState` concurrently. It only writes the
//   VGA status/frame registers, which are read-only to the guest; every other
//   VGA register is written only by guest MMIO under `mmio_lock`.
// - Device tick state that only core 0 advances (PIT countdown, audio sample
//   countdown) lives in atomics so the per-tick fast path takes no locks.
// All atomics use SeqCst.

use std::collections::{HashMap, VecDeque};
use std::convert::TryFrom;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU32, Ordering};
use std::sync::{Mutex, RwLock};

use crate::mouse::MouseQueue;

pub const PHYSMEM_MAX: u32 = 0x7FF_FFFF;

// ---- Device interrupt lines raised by memory-mapped devices -----------------
pub const SD_INTERRUPT_BIT: u32 = 1 << 3;
pub const VGA_INTERRUPT_BIT: u32 = 1 << 4;
pub const SD2_INTERRUPT_BIT: u32 = 1 << 6;
pub const AUDIO_INTERRUPT_BIT: u32 = 1 << 7;

// ---- VGA geometry ------------------------------------------------------------
pub const FRAME_WIDTH: u32 = 640;
pub const FRAME_HEIGHT: u32 = 480;
pub const TILE_WIDTH: u32 = 8;
pub const PIXEL_FRAME_WIDTH: u32 = FRAME_WIDTH / 2;
pub const PIXEL_FRAME_HEIGHT: u32 = FRAME_HEIGHT / 2;
// Bytes per 8x8 tile (16-bit pixels).
pub const TILE_SIZE: u32 = TILE_WIDTH * TILE_WIDTH * 2;
pub const SPRITE_WIDTH: u32 = 32;
// Bytes per 32x32 sprite (16-bit pixels).
pub const SPRITE_SIZE: u32 = SPRITE_WIDTH * SPRITE_WIDTH * 2;
pub const SPRITE_COUNT: usize = 16;
pub const TILE_FB_WIDTH_TILES: u32 = FRAME_WIDTH / TILE_WIDTH;
pub const TILE_FB_HEIGHT_TILES: u32 = FRAME_HEIGHT / TILE_WIDTH;

// ---- MMIO address map (docs/mem_map.md); END bounds are exclusive ------------
const AUDIO_RING_BUFFER_START: u32 = 0x7FB_8000;
const AUDIO_RING_BUFFER_SIZE: u32 = 0x4000;
const AUDIO_RING_BUFFER_END: u32 = AUDIO_RING_BUFFER_START + AUDIO_RING_BUFFER_SIZE;
// Everything at or above the audio ring is MMIO; RAM ends here.
const IO_START: u32 = AUDIO_RING_BUFFER_START;

const PIXEL_FRAME_BUFFER_START: u32 = 0x7FC_0000;
const PIXEL_FRAME_BUFFER_SIZE: u32 = PIXEL_FRAME_WIDTH * PIXEL_FRAME_HEIGHT * 2;
const PIXEL_FRAME_BUFFER_END: u32 = PIXEL_FRAME_BUFFER_START + PIXEL_FRAME_BUFFER_SIZE;
// Two bytes per tile entry (index + color) in an 80x60 grid.
const TILE_FRAME_BUFFER_SIZE: u32 = TILE_FB_WIDTH_TILES * TILE_FB_HEIGHT_TILES * 2;
// Aligned down to a 4 KiB page below the pixel framebuffer for TLB mappings.
const TILE_FRAME_BUFFER_START: u32 = (PIXEL_FRAME_BUFFER_START - TILE_FRAME_BUFFER_SIZE) & !0xFFF;
const TILE_FRAME_BUFFER_END: u32 = TILE_FRAME_BUFFER_START + TILE_FRAME_BUFFER_SIZE;

const PS2_STREAM: u32 = 0x7FE_5800;
// Reading the PS/2 high byte pops the event (software reads it as a halfword).
const PS2_STREAM_HIGH: u32 = PS2_STREAM + 1;
const UART_TX: u32 = 0x7FE_5802;
const UART_RX: u32 = 0x7FE_5803;
pub const PIT_START: u32 = 0x7FE_5804;
const PIT_END: u32 = PIT_START + 4;
// PS/2 mouse event word: byte 0 peeks, byte 3 pops (software reads a word).
const MOUSE_STREAM_START: u32 = 0x7FE_5808;
const MOUSE_STREAM_POP: u32 = MOUSE_STREAM_START + 3;
const MOUSE_STREAM_END: u32 = MOUSE_STREAM_START + 4;

const SD_DMA_START: u32 = 0x7FE_5810;
const SD2_DMA_START: u32 = 0x7FE_5828;
const SD_DMA_RANGE_SIZE: u32 = 0x18;
const SD_DMA_END: u32 = SD_DMA_START + SD_DMA_RANGE_SIZE;
const SD2_DMA_END: u32 = SD2_DMA_START + SD_DMA_RANGE_SIZE;

const AUDIO_REGS_START: u32 = 0x7FE_5840;
const AUDIO_REGS_END: u32 = AUDIO_REGS_START + 0x14;

// Sprite coordinate registers: per sprite, x then y as signed 16-bit LE values.
const SPRITE_REGISTERS_START: u32 = 0x7FE_5B00;
const SPRITE_REGISTERS_END: u32 = SPRITE_REGISTERS_START + 4 * SPRITE_COUNT as u32;
const TILE_H_SCROLL_START: u32 = 0x7FE_5B40;
const TILE_V_SCROLL_START: u32 = 0x7FE_5B42;
// Each tile pixel is repeated 2^n times.
const TILE_SCALE_REGISTER: u32 = 0x7FE_5B44;
const VGA_STATUS_REGISTER: u32 = 0x7FE_5B46;
const VGA_FRAME_REGISTER_START: u32 = 0x7FE_5B48;
const VGA_FRAME_REGISTER_END: u32 = VGA_FRAME_REGISTER_START + 4;
const CLK_REG_START: u32 = 0x7FE_5B4C;
const CLK_REG_END: u32 = CLK_REG_START + 4;
const PIXEL_H_SCROLL_START: u32 = 0x7FE_5B50;
const PIXEL_V_SCROLL_START: u32 = 0x7FE_5B52;
// Each pixel is repeated 2^(n+1) times.
const PIXEL_SCALE_REGISTER: u32 = 0x7FE_5B54;
const SPRITE_SCALE_START: u32 = 0x7FE_5B60;
const SPRITE_SCALE_END: u32 = SPRITE_SCALE_START + SPRITE_COUNT as u32;

const TILE_MAP_START: u32 = 0x7FE_8000;
const TILE_MAP_SIZE: u32 = 0x8000;
const TILE_MAP_END: u32 = TILE_MAP_START + TILE_MAP_SIZE;
const SPRITE_MAP_START: u32 = 0x7FF_0000;
const SPRITE_MAP_SIZE: u32 = SPRITE_SIZE * SPRITE_COUNT as u32;
const SPRITE_MAP_END: u32 = SPRITE_MAP_START + SPRITE_MAP_SIZE;

// ---- SD DMA engine (register offsets within one SD block) --------------------
const SD_BLOCK_SIZE: usize = 512;
const SD_DMA_BYTES_PER_TICK: u32 = 4;
const SD_DMA_OFFSET_MEM_ADDR: u32 = 0x0;
const SD_DMA_OFFSET_SD_BLOCK: u32 = 0x4;
const SD_DMA_OFFSET_LEN: u32 = 0x8;
const SD_DMA_OFFSET_CTRL: u32 = 0xC;
const SD_DMA_OFFSET_STATUS: u32 = 0x10;
const SD_DMA_CTRL_START: u32 = 1 << 0;
const SD_DMA_CTRL_DIR_RAM_TO_SD: u32 = 1 << 1;
const SD_DMA_CTRL_IRQ_ENABLE: u32 = 1 << 2;
const SD_DMA_CTRL_INIT: u32 = 1 << 3;
const SD_DMA_CTRL_MASK: u32 =
    SD_DMA_CTRL_START | SD_DMA_CTRL_DIR_RAM_TO_SD | SD_DMA_CTRL_IRQ_ENABLE | SD_DMA_CTRL_INIT;
const SD_DMA_STATUS_BUSY: u32 = 1 << 0;
const SD_DMA_STATUS_DONE: u32 = 1 << 1;
const SD_DMA_STATUS_ERR: u32 = 1 << 2;
const SD_DMA_ERR_NONE: u32 = 0;
const SD_DMA_ERR_BUSY: u32 = 1;
const SD_DMA_ERR_ZERO_LEN: u32 = 2;
const SD_DMA_ERR_NOT_INITIALIZED: u32 = 3;
const SD_INIT_TICKS: u32 = 32;

// ---- Audio device -------------------------------------------------------------
const AUDIO_OFFSET_CTRL: u32 = 0x0;
const AUDIO_OFFSET_STATUS: u32 = 0x4;
const AUDIO_OFFSET_WRITE_IDX: u32 = 0x8;
const AUDIO_OFFSET_READ_IDX: u32 = 0xC;
const AUDIO_OFFSET_WATERMARK: u32 = 0x10;
const AUDIO_SAMPLE_BYTES: u32 = 2;
pub const AUDIO_SAMPLE_RATE_HZ: u32 = 25_000;
// Device ticks model a 100 MHz clock.
const AUDIO_TICKS_PER_SAMPLE: u32 = 100_000_000 / AUDIO_SAMPLE_RATE_HZ;
const AUDIO_CTRL_ENABLE: u32 = 1 << 0;
const AUDIO_CTRL_IRQ_ENABLE: u32 = 1 << 1;
const AUDIO_STATUS_ENABLED: u32 = 1 << 0;
const AUDIO_STATUS_LOW_WATER: u32 = 1 << 1;
const AUDIO_STATUS_UNDERRUN: u32 = 1 << 2;
const AUDIO_STATUS_IRQ_PENDING: u32 = 1 << 3;

// Number of 32-bit RAM words below IO_START.
const RAM_WORDS: usize = (IO_START / 4) as usize;

// Byte `offset` (0..=3) of a little-endian register value.
fn byte_of(value: u32, offset: u32) -> u8 {
    (value >> (8 * offset)) as u8
}

// `value` with byte `offset` (0..=3) replaced by `byte`.
fn with_byte(value: u32, offset: u32, byte: u8) -> u32 {
    let shift = 8 * offset;
    (value & !(0xFF << shift)) | (u32::from(byte) << shift)
}

// Replace one byte of a 16-bit register. Only called under `mmio_lock`, and
// no other thread writes these registers, so load+store cannot lose updates.
fn store_u16_byte(reg: &AtomicU16, offset: u32, byte: u8) {
    let value = with_byte(u32::from(reg.load(Ordering::SeqCst)), offset, byte);
    reg.store(value as u16, Ordering::SeqCst);
}

// Identify which SD card device should receive a host image.
#[derive(Clone, Copy)]
pub enum SdSlot {
    Sd0,
    Sd1,
}

// Allocate `count` zeroed words as atomics. Going through a zeroed `u32`
// allocation lets the host hand out lazily zeroed pages instead of touching
// all of guest RAM at startup.
fn zeroed_atomic_words(count: usize) -> Box<[AtomicU32]> {
    const _: () = assert!(
        std::mem::size_of::<AtomicU32>() == std::mem::size_of::<u32>()
            && std::mem::align_of::<AtomicU32>() == std::mem::align_of::<u32>()
    );
    let words: Box<[u32]> = vec![0u32; count].into_boxed_slice();
    // SAFETY: AtomicU32 has the same size and bit validity as u32 (std docs)
    // and the assertion above checks the alignment matches, so this
    // allocation is valid for, and is later freed as, [AtomicU32].
    unsafe { Box::from_raw(Box::into_raw(words) as *mut [AtomicU32]) }
}

// Extract `out.len()` guest bytes starting at byte `offset` of `word`.
fn word_bytes(word: u32, offset: u32, out: &mut [u8]) {
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = byte_of(word, offset + i as u32);
    }
}

// Device tick counters advanced every tick by core 0 only. Kept on their own
// cache line so those writes do not invalidate state other cores read every
// tick (clock divider, input-pending flag).
#[repr(align(64))]
struct DeviceClock {
    pit_countdown: AtomicU32,
    audio_sample_countdown: AtomicU32,
}

// VGA state shared between guest MMIO and the graphics thread.
pub struct VgaState {
    pub pixel_frame_buffer: RwLock<Vec<u8>>,
    pub tile_frame_buffer: RwLock<Vec<u8>>,
    pub tile_map: RwLock<Vec<u8>>,
    pub sprite_pixels: RwLock<Vec<u8>>,
    // Per sprite: x in bits 15:0, y in bits 31:16 (signed 16-bit each).
    pub sprite_coords: [AtomicU32; SPRITE_COUNT],
    pub sprite_scales: [AtomicU8; SPRITE_COUNT],
    pub tile_hscroll: AtomicU16,
    pub tile_vscroll: AtomicU16,
    pub pixel_hscroll: AtomicU16,
    pub pixel_vscroll: AtomicU16,
    pub tile_scale: AtomicU8,
    pub pixel_scale: AtomicU8,
    // Written by the graphics thread only; read-only to the guest.
    pub status: AtomicU8,
    pub frame: AtomicU32,
}

impl VgaState {
    fn new() -> Self {
        VgaState {
            pixel_frame_buffer: RwLock::new(vec![0; PIXEL_FRAME_BUFFER_SIZE as usize]),
            tile_frame_buffer: RwLock::new(vec![0; TILE_FRAME_BUFFER_SIZE as usize]),
            tile_map: RwLock::new(vec![0; TILE_MAP_SIZE as usize]),
            // Sprite pixels reset to 0xFFFF, which is transparent.
            sprite_pixels: RwLock::new(vec![0xFF; SPRITE_MAP_SIZE as usize]),
            sprite_coords: std::array::from_fn(|_| AtomicU32::new(0)),
            sprite_scales: std::array::from_fn(|_| AtomicU8::new(0)),
            tile_hscroll: AtomicU16::new(0),
            tile_vscroll: AtomicU16::new(0),
            pixel_hscroll: AtomicU16::new(0),
            pixel_vscroll: AtomicU16::new(0),
            tile_scale: AtomicU8::new(0),
            pixel_scale: AtomicU8::new(0),
            status: AtomicU8::new(0),
            frame: AtomicU32::new(0),
        }
    }
}

// SD card storage indexed by block, plus DMA register state.
// Invariants: dma_remaining > 0 while dma_active; BUSY status implies a DMA
// or init sequence is active; image_len is the exported image length and
// grows when writes land past it.
struct SdCard {
    storage: HashMap<u32, Vec<u8>>,
    image_len: u64,
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
    init_active: bool,
    init_ticks_remaining: u32,
    initialized: bool,
}

// What one SD engine tick asks the memory system to do.
enum SdTick {
    Idle,
    RaiseInterrupt,
    // Move `bytes` between RAM at `mem_addr` and the card at `sd_offset`;
    // raise the interrupt afterwards if `irq_after`.
    Transfer {
        mem_addr: u32,
        sd_offset: u64,
        bytes: u32,
        to_sd: bool,
        irq_after: bool,
    },
}

impl SdCard {
    // Create an empty SD device with reset DMA state.
    fn new(dma_ticks_per_word: u32) -> Self {
        SdCard {
            storage: HashMap::new(),
            image_len: 0,
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
            dma_ticks_per_word: dma_ticks_per_word.max(1),
            dma_tick_countdown: 0,
            init_active: false,
            init_ticks_remaining: 0,
            initialized: false,
        }
    }

    // Report a command issued while busy without disturbing the running one.
    fn reject_if_busy(&mut self) -> bool {
        if self.dma_status & SD_DMA_STATUS_BUSY != 0 {
            self.dma_err = SD_DMA_ERR_BUSY;
            self.dma_status |= SD_DMA_STATUS_ERR;
            true
        } else {
            false
        }
    }

    // Start the SD initialization sequence; completes after SD_INIT_TICKS.
    fn start_init(&mut self) -> bool {
        if self.reject_if_busy() {
            return false;
        }
        self.dma_active = false;
        self.dma_remaining = 0;
        self.dma_tick_countdown = 0;
        self.dma_err = SD_DMA_ERR_NONE;
        self.dma_status = SD_DMA_STATUS_BUSY;
        self.init_active = true;
        self.init_ticks_remaining = SD_INIT_TICKS;
        self.initialized = false;
        false
    }

    // Start a DMA transfer from the current registers. Returns true if the
    // command failed immediately and the interrupt should be raised now.
    fn start_dma(&mut self) -> bool {
        if self.reject_if_busy() {
            return false;
        }
        let irq_enable = self.dma_ctrl & SD_DMA_CTRL_IRQ_ENABLE != 0;
        // SD_DMA_LEN is in blocks; the engine counts bytes (32-bit truncation).
        let len_bytes = self.dma_len.wrapping_mul(SD_BLOCK_SIZE as u32);
        let error = if !self.initialized {
            Some(SD_DMA_ERR_NOT_INITIALIZED)
        } else if len_bytes == 0 {
            Some(SD_DMA_ERR_ZERO_LEN)
        } else {
            None
        };
        if let Some(err) = error {
            self.dma_err = err;
            self.dma_status = SD_DMA_STATUS_DONE | SD_DMA_STATUS_ERR;
            self.dma_active = false;
            return irq_enable;
        }
        self.dma_mem_cursor = self.dma_mem_addr & !0x3;
        self.dma_sd_byte_cursor = u64::from(self.dma_sd_block) * SD_BLOCK_SIZE as u64;
        self.dma_remaining = len_bytes;
        self.dma_err = SD_DMA_ERR_NONE;
        self.dma_status = SD_DMA_STATUS_BUSY;
        self.dma_active = true;
        self.dma_tick_countdown = 0;
        false
    }

    // Read one byte of the DMA register block. STATUS.ERR mirrors ERR != 0.
    fn read_reg_byte(&self, offset: u32) -> u8 {
        let value = match offset & !3 {
            SD_DMA_OFFSET_MEM_ADDR => self.dma_mem_addr,
            SD_DMA_OFFSET_SD_BLOCK => self.dma_sd_block,
            SD_DMA_OFFSET_LEN => self.dma_len,
            SD_DMA_OFFSET_CTRL => self.dma_ctrl,
            SD_DMA_OFFSET_STATUS => {
                if self.dma_err != SD_DMA_ERR_NONE {
                    self.dma_status | SD_DMA_STATUS_ERR
                } else {
                    self.dma_status & !SD_DMA_STATUS_ERR
                }
            }
            _ => self.dma_err,
        };
        byte_of(value, offset & 3)
    }

    // Write one byte of the DMA register block. CTRL.START/INIT are command
    // strobes that clear once observed; any write to STATUS clears DONE/ERR.
    // Returns true when the write requires an immediate interrupt.
    fn write_reg_byte(&mut self, offset: u32, value: u8) -> bool {
        let byte = offset & 3;
        match offset & !3 {
            SD_DMA_OFFSET_MEM_ADDR => self.dma_mem_addr = with_byte(self.dma_mem_addr, byte, value),
            SD_DMA_OFFSET_SD_BLOCK => self.dma_sd_block = with_byte(self.dma_sd_block, byte, value),
            SD_DMA_OFFSET_LEN => self.dma_len = with_byte(self.dma_len, byte, value),
            SD_DMA_OFFSET_CTRL => {
                let ctrl = with_byte(self.dma_ctrl, byte, value) & SD_DMA_CTRL_MASK;
                self.dma_ctrl = ctrl & !(SD_DMA_CTRL_START | SD_DMA_CTRL_INIT);
                if ctrl & SD_DMA_CTRL_INIT != 0 {
                    return self.start_init();
                }
                if ctrl & SD_DMA_CTRL_START != 0 {
                    return self.start_dma();
                }
            }
            SD_DMA_OFFSET_STATUS => {
                self.dma_status &= !(SD_DMA_STATUS_DONE | SD_DMA_STATUS_ERR);
                self.dma_err = SD_DMA_ERR_NONE;
            }
            // SD_DMA_ERR is read-only; writes are ignored.
            _ => {}
        }
        false
    }

    // Advance the init sequence or DMA engine by one device tick.
    fn tick(&mut self) -> SdTick {
        let irq_enable = self.dma_ctrl & SD_DMA_CTRL_IRQ_ENABLE != 0;
        if self.init_active {
            self.init_ticks_remaining = self.init_ticks_remaining.saturating_sub(1);
            if self.init_ticks_remaining != 0 {
                return SdTick::Idle;
            }
            self.init_active = false;
            self.initialized = true;
            self.dma_status = (self.dma_status & !SD_DMA_STATUS_BUSY) | SD_DMA_STATUS_DONE;
            return if irq_enable { SdTick::RaiseInterrupt } else { SdTick::Idle };
        }
        if !self.dma_active {
            return SdTick::Idle;
        }
        if self.dma_tick_countdown > 0 {
            self.dma_tick_countdown -= 1;
            return SdTick::Idle;
        }
        self.dma_tick_countdown = self.dma_ticks_per_word - 1;
        let bytes = self.dma_remaining.min(SD_DMA_BYTES_PER_TICK);
        let (mem_addr, sd_offset) = (self.dma_mem_cursor, self.dma_sd_byte_cursor);
        self.dma_mem_cursor = self.dma_mem_cursor.wrapping_add(bytes);
        self.dma_sd_byte_cursor = self.dma_sd_byte_cursor.wrapping_add(u64::from(bytes));
        self.dma_remaining -= bytes;
        let done = self.dma_remaining == 0;
        if done {
            self.dma_active = false;
            self.dma_status = (self.dma_status & !SD_DMA_STATUS_BUSY) | SD_DMA_STATUS_DONE;
            if self.dma_err != SD_DMA_ERR_NONE {
                self.dma_status |= SD_DMA_STATUS_ERR;
            }
        }
        SdTick::Transfer {
            mem_addr,
            sd_offset,
            bytes,
            to_sd: self.dma_ctrl & SD_DMA_CTRL_DIR_RAM_TO_SD != 0,
            irq_after: done && irq_enable,
        }
    }

    // Read a byte from storage; unwritten blocks read as 0.
    fn read_storage_byte(&self, byte_offset: u64) -> u8 {
        let block = (byte_offset / SD_BLOCK_SIZE as u64) as u32;
        let offset = (byte_offset % SD_BLOCK_SIZE as u64) as usize;
        self.storage.get(&block).map_or(0, |b| b[offset])
    }

    // Write a byte to storage, allocating its block and growing the image.
    fn write_storage_byte(&mut self, byte_offset: u64, value: u8) {
        let block = (byte_offset / SD_BLOCK_SIZE as u64) as u32;
        let offset = (byte_offset % SD_BLOCK_SIZE as u64) as usize;
        self.storage
            .entry(block)
            .or_insert_with(|| vec![0; SD_BLOCK_SIZE])[offset] = value;
        self.image_len = self.image_len.max(byte_offset + 1);
    }

    // Replace storage with a raw image starting at block 0.
    fn load_image(&mut self, image: &[u8]) {
        self.storage.clear();
        self.image_len = image.len() as u64;
        for (index, chunk) in image.chunks(SD_BLOCK_SIZE).enumerate() {
            let mut block = vec![0u8; SD_BLOCK_SIZE];
            block[..chunk.len()].copy_from_slice(chunk);
            self.storage.insert(index as u32, block);
        }
    }

    // Serialize storage as a raw image covering [0, image_len), zero-filling gaps.
    fn dump_image(&self) -> Vec<u8> {
        let len =
            usize::try_from(self.image_len).expect("SD: image length exceeds host address space");
        let mut image = vec![0u8; len];
        for (&block_index, block) in &self.storage {
            let start = block_index as usize * SD_BLOCK_SIZE;
            if start < len {
                let end = (start + SD_BLOCK_SIZE).min(len);
                image[start..end].copy_from_slice(&block[..end - start]);
            }
        }
        image
    }
}

// Fixed-format PCM sink exposed through MMIO registers plus a byte ring.
// Software writes PCM bytes and advances WRITE_IDX; the device advances
// READ_IDX at the sample rate and raises an interrupt only when LOW_WATER
// goes from false to true while IRQ delivery is enabled.
// Invariants:
// - read_idx always stays normalized to the ring size
// - UNDERRUN clears when playback is disabled or software publishes at least
//   one full sample again
struct AudioDevice {
    ring: Vec<u8>,
    ctrl: u32,
    write_idx: u32,
    read_idx: u32,
    watermark: u32,
    underrun: bool,
}

impl AudioDevice {
    // Create an audio device with an empty ring.
    fn new() -> Self {
        AudioDevice {
            ring: vec![0; AUDIO_RING_BUFFER_SIZE as usize],
            ctrl: 0,
            write_idx: 0,
            read_idx: 0,
            watermark: 0,
            underrun: false,
        }
    }

    // Unread PCM bytes between producer and consumer.
    fn buffered_bytes(&self) -> u32 {
        let write = self.write_idx % AUDIO_RING_BUFFER_SIZE;
        if write >= self.read_idx {
            write - self.read_idx
        } else {
            AUDIO_RING_BUFFER_SIZE - (self.read_idx - write)
        }
    }

    // Whether unread data is at or below the watermark (this is also the
    // IRQ-pending condition).
    fn low_water(&self) -> bool {
        self.buffered_bytes() <= self.watermark
    }

    fn enabled(&self) -> bool {
        self.ctrl & AUDIO_CTRL_ENABLE != 0
    }

    // Whether this change crossed into low water with IRQs enabled.
    fn low_water_edge(&self, was_low_water: bool) -> bool {
        !was_low_water && self.low_water() && self.ctrl & AUDIO_CTRL_IRQ_ENABLE != 0
    }

    // Guest-visible status register.
    fn status(&self) -> u32 {
        let mut status = 0;
        if self.enabled() {
            status |= AUDIO_STATUS_ENABLED;
        }
        if self.low_water() {
            status |= AUDIO_STATUS_LOW_WATER | AUDIO_STATUS_IRQ_PENDING;
        }
        if self.underrun {
            status |= AUDIO_STATUS_UNDERRUN;
        }
        status
    }

    // Read one byte of the register window.
    fn read_reg_byte(&self, offset: u32) -> u8 {
        let value = match offset & !3 {
            AUDIO_OFFSET_CTRL => self.ctrl,
            AUDIO_OFFSET_STATUS => self.status(),
            AUDIO_OFFSET_WRITE_IDX => self.write_idx,
            AUDIO_OFFSET_READ_IDX => self.read_idx,
            _ => self.watermark,
        };
        byte_of(value, offset & 3)
    }

    // Write one byte of the register window. STATUS and READ_IDX are
    // read-only; writing them is a guest bug and stops the emulator.
    fn write_reg_byte(&mut self, offset: u32, value: u8) {
        let byte = offset & 3;
        match offset & !3 {
            AUDIO_OFFSET_CTRL => {
                self.ctrl = with_byte(self.ctrl, byte, value)
                    & (AUDIO_CTRL_ENABLE | AUDIO_CTRL_IRQ_ENABLE);
                if !self.enabled() {
                    self.underrun = false;
                }
            }
            AUDIO_OFFSET_WRITE_IDX => self.write_idx = with_byte(self.write_idx, byte, value),
            AUDIO_OFFSET_WATERMARK => self.watermark = with_byte(self.watermark, byte, value),
            reg => panic!(
                "MMIO: attempting to write read-only audio {} register (0x{:08X})",
                if reg == AUDIO_OFFSET_STATUS { "status" } else { "read index" },
                AUDIO_REGS_START + reg
            ),
        }
    }

    // Consume one sample now. Disabled playback outputs silence without
    // moving READ_IDX; enabled playback with an empty ring latches UNDERRUN
    // and outputs signed zero.
    fn consume_sample_now(&mut self) -> i16 {
        if !self.enabled() {
            return 0;
        }
        if self.buffered_bytes() < AUDIO_SAMPLE_BYTES {
            self.underrun = true;
            return 0;
        }
        let lo = self.ring[self.read_idx as usize];
        let hi = self.ring[((self.read_idx + 1) % AUDIO_RING_BUFFER_SIZE) as usize];
        self.read_idx = (self.read_idx + AUDIO_SAMPLE_BYTES) % AUDIO_RING_BUFFER_SIZE;
        i16::from_le_bytes([lo, hi])
    }
}

// Guest RAM, MMIO devices, and their architecturally visible state.
pub struct Memory {
    ram: Box<[AtomicU32]>,
    mmio_lock: Mutex<()>,
    device_clock: DeviceClock,
    vga: VgaState,
    // Host key events waiting for the guest; `input_pending` mirrors
    // "non-empty" so every core can poll it each tick without locking.
    input: Mutex<VecDeque<u16>>,
    input_pending: AtomicBool,
    // Host mouse events waiting for the guest; `mouse_pending` mirrors
    // "non-empty" for lock-free per-tick polling like `input_pending`.
    mouse: Mutex<MouseQueue>,
    mouse_pending: AtomicBool,
    pit_reload: AtomicU32,
    clk_divider: AtomicU32,
    sd_cards: [Mutex<SdCard>; 2],
    audio: Mutex<AudioDevice>,
    // Device interrupt bits raised since core 0 last collected them.
    pending_interrupt: AtomicU32,
    use_uart_rx: bool,
}

impl Memory {
    // Create guest memory initialized from a sparse byte image (bytes at or
    // above IO_START are ignored) with every device reset.
    pub fn new(ram: HashMap<u32, u8>, use_uart_rx: bool, sd_dma_ticks_per_word: u32) -> Memory {
        let mut words = zeroed_atomic_words(RAM_WORDS);
        for (addr, value) in ram {
            if addr < IO_START {
                let word = words[(addr / 4) as usize].get_mut();
                *word = with_byte(*word, addr % 4, value);
            }
        }
        Memory {
            ram: words,
            mmio_lock: Mutex::new(()),
            device_clock: DeviceClock {
                pit_countdown: AtomicU32::new(0),
                audio_sample_countdown: AtomicU32::new(0),
            },
            vga: VgaState::new(),
            input: Mutex::new(VecDeque::new()),
            input_pending: AtomicBool::new(false),
            mouse: Mutex::new(MouseQueue::new()),
            mouse_pending: AtomicBool::new(false),
            pit_reload: AtomicU32::new(0),
            clk_divider: AtomicU32::new(0),
            sd_cards: [
                Mutex::new(SdCard::new(sd_dma_ticks_per_word)),
                Mutex::new(SdCard::new(sd_dma_ticks_per_word)),
            ],
            audio: Mutex::new(AudioDevice::new()),
            pending_interrupt: AtomicU32::new(0),
            use_uart_rx,
        }
    }

    // RAM word containing `addr` (which must be below IO_START).
    fn ram_word(&self, addr: u32) -> &AtomicU32 {
        &self.ram[(addr / 4) as usize]
    }

    // Store guest bytes within one RAM word. A full aligned word is a single
    // store; narrower stores merge into the word with a compare-and-swap so a
    // concurrent store to the word's other bytes is never lost.
    fn store_ram_bytes(&self, addr: u32, data: &[u8]) {
        let word = self.ram_word(addr);
        if data.len() == 4 {
            word.store(u32::from_le_bytes(data.try_into().unwrap()), Ordering::SeqCst);
            return;
        }
        let offset = addr % 4;
        let merge = |value: u32| {
            let merged = data
                .iter()
                .enumerate()
                .fold(value, |acc, (i, byte)| with_byte(acc, offset + i as u32, *byte));
            Some(merged)
        };
        let _ = word.fetch_update(Ordering::SeqCst, Ordering::SeqCst, merge);
    }

    // Warn when guest code reads the null physical address.
    fn warn_null_read(addr: u32) {
        if addr == 0 {
            println!("Warning: reading from physical address 0x00000000");
        }
    }

    // Warn when guest code writes the null physical address.
    fn warn_null_write(addr: u32, data: u8) {
        if addr == 0 {
            println!("Warning: writing to physical address 0x00000000: 0x{:08X}", data);
        }
    }

    // ---- Shared state accessors ------------------------------------------

    // VGA state for the graphics thread.
    pub fn vga(&self) -> &VgaState {
        &self.vga
    }

    // Queue a host key event for the guest (PS/2 stream or UART RX).
    pub fn push_input(&self, event: u16) {
        self.input.lock().unwrap().push_back(event);
        self.input_pending.store(true, Ordering::SeqCst);
    }

    // Pop the next queued input event (0 if none).
    fn pop_input(&self) -> u16 {
        let mut queue = self.input.lock().unwrap();
        let value = queue.pop_front().unwrap_or(0);
        self.input_pending.store(!queue.is_empty(), Ordering::SeqCst);
        value
    }

    // Whether input is waiting in the PS/2/UART queue.
    pub fn has_pending_input(&self) -> bool {
        self.input_pending.load(Ordering::SeqCst)
    }

    // Queue host mouse state for the guest (docs/mem_map.md "PS/2 mouse").
    // `buttons` uses the event-word button bits; motion is in guest screen
    // pixels with +dy = down and +wheel = scroll toward the user.
    pub fn push_mouse(&self, buttons: u8, dx: i32, dy: i32, wheel: i32) {
        let mut queue = self.mouse.lock().unwrap();
        queue.push(buttons, dx, dy, wheel);
        self.mouse_pending.store(!queue.is_empty(), Ordering::SeqCst);
    }

    // Pop the oldest mouse event word (0 if none).
    fn pop_mouse(&self) -> u32 {
        let mut queue = self.mouse.lock().unwrap();
        let value = queue.pop();
        self.mouse_pending.store(!queue.is_empty(), Ordering::SeqCst);
        value
    }

    // Whether a mouse event is waiting for the guest.
    pub fn has_pending_mouse(&self) -> bool {
        self.mouse_pending.load(Ordering::SeqCst)
    }

    // Current clock-divider register value (read every tick by every core).
    pub fn clock_divider(&self) -> u32 {
        self.clk_divider.load(Ordering::SeqCst)
    }

    // Publish a device interrupt without discarding concurrently raised ones.
    pub fn raise_pending_interrupt(&self, interrupt_bit: u32) {
        self.pending_interrupt.fetch_or(interrupt_bit, Ordering::SeqCst);
    }

    // Take (and clear) the device interrupts raised since the last call.
    pub fn check_interrupts(&self) -> u32 {
        // Core 0 calls this every tick; avoid a read-modify-write when idle.
        if self.pending_interrupt.load(Ordering::SeqCst) == 0 {
            return 0;
        }
        self.pending_interrupt.swap(0, Ordering::SeqCst)
    }

    // ---- CPU-facing accesses (naturally aligned) ---------------------------

    // Read one byte from RAM or MMIO.
    pub fn read(&self, addr: u32) -> u8 {
        if addr >= IO_START {
            return self.mmio_transaction(|m| m.read_mmio_byte(addr));
        }
        Self::warn_null_read(addr);
        byte_of(self.ram_word(addr).load(Ordering::SeqCst), addr % 4)
    }

    // Read an aligned little-endian halfword (the address is aligned down).
    pub fn read_u16(&self, addr: u32) -> u16 {
        let mut bytes = [0; 2];
        self.read_aligned(addr & !1, &mut bytes);
        u16::from_le_bytes(bytes)
    }

    // Read an aligned little-endian word (the address is aligned down).
    pub fn read_u32(&self, addr: u32) -> u32 {
        let mut bytes = [0; 4];
        self.read_aligned(addr & !3, &mut bytes);
        u32::from_le_bytes(bytes)
    }

    // Read an aligned 2- or 4-byte value, which is either inside one RAM
    // word or all MMIO.
    fn read_aligned(&self, addr: u32, out: &mut [u8]) {
        if addr >= IO_START {
            self.mmio_transaction(|m| m.read_mmio_bytes(addr, out));
            return;
        }
        Self::warn_null_read(addr);
        word_bytes(self.ram_word(addr).load(Ordering::SeqCst), addr % 4, out);
    }

    // Write one byte to RAM or MMIO.
    pub fn write(&self, addr: u32, data: u8) {
        self.write_aligned(addr, &[data]);
    }

    // Write an aligned little-endian halfword (the address is aligned down).
    pub fn write_u16(&self, addr: u32, data: u16) {
        self.write_aligned(addr & !1, &data.to_le_bytes());
    }

    // Write an aligned little-endian word (the address is aligned down).
    pub fn write_u32(&self, addr: u32, data: u32) {
        self.write_aligned(addr & !3, &data.to_le_bytes());
    }

    // Write an aligned value that is either inside one RAM word or all MMIO.
    fn write_aligned(&self, addr: u32, data: &[u8]) {
        if addr >= IO_START {
            self.mmio_transaction(|m| m.write_mmio_bytes(addr, data));
            return;
        }
        Self::warn_null_write(addr, data[0]);
        self.store_ram_bytes(addr, data);
    }

    // Atomically replace the aligned word at `addr` with `update(old)` and
    // return the old value. RAM uses a compare-and-swap loop (so `update` may
    // run more than once); MMIO holds `mmio_lock` across the read and write.
    pub fn atomic_update_u32(&self, addr: u32, update: impl Fn(u32) -> u32) -> u32 {
        let addr = addr & !3;
        if addr >= IO_START {
            return self.mmio_transaction(|m| {
                let mut prev = [0; 4];
                m.read_mmio_bytes(addr, &mut prev);
                let prev = u32::from_le_bytes(prev);
                m.write_mmio_bytes(addr, &update(prev).to_le_bytes());
                prev
            });
        }
        self.ram_word(addr)
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |prev| Some(update(prev)))
            .unwrap()
    }

    // ---- Byte ranges (SD DMA and tests) -------------------------------------

    // Read a contiguous physical range byte by byte (an aligned RAM word is
    // read with one load). Used by SD DMA, which moves aligned words.
    pub fn read_phys_range(&self, addr: u32, out: &mut [u8]) {
        if out.len() == 4 && addr.is_multiple_of(4) && addr < IO_START {
            word_bytes(self.ram_word(addr).load(Ordering::SeqCst), 0, out);
            return;
        }
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.read(addr + i as u32);
        }
    }

    // Write a contiguous physical range; see `read_phys_range`.
    pub fn write_phys_range(&self, addr: u32, data: &[u8]) {
        if data.len() == 4 && addr.is_multiple_of(4) && addr < IO_START {
            self.store_ram_bytes(addr, data);
            return;
        }
        for (i, byte) in data.iter().enumerate() {
            self.write(addr + i as u32, *byte);
        }
    }

    // ---- MMIO --------------------------------------------------------------

    // Run one guest-visible MMIO access under `mmio_lock`. Afterwards the
    // audio device recovers from underrun if refilled and raises its
    // interrupt if the access crossed into low water, so multi-byte writes are
    // judged on their final value, never an intermediate byte.
    fn mmio_transaction<R>(&self, access: impl FnOnce(&Self) -> R) -> R {
        let _guard = self.mmio_lock.lock().unwrap();
        let was_low_water = self.audio.lock().unwrap().low_water();
        let result = access(self);
        let mut audio = self.audio.lock().unwrap();
        if audio.buffered_bytes() >= AUDIO_SAMPLE_BYTES {
            audio.underrun = false;
        }
        if audio.low_water_edge(was_low_water) {
            self.raise_pending_interrupt(AUDIO_INTERRUPT_BIT);
        }
        result
    }

    // Lock-free register (and its base) containing `addr`, if any.
    fn lockfree_register(&self, addr: u32) -> Option<(&AtomicU32, u32)> {
        match addr {
            PIT_START..PIT_END => Some((&self.pit_reload, PIT_START)),
            CLK_REG_START..CLK_REG_END => Some((&self.clk_divider, CLK_REG_START)),
            _ => None,
        }
    }

    // Read contiguous MMIO bytes. Caller holds `mmio_lock`.
    fn read_mmio_bytes(&self, addr: u32, out: &mut [u8]) {
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.read_mmio_byte(addr + i as u32);
        }
    }

    // Write contiguous MMIO bytes. Caller holds `mmio_lock`. Writes that fall
    // inside one lock-free register are applied with a single store.
    fn write_mmio_bytes(&self, addr: u32, data: &[u8]) {
        if let Some((reg, base)) = self.lockfree_register(addr)
            && addr + data.len() as u32 <= base + 4 {
                let mut value = reg.load(Ordering::SeqCst);
                for (i, byte) in data.iter().enumerate() {
                    value = with_byte(value, addr - base + i as u32, *byte);
                }
                reg.store(value, Ordering::SeqCst);
                return;
            }
        for (i, byte) in data.iter().enumerate() {
            self.write_mmio_byte(addr + i as u32, *byte);
        }
    }

    // Decode one MMIO byte read. Unmapped addresses stop the emulator because
    // they indicate a guest bug that real hardware would not report.
    fn read_mmio_byte(&self, addr: u32) -> u8 {
        let vga = &self.vga;
        match addr {
            AUDIO_RING_BUFFER_START..AUDIO_RING_BUFFER_END => {
                self.audio.lock().unwrap().ring[(addr - AUDIO_RING_BUFFER_START) as usize]
            }
            AUDIO_REGS_START..AUDIO_REGS_END => {
                self.audio.lock().unwrap().read_reg_byte(addr - AUDIO_REGS_START)
            }
            TILE_FRAME_BUFFER_START..TILE_FRAME_BUFFER_END => {
                vga.tile_frame_buffer.read().unwrap()[(addr - TILE_FRAME_BUFFER_START) as usize]
            }
            PIXEL_FRAME_BUFFER_START..PIXEL_FRAME_BUFFER_END => {
                vga.pixel_frame_buffer.read().unwrap()[(addr - PIXEL_FRAME_BUFFER_START) as usize]
            }
            TILE_MAP_START..TILE_MAP_END => {
                vga.tile_map.read().unwrap()[(addr - TILE_MAP_START) as usize]
            }
            SPRITE_MAP_START..SPRITE_MAP_END => {
                vga.sprite_pixels.read().unwrap()[(addr - SPRITE_MAP_START) as usize]
            }
            SPRITE_REGISTERS_START..SPRITE_REGISTERS_END => {
                let offset = addr - SPRITE_REGISTERS_START;
                byte_of(vga.sprite_coords[(offset / 4) as usize].load(Ordering::SeqCst), offset % 4)
            }
            SD_DMA_START..SD_DMA_END => {
                self.sd_cards[0].lock().unwrap().read_reg_byte(addr - SD_DMA_START)
            }
            SD2_DMA_START..SD2_DMA_END => {
                self.sd_cards[1].lock().unwrap().read_reg_byte(addr - SD2_DMA_START)
            }
            // Software reads the PS/2 stream as a halfword: the low byte
            // peeks and reading the high byte pops the event.
            PS2_STREAM if !self.use_uart_rx => {
                self.input.lock().unwrap().front().copied().unwrap_or(0) as u8
            }
            PS2_STREAM_HIGH if !self.use_uart_rx => (self.pop_input() >> 8) as u8,
            PS2_STREAM | PS2_STREAM_HIGH => 0,
            // Byte 3 pops; the other bytes peek, so an aligned word load
            // returns one whole event and consumes it.
            MOUSE_STREAM_POP => (self.pop_mouse() >> 24) as u8,
            MOUSE_STREAM_START..MOUSE_STREAM_POP => {
                self.mouse.lock().unwrap().peek_byte(addr - MOUSE_STREAM_START)
            }
            UART_TX => panic!("MMIO: attempting to read output port (address {:X})", UART_TX),
            UART_RX if self.use_uart_rx => {
                let value = self.pop_input();
                // Key releases are not delivered over UART.
                if value & 0xFF00 != 0 { 0 } else { value as u8 }
            }
            UART_RX => 0,
            PIT_START..PIT_END | CLK_REG_START..CLK_REG_END => {
                let (reg, base) = self.lockfree_register(addr).unwrap();
                byte_of(reg.load(Ordering::SeqCst), addr - base)
            }
            TILE_H_SCROLL_START..TILE_SCALE_REGISTER => {
                let (reg, base) = if addr < TILE_V_SCROLL_START {
                    (&vga.tile_hscroll, TILE_H_SCROLL_START)
                } else {
                    (&vga.tile_vscroll, TILE_V_SCROLL_START)
                };
                byte_of(u32::from(reg.load(Ordering::SeqCst)), addr - base)
            }
            PIXEL_H_SCROLL_START..PIXEL_SCALE_REGISTER => {
                let (reg, base) = if addr < PIXEL_V_SCROLL_START {
                    (&vga.pixel_hscroll, PIXEL_H_SCROLL_START)
                } else {
                    (&vga.pixel_vscroll, PIXEL_V_SCROLL_START)
                };
                byte_of(u32::from(reg.load(Ordering::SeqCst)), addr - base)
            }
            TILE_SCALE_REGISTER => vga.tile_scale.load(Ordering::SeqCst),
            PIXEL_SCALE_REGISTER => vga.pixel_scale.load(Ordering::SeqCst),
            SPRITE_SCALE_START..SPRITE_SCALE_END => {
                vga.sprite_scales[(addr - SPRITE_SCALE_START) as usize].load(Ordering::SeqCst)
            }
            VGA_STATUS_REGISTER => vga.status.load(Ordering::SeqCst),
            VGA_FRAME_REGISTER_START..VGA_FRAME_REGISTER_END => {
                byte_of(vga.frame.load(Ordering::SeqCst), addr - VGA_FRAME_REGISTER_START)
            }
            _ => panic!("MMIO: read from unmapped IO address 0x{:08X}", addr),
        }
    }

    // Apply one MMIO byte write. Writes to read-only or unmapped registers
    // stop the emulator because they indicate a guest bug.
    fn write_mmio_byte(&self, addr: u32, data: u8) {
        let vga = &self.vga;
        match addr {
            AUDIO_RING_BUFFER_START..AUDIO_RING_BUFFER_END => {
                self.audio.lock().unwrap().ring[(addr - AUDIO_RING_BUFFER_START) as usize] = data
            }
            AUDIO_REGS_START..AUDIO_REGS_END => {
                self.audio.lock().unwrap().write_reg_byte(addr - AUDIO_REGS_START, data)
            }
            TILE_FRAME_BUFFER_START..TILE_FRAME_BUFFER_END => {
                vga.tile_frame_buffer.write().unwrap()[(addr - TILE_FRAME_BUFFER_START) as usize] =
                    data
            }
            PIXEL_FRAME_BUFFER_START..PIXEL_FRAME_BUFFER_END => {
                vga.pixel_frame_buffer.write().unwrap()
                    [(addr - PIXEL_FRAME_BUFFER_START) as usize] = data
            }
            TILE_MAP_START..TILE_MAP_END => {
                vga.tile_map.write().unwrap()[(addr - TILE_MAP_START) as usize] = data
            }
            SPRITE_MAP_START..SPRITE_MAP_END => {
                vga.sprite_pixels.write().unwrap()[(addr - SPRITE_MAP_START) as usize] = data
            }
            SPRITE_REGISTERS_START..SPRITE_REGISTERS_END => {
                let offset = addr - SPRITE_REGISTERS_START;
                let reg = &vga.sprite_coords[(offset / 4) as usize];
                reg.store(with_byte(reg.load(Ordering::SeqCst), offset % 4, data), Ordering::SeqCst);
            }
            SD_DMA_START..SD_DMA_END => {
                if self.sd_cards[0].lock().unwrap().write_reg_byte(addr - SD_DMA_START, data) {
                    self.raise_pending_interrupt(SD_INTERRUPT_BIT);
                }
            }
            SD2_DMA_START..SD2_DMA_END => {
                if self.sd_cards[1].lock().unwrap().write_reg_byte(addr - SD2_DMA_START, data) {
                    self.raise_pending_interrupt(SD2_INTERRUPT_BIT);
                }
            }
            PS2_STREAM | PS2_STREAM_HIGH => panic!(
                "MMIO: attempting to write read-only PS/2 keyboard stream (address 0x{:08X}, data 0x{:02X})",
                addr, data
            ),
            UART_TX => {
                print!("{}", data as char);
                io::stdout().flush().unwrap();
            }
            UART_RX => panic!("MMIO: attempting to write input port (address {:X})", UART_RX),
            MOUSE_STREAM_START..MOUSE_STREAM_END => panic!(
                "MMIO: attempting to write read-only PS/2 mouse stream (address 0x{:08X}, data 0x{:02X})",
                addr, data
            ),
            PIT_START..PIT_END | CLK_REG_START..CLK_REG_END => self.write_mmio_bytes(addr, &[data]),
            TILE_H_SCROLL_START..TILE_V_SCROLL_START => {
                store_u16_byte(&vga.tile_hscroll, addr - TILE_H_SCROLL_START, data)
            }
            TILE_V_SCROLL_START..TILE_SCALE_REGISTER => {
                store_u16_byte(&vga.tile_vscroll, addr - TILE_V_SCROLL_START, data)
            }
            PIXEL_H_SCROLL_START..PIXEL_V_SCROLL_START => {
                store_u16_byte(&vga.pixel_hscroll, addr - PIXEL_H_SCROLL_START, data)
            }
            PIXEL_V_SCROLL_START..PIXEL_SCALE_REGISTER => {
                store_u16_byte(&vga.pixel_vscroll, addr - PIXEL_V_SCROLL_START, data)
            }
            TILE_SCALE_REGISTER => vga.tile_scale.store(data, Ordering::SeqCst),
            PIXEL_SCALE_REGISTER => vga.pixel_scale.store(data, Ordering::SeqCst),
            SPRITE_SCALE_START..SPRITE_SCALE_END => {
                vga.sprite_scales[(addr - SPRITE_SCALE_START) as usize].store(data, Ordering::SeqCst)
            }
            VGA_STATUS_REGISTER => panic!(
                "MMIO: attempting to write read-only VGA status register (0x{:08X})",
                VGA_STATUS_REGISTER
            ),
            VGA_FRAME_REGISTER_START..VGA_FRAME_REGISTER_END => panic!(
                "MMIO: attempting to write read-only VGA frame register (0x{:08X})",
                VGA_FRAME_REGISTER_START
            ),
            _ => panic!("MMIO: write to unmapped IO address 0x{:08X}", addr),
        }
    }

    // ---- SD images -----------------------------------------------------------

    fn sd_card(&self, slot: SdSlot) -> &Mutex<SdCard> {
        match slot {
            SdSlot::Sd0 => &self.sd_cards[0],
            SdSlot::Sd1 => &self.sd_cards[1],
        }
    }

    // Load a raw SD image into the selected device.
    pub fn load_sd_image(&self, slot: SdSlot, image: &[u8]) {
        self.sd_card(slot).lock().unwrap().load_image(image);
    }

    // Export the selected device as a raw host image.
    pub fn dump_sd_image(&self, slot: SdSlot) -> Vec<u8> {
        self.sd_card(slot).lock().unwrap().dump_image()
    }

    // ---- Device ticks (core 0 only) --------------------------------------------

    // Advance both SD engines by one device tick.
    pub fn tick_sd_dma(&self) {
        self.tick_sd_card(0, SD_INTERRUPT_BIT);
        self.tick_sd_card(1, SD2_INTERRUPT_BIT);
    }

    // Advance one SD engine, moving at most one DMA word between the card and
    // guest memory. The card lock is not held across the guest-memory access.
    fn tick_sd_card(&self, index: usize, interrupt_bit: u32) {
        let action = self.sd_cards[index].lock().unwrap().tick();
        let SdTick::Transfer {
            mem_addr,
            sd_offset,
            bytes,
            to_sd,
            irq_after,
        } = action
        else {
            if matches!(action, SdTick::RaiseInterrupt) {
                self.raise_pending_interrupt(interrupt_bit);
            }
            return;
        };
        let mut buf = [0u8; SD_DMA_BYTES_PER_TICK as usize];
        let buf = &mut buf[..bytes as usize];
        if to_sd {
            self.read_phys_range(mem_addr, buf);
            let mut sd = self.sd_cards[index].lock().unwrap();
            for (i, byte) in buf.iter().enumerate() {
                sd.write_storage_byte(sd_offset + i as u64, *byte);
            }
        } else {
            {
                let sd = self.sd_cards[index].lock().unwrap();
                for (i, slot) in buf.iter_mut().enumerate() {
                    *slot = sd.read_storage_byte(sd_offset + i as u64);
                }
            }
            self.write_phys_range(mem_addr, buf);
        }
        if irq_after {
            self.raise_pending_interrupt(interrupt_bit);
        }
    }

    // Advance the PIT; returns true when the timer interrupt fires. When the
    // countdown reaches 0 it reloads from PIT_RELOAD (if nonzero) and fires.
    pub fn tick_pit(&self) -> bool {
        let countdown = self.device_clock.pit_countdown.load(Ordering::SeqCst);
        if countdown != 0 {
            self.device_clock.pit_countdown.store(countdown - 1, Ordering::SeqCst);
            return false;
        }
        let reload = self.pit_reload.load(Ordering::SeqCst);
        if reload == 0 {
            return false;
        }
        self.device_clock.pit_countdown.store(reload, Ordering::SeqCst);
        true
    }

    // Advance the audio sample clock by one 100 MHz device tick. Every
    // AUDIO_TICKS_PER_SAMPLE ticks, enabled playback consumes one sample
    // (returned for the host backend) and may raise the low-water interrupt.
    // Only the expiring tick takes `mmio_lock`.
    pub fn tick_audio(&self) -> Option<i16> {
        let countdown = self.device_clock.audio_sample_countdown.load(Ordering::SeqCst);
        if countdown > 0 {
            self.device_clock.audio_sample_countdown.store(countdown - 1, Ordering::SeqCst);
            return None;
        }
        self.device_clock.audio_sample_countdown
            .store(AUDIO_TICKS_PER_SAMPLE - 1, Ordering::SeqCst);
        let _guard = self.mmio_lock.lock().unwrap();
        let mut audio = self.audio.lock().unwrap();
        if !audio.enabled() {
            return None;
        }
        let was_low_water = audio.low_water();
        let sample = audio.consume_sample_now();
        if audio.low_water_edge(was_low_water) {
            self.raise_pending_interrupt(AUDIO_INTERRUPT_BIT);
        }
        Some(sample)
    }

    // Consume `sample_count` samples immediately for wall-clock audio mode,
    // filling `out` (reused across calls) and updating READ_IDX/UNDERRUN.
    pub fn consume_audio_wallclock_samples(&self, sample_count: usize, out: &mut Vec<i16>) {
        let _guard = self.mmio_lock.lock().unwrap();
        let mut audio = self.audio.lock().unwrap();
        let was_low_water = audio.low_water();
        out.clear();
        out.extend((0..sample_count).map(|_| audio.consume_sample_now()));
        if audio.low_water_edge(was_low_water) {
            self.raise_pending_interrupt(AUDIO_INTERRUPT_BIT);
        }
    }
}

#[cfg(test)]
/*
Summary:
- Verifies SD image serialization preserves sparse/block-backed state.
- Verifies RAM is zero-filled, byte stores merge into words, and range helpers
  cross word and page boundaries.
- Verifies PIT reload and pending interrupt atomics preserve MMIO behavior.
- Verifies audio interrupt edges are detected for every MMIO write width.
- Verifies the sprite map holds exactly the 16 sprites from docs/mem_map.md.
*/
mod tests {
    use super::*;

    // Preserve the original image length when no later writes extend it.
    #[test]
    fn sd_dump_preserves_loaded_image_length() {
        let mut sd = SdCard::new(1);
        let image = [0x12, 0x34, 0x56];
        sd.load_image(&image);
        assert_eq!(sd.dump_image(), image);
    }

    // Extend an exported SD image far enough to include newly written bytes.
    #[test]
    fn sd_dump_grows_to_cover_written_bytes() {
        let mut sd = SdCard::new(1);
        sd.load_image(&[0xAA]);
        sd.write_storage_byte(511, 0xCC);

        let image = sd.dump_image();
        assert_eq!(image.len(), 512);
        assert_eq!(image[0], 0xAA);
        assert_eq!(image[511], 0xCC);
    }

    // Fill unwritten gaps with zeros when exporting sparse SD storage.
    #[test]
    fn sd_dump_zero_fills_sparse_gaps() {
        let mut sd = SdCard::new(1);
        sd.write_storage_byte(600, 0x5A);

        let image = sd.dump_image();
        assert_eq!(image.len(), 601);
        assert_eq!(image[0], 0);
        assert_eq!(image[599], 0);
        assert_eq!(image[600], 0x5A);
    }

    // Return zero when guest RAM reads a page that has never been written.
    #[test]
    fn ram_reads_zero_from_untouched_memory() {
        let memory = Memory::new(HashMap::new(), false, 1);

        assert_eq!(memory.read(0x0000_1234), 0);
        assert_eq!(memory.read_u32(0x0000_1FFC), 0);
    }

    // Distribute a loaded RAM image correctly across page boundaries.
    #[test]
    fn ram_image_initializes_multiple_pages() {
        let mut image = HashMap::new();
        image.insert(0x0000_0010, 0x12);
        image.insert(0x0000_1001, 0x34);

        let memory = Memory::new(image, false, 1);

        assert_eq!(memory.read(0x0000_0010), 0x12);
        assert_eq!(memory.read(0x0000_1001), 0x34);
        assert_eq!(memory.read(0x0000_1002), 0);
    }

    // Range helpers cross word and page boundaries and take the word fast path.
    #[test]
    fn ram_phys_range_helpers_span_page_boundaries() {
        let memory = Memory::new(HashMap::new(), false, 1);
        let expected = [0xAA, 0xBB, 0xCC, 0xDD];

        memory.write_phys_range(0x0000_0FFE, &expected);
        memory.write_phys_range(0x0000_2000, &expected);
        assert_eq!(memory.read_u32(0x0000_2000), 0xDDCC_BBAA);

        let mut actual = [0u8; 4];
        memory.read_phys_range(0x0000_0FFE, &mut actual);
        assert_eq!(actual, expected);
        assert_eq!(memory.read_u16(0x0000_1000), 0xDDCC);
    }

    // Apply the most recently written PIT reload value on the next timer cycle.
    #[test]
    fn pit_tick_uses_latest_written_reload() {
        let memory = Memory::new(HashMap::new(), false, 1);

        memory.write_u32(PIT_START, 3);

        assert!(memory.tick_pit());
        assert_eq!(memory.device_clock.pit_countdown.load(Ordering::SeqCst), 3);
        assert_eq!(memory.read_u32(PIT_START), 3);
    }

    // Narrow stores must merge into their word without disturbing neighbors.
    #[test]
    fn narrow_ram_stores_merge_into_word() {
        let memory = Memory::new(HashMap::new(), false, 1);
        memory.write_u32(0x100, 0x1122_3344);
        memory.write(0x101, 0xAB);
        memory.write_u16(0x102, 0xCDEF);
        assert_eq!(memory.read_u32(0x100), 0xCDEF_AB44);
        assert_eq!(memory.read(0x103), 0xCD);
        assert_eq!(memory.atomic_update_u32(0x100, |v| v.wrapping_add(1)), 0xCDEF_AB44);
        assert_eq!(memory.read_u16(0x100), 0xAB45);
    }

    // The clock divider is read lock-free every tick, so MMIO writes must
    // reach the atomic the CPU reads.
    #[test]
    fn clock_divider_mmio_round_trips() {
        let memory = Memory::new(HashMap::new(), false, 1);
        memory.write_u32(CLK_REG_START, 0x0102_0304);
        assert_eq!(memory.clock_divider(), 0x0102_0304);
        memory.write(CLK_REG_START + 3, 0xFF);
        assert_eq!(memory.read_u32(CLK_REG_START), 0xFF02_0304);
    }

    // Atomically take all pending interrupts and clear the published set.
    #[test]
    fn pending_interrupts_swap_and_clear() {
        let memory = Memory::new(HashMap::new(), false, 1);

        memory.raise_pending_interrupt(SD_INTERRUPT_BIT | VGA_INTERRUPT_BIT);

        assert_eq!(memory.check_interrupts(), SD_INTERRUPT_BIT | VGA_INTERRUPT_BIT);
        assert_eq!(memory.check_interrupts(), 0);
    }

    // Only 16 sprites exist; the map used to allocate 32768 of them.
    #[test]
    fn sprite_map_matches_mem_map() {
        let memory = Memory::new(HashMap::new(), false, 1);
        assert_eq!(memory.vga().sprite_pixels.read().unwrap().len(), 16 * 32 * 32 * 2);
        memory.write(SPRITE_MAP_END - 1, 0x12);
        assert_eq!(memory.read(SPRITE_MAP_END - 1), 0x12);
    }

    // Map the audio ring immediately below its control-register window.
    #[test]
    fn audio_ring_extends_mmio_downward() {
        let mut image = HashMap::new();
        image.insert(AUDIO_RING_BUFFER_START - 1, 0x11);
        image.insert(AUDIO_RING_BUFFER_START, 0x22);

        let memory = Memory::new(image, false, 1);

        assert_eq!(memory.read(AUDIO_RING_BUFFER_START - 1), 0x11);
        assert_eq!(
            memory.read(AUDIO_RING_BUFFER_START),
            0,
            "MMIO-backed audio ring bytes must not be initialized from the RAM image",
        );

        memory.write(AUDIO_RING_BUFFER_START, 0x33);
        assert_eq!(memory.read(AUDIO_RING_BUFFER_START), 0x33);
    }

    // Advance the audio consumer index and resume playback after an underrun refill.
    #[test]
    fn audio_tick_advances_read_idx_and_recovers_underrun_after_refill() {
        let memory = Memory::new(HashMap::new(), false, 1);
        let status = AUDIO_REGS_START + AUDIO_OFFSET_STATUS;

        memory.write(AUDIO_RING_BUFFER_START, 0x34);
        memory.write(AUDIO_RING_BUFFER_START + 1, 0x12);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 2);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_CTRL, AUDIO_CTRL_ENABLE);

        assert_eq!(
            memory.tick_audio(),
            Some(i16::from_le_bytes([0x34, 0x12])),
            "audio tick must return the PCM sample consumed from the MMIO ring",
        );
        assert_eq!(memory.read_u32(AUDIO_REGS_START + AUDIO_OFFSET_READ_IDX), 2);
        assert_eq!(memory.read_u32(status) & AUDIO_STATUS_UNDERRUN, 0);

        let mut underrun_sample = None;
        for _ in 0..AUDIO_TICKS_PER_SAMPLE {
            underrun_sample = memory.tick_audio();
        }

        assert_eq!(underrun_sample, Some(0), "audio underrun must output signed-zero samples");
        assert_ne!(
            memory.read_u32(status) & AUDIO_STATUS_UNDERRUN,
            0,
            "enabled playback must latch UNDERRUN after the ring becomes empty",
        );
        assert_eq!(
            memory.read_u32(AUDIO_REGS_START + AUDIO_OFFSET_READ_IDX),
            2,
            "the device must not advance READ_IDX while outputting underrun silence",
        );

        memory.write(AUDIO_RING_BUFFER_START + 2, 0x78);
        memory.write(AUDIO_RING_BUFFER_START + 3, 0x56);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 4);
        assert_eq!(
            memory.read_u32(status) & AUDIO_STATUS_UNDERRUN,
            0,
            "publishing another sample must clear UNDERRUN automatically",
        );
    }

    // Raise one audio interrupt when buffered data first crosses below the watermark.
    #[test]
    fn audio_irq_fires_once_on_low_water_rising_edge() {
        let memory = Memory::new(HashMap::new(), false, 1);

        memory.write(AUDIO_RING_BUFFER_START, 0x34);
        memory.write(AUDIO_RING_BUFFER_START + 1, 0x12);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 2);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WATERMARK, 0);
        memory.write_u32(
            AUDIO_REGS_START + AUDIO_OFFSET_CTRL,
            AUDIO_CTRL_ENABLE | AUDIO_CTRL_IRQ_ENABLE,
        );

        assert_eq!(
            memory.check_interrupts(),
            0,
            "enabling IRQ while LOW_WATER is false must not synthesize an interrupt",
        );

        assert_eq!(memory.tick_audio(), Some(i16::from_le_bytes([0x34, 0x12])));

        assert_eq!(memory.check_interrupts(), AUDIO_INTERRUPT_BIT);
        assert_eq!(
            memory.check_interrupts(),
            0,
            "audio IRQ must be edge-triggered rather than level-triggered",
        );
    }

    // A byte-wide store that drops the buffered level to the watermark must
    // raise the interrupt too; only halfword/word stores used to.
    #[test]
    fn audio_irq_edge_detected_for_byte_writes() {
        let memory = Memory::new(HashMap::new(), false, 1);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 8);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WATERMARK, 4);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_CTRL, AUDIO_CTRL_IRQ_ENABLE);
        assert_eq!(memory.check_interrupts(), 0);

        memory.write(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 2);

        assert_eq!(memory.check_interrupts(), AUDIO_INTERRUPT_BIT);
    }

    // Do not synthesize a past low-water edge when interrupts are enabled late.
    #[test]
    fn audio_enabling_irq_while_low_water_is_already_true_does_not_backfill_interrupt() {
        let memory = Memory::new(HashMap::new(), false, 1);

        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WATERMARK, 0);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_CTRL, AUDIO_CTRL_IRQ_ENABLE);

        assert_eq!(
            memory.check_interrupts(),
            0,
            "enabling IRQ after LOW_WATER asserted must not backfill an interrupt",
        );
    }

    // Treat underrun as playback state rather than a second interrupt source.
    #[test]
    fn audio_underrun_does_not_raise_a_separate_interrupt() {
        let memory = Memory::new(HashMap::new(), false, 1);

        memory.write(AUDIO_RING_BUFFER_START, 0x34);
        memory.write(AUDIO_RING_BUFFER_START + 1, 0x12);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 2);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WATERMARK, 0);
        memory.write_u32(
            AUDIO_REGS_START + AUDIO_OFFSET_CTRL,
            AUDIO_CTRL_ENABLE | AUDIO_CTRL_IRQ_ENABLE,
        );

        assert_eq!(memory.tick_audio(), Some(i16::from_le_bytes([0x34, 0x12])));
        assert_eq!(memory.check_interrupts(), AUDIO_INTERRUPT_BIT);

        for _ in 0..AUDIO_TICKS_PER_SAMPLE {
            let _ = memory.tick_audio();
        }

        assert_eq!(
            memory.check_interrupts(),
            0,
            "UNDERRUN must not raise a second audio interrupt once LOW_WATER is already active",
        );
    }

    // Let wall-clock audio consumption progress independently of device ticks.
    #[test]
    fn wallclock_audio_consumption_advances_without_waiting_for_device_ticks() {
        let memory = Memory::new(HashMap::new(), false, 1);
        let mut samples = Vec::new();

        memory.write(AUDIO_RING_BUFFER_START, 0x34);
        memory.write(AUDIO_RING_BUFFER_START + 1, 0x12);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_WRITE_IDX, 2);
        memory.write_u32(AUDIO_REGS_START + AUDIO_OFFSET_CTRL, AUDIO_CTRL_ENABLE);

        memory.consume_audio_wallclock_samples(1, &mut samples);

        assert_eq!(
            samples,
            vec![i16::from_le_bytes([0x34, 0x12])],
            "wall-clock audio mode must emit the queued sample immediately",
        );
        assert_eq!(
            memory.read_u32(AUDIO_REGS_START + AUDIO_OFFSET_READ_IDX),
            2,
            "wall-clock audio mode must advance READ_IDX by one sample",
        );
    }

    // A guest word load at the mouse stream returns one whole event and
    // consumes it; an empty stream reads 0 and keeps the pending flag clear.
    #[test]
    fn mouse_stream_word_read_pops_one_event() {
        let memory = Memory::new(HashMap::new(), false, 1);
        assert_eq!(memory.read_u32(MOUSE_STREAM_START), 0);
        assert!(!memory.has_pending_mouse());

        memory.push_mouse(crate::mouse::MOUSE_BUTTON_LEFT, -3, 5, 1);
        memory.push_mouse(0, 0, 0, 0);
        assert!(memory.has_pending_mouse());

        assert_eq!(memory.read_u32(MOUSE_STREAM_START), 0x0105_FD09);
        assert_eq!(memory.read(MOUSE_STREAM_START), 0x08);
        assert!(memory.has_pending_mouse());
        assert_eq!(memory.read_u32(MOUSE_STREAM_START), 0x0000_0008);
        assert!(!memory.has_pending_mouse());
        assert_eq!(memory.read_u32(MOUSE_STREAM_START), 0);
    }

    // Both bytes of the keyboard stream are input-only; the high byte used
    // to fall through to the generic "unmapped IO" panic.
    #[test]
    #[should_panic(expected = "read-only PS/2 keyboard stream (address 0x07FE5801")]
    fn ps2_stream_high_byte_rejects_writes() {
        let memory = Memory::new(HashMap::new(), false, 1);
        memory.write(PS2_STREAM_HIGH, 0);
    }

    // The mouse stream is input-only; a store is a guest bug.
    #[test]
    #[should_panic(expected = "read-only PS/2 mouse stream")]
    fn mouse_stream_rejects_writes() {
        let memory = Memory::new(HashMap::new(), false, 1);
        memory.write_u32(MOUSE_STREAM_START, 0);
    }
}
