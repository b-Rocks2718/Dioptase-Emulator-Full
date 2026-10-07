// One Dioptase core: architectural register state, instruction execution,
// address translation, and exception/interrupt entry. Multicore run setup
// lives in `run`, the interrupt controller in `interrupts`.
//
// References: docs/ISA.md (instructions, control registers, exceptions),
// docs/mem_map.md (IVT and MMIO), Dioptase-OS/docs/kernel_mem_map.md (kernel
// regions used for trace labels and the profiler's dense PC table).
//
// Exception/interrupt entry (docs/ISA.md): EPC <- resume PC, EFG <- FLG,
// IMR[31] <- 0, PSR <- PSR + 1 (kernel mode is PSR != 0), PC <- IVT[vector].
// `rfe` reverses this and sets IMR[31].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::AudioSink;
use crate::isa::{
    OPC_ALU, OPC_ALU_IMM, OPC_LUI, OPC_MEM_WORD_ABS, OPC_MEM_BYTE_IMM, OPC_BRANCH_IMM,
    OPC_BRANCH_ABS_REG, OPC_BRANCH_REL_REG, OPC_TRAP, OPC_FETCH_ADD_ABS, OPC_FETCH_ADD_IMM,
    OPC_SWAP_ABS, OPC_SWAP_IMM, OPC_ADPC, OPC_PRIVILEGED, OP_SUB, OP_SUBB,
};
use crate::memory::{Memory, PHYSMEM_MAX, SdSlot};

mod alu;
mod debugger;
mod interrupts;
pub mod profiler;
mod program;
mod run;
mod tlb;

use interrupts::{IPI_INTERRUPT_BIT, InterruptController, format_interrupts};
use profiler::CoreProfile;
use program::{
    DebugInfo, DebugLine, DebugLocal, LabelMap, load_program, parse_debug_line, parse_label_line,
    read_lines,
};
use tlb::{Access, TLB_FAULT_ABSENT, Tlb, TlbAccess};

pub use run::{RunConfig, RunResult, ScheduleMode, run_program};

// Reset vector for kernel entry (docs/mem_map.md).
const RESET_PC: u32 = 0x0000_0400;

// Kernel memory-map regions from Dioptase-OS/docs/kernel_mem_map.md as
// (start, exclusive end, name). They only label trace output and bound the
// profiler's dense PC table; they do not change emulated behavior.
const KERNEL_REGIONS: [(u32, u32, &str); 8] = [
    (0x0000_0000, 0x0000_0400, "ivt"),
    // 32 KiB BIOS image and MBR buffer; the kernel may reuse it after entry.
    (0x0000_0400, 0x0000_8400, "bios"),
    // Boot-core BIOS stack; grows down from BIOS_STACK_TOP in bios/init.s.
    (0x0000_8400, 0x0001_0000, "bios_stack"),
    (0x0001_0000, 0x000B_0000, "kernel_text"),
    (0x000B_0000, 0x000E_0000, "kernel_data"),
    (0x000E_0000, 0x000E_8000, "kernel_rodata"),
    (0x000E_8000, 0x000F_0000, "kernel_bss"),
    // Per-core 16 KiB kernel stacks: core 3 at the bottom, core 0 at the top.
    (0x000F_0000, 0x0010_0000, "kernel_stack"),
];
// Top of the kernel image regions; bounds the profiler's dense PC table.
const KERNEL_STACK_END: u32 = 0x0010_0000;

// Control register indices (docs/ISA.md "Control registers").
const CREG_PSR: usize = 0;
const CREG_PID: usize = 1;
const CREG_ISR: usize = 2;
const CREG_IMR: usize = 3;
const CREG_EPC: usize = 4;
const CREG_FLG: usize = 5;
const CREG_EFG: usize = 6;
const CREG_TLB: usize = 7;
const CREG_KSP: usize = 8;
const CREG_CID: usize = 9;
const CREG_TLBF: usize = 12;
// One past the highest architectural control register; cr10 and cr11 are
// reserved holes inside this range.
const CREG_COUNT: usize = 13;
// Debugger names for each control register, by index. `None` marks a reserved
// number (cr10/cr11, formerly the IPI mailboxes) that crmv rejects.
const CREG_NAMES: [Option<&str>; CREG_COUNT] = [
    Some("psr"), Some("pid"), Some("isr"), Some("imr"), Some("epc"), Some("flg"),
    Some("efg"), Some("tlb"), Some("ksp"), Some("cid"), None, None, Some("tlbf"),
];

// Whether `idx` names a control register defined by docs/ISA.md.
fn creg_defined(idx: usize) -> bool {
    CREG_NAMES.get(idx).is_some_and(|name| name.is_some())
}

// IMR[31] globally enables interrupts.
const IMR_GLOBAL_ENABLE: u32 = 1 << 31;
// Secondary cores start able to take only the wake-up IPI.
const SECONDARY_CORE_IMR: u32 = IMR_GLOBAL_ENABLE | IPI_INTERRUPT_BIT;

// FLG bits: carry | zero | sign | overflow.
const FLAG_CARRY: u32 = 1 << 0;
const FLAG_ZERO: u32 = 1 << 1;
const FLAG_SIGN: u32 = 1 << 2;
const FLAG_OVERFLOW: u32 = 1 << 3;
const ALU_FLAGS: u32 = FLAG_CARRY | FLAG_ZERO | FLAG_SIGN | FLAG_OVERFLOW;

// IVT word indices (docs/ISA.md "Exceptions" and "Interrupts").
const VEC_TRAP: u32 = 0x01;
const VEC_INVALID_INSTR: u32 = 0x80;
const VEC_PRIVILEGE: u32 = 0x81;
const VEC_TLB_MISS: u32 = 0x82;
const VEC_MISALIGNED_PC: u32 = 0x84;
// Interrupt line n (0..=15) vectors through IVT[0xF0 + n]; higher lines win.
const VEC_INTERRUPT_BASE: u32 = 0xF0;
const INTERRUPT_LINES_MASK: u32 = 0xFFFF;

// General-purpose register aliased to KSP while in kernel mode.
const SP_REG: u32 = 31;
// tlbw keeps the low 27 bits of rA (PPN and flags).
const TLB_ENTRY_MASK: u32 = 0x07FF_FFFF;
// trap encodings with any of these bits set are reserved.
const TRAP_RESERVED_MASK: u32 = 0x07FF_FFFF;

// Global toggle for `--trace-ints` output.
static TRACE_INTERRUPTS: AtomicBool = AtomicBool::new(false);

// Enable or disable process-wide interrupt trace output.
pub fn set_trace_interrupts(enabled: bool) {
    TRACE_INTERRUPTS.store(enabled, Ordering::Relaxed);
}

// Whether `--trace-ints` output is enabled.
fn tracing() -> bool {
    TRACE_INTERRUPTS.load(Ordering::Relaxed)
}

// Field extractors shared by most instruction formats.
fn field_a(instr: u32) -> u32 {
    (instr >> 22) & 0x1F
}
fn field_b(instr: u32) -> u32 {
    (instr >> 17) & 0x1F
}

// Access width of a load or store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Width {
    Byte,
    Half,
    Word,
}

impl Width {
    fn bytes(self) -> u32 {
        match self {
            Width::Byte => 1,
            Width::Half => 2,
            Width::Word => 4,
        }
    }
}

// Address formation shared by the memory and atomic instruction groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddrMode {
    // rB + imm (memory forms also support pre/post-increment)
    Absolute,
    // rB + imm + PC + 4
    Relative,
    // imm + PC + 4
    Immediate,
}

// Read-modify-write operation performed by an atomic instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtomicOp {
    FetchAdd,
    Swap,
}

// Selects which kinds of access cause a watchpoint to fire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchKind {
    Read,
    Write,
    ReadWrite,
}

// Records whether a watchpoint was triggered by a read or write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchAccess {
    Read,
    Write,
}

// Single-byte watchpoint tracked by exact virtual address.
#[derive(Clone, Copy, Debug)]
struct Watchpoint {
    addr: u32,
    kind: WatchKind,
}

// Describes the memory access that triggered a watchpoint.
#[derive(Clone, Copy, Debug)]
struct WatchpointHit {
    addr: u32,
    access: WatchAccess,
    value: u8,
}

// What one attempt to run the next instruction did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepOutcome {
    Executed { pc: u32, instr: u32 },
    Sleeping,
    TlbMiss { pc: u32 },
    MisalignedPc { pc: u32 },
}

// Owns one core's CPU state while sharing memory and devices with other cores.
pub struct Emulator {
    regfile: [u32; 32],
    cregfile: [u32; CREG_COUNT],
    memory: Arc<Memory>,
    interrupts: Arc<InterruptController>,
    tlb: Tlb,
    pc: u32,
    asleep: bool,
    // Set by `mode sleep` so the waking interrupt resumes after the sleep
    // instruction; secondary cores that start asleep resume at their PC.
    sleep_armed: bool,
    halted: bool,
    // Ticks since the run started (the --max-cycles budget and clock divider).
    count: u32,
    core_id: u32,
    // Whether this core advances the audio device on emulated ticks (core 0
    // only, and not in wall-clock audio mode).
    ticks_audio: bool,
    audio_sink: Option<Arc<AudioSink>>,
    // TLBF value for the translation fault raised by the current access.
    pending_tlb_fault: Option<u32>,
    watchpoints: Vec<Watchpoint>,
    watchpoint_hit: Option<WatchpointHit>,
    // Present only when `--profile` is enabled; see profiler.rs.
    profile: Option<CoreProfile>,
}

// Build guest memory from a program image and optional raw SD images.
fn build_memory(
    bytes: HashMap<u32, u8>,
    use_uart_rx: bool,
    sd_dma_ticks_per_word: u32,
    sd0_image: Option<&[u8]>,
    sd1_image: Option<&[u8]>,
) -> Arc<Memory> {
    let memory = Memory::new(bytes, use_uart_rx, sd_dma_ticks_per_word);
    for (slot, image) in [
        (SdSlot::Sd0, sd0_image),
        (SdSlot::Sd1, sd1_image),
    ] {
        if let Some(image) = image {
            memory.load_sd_image(slot, image);
        }
    }
    Arc::new(memory)
}

impl Emulator {
    // Create a single-core emulator with its own memory and devices; used by
    // the interactive debuggers.
    fn standalone(
        bytes: HashMap<u32, u8>,
        use_uart_rx: bool,
        sd_dma_ticks_per_word: u32,
        sd0_image: Option<&[u8]>,
        sd1_image: Option<&[u8]>,
    ) -> Emulator {
        let memory = build_memory(bytes, use_uart_rx, sd_dma_ticks_per_word, sd0_image, sd1_image);
        Emulator::from_shared(memory, InterruptController::new(1, use_uart_rx), 0)
    }

    // Export one SD device as a raw host image.
    pub fn dump_sd_image(&self, slot: SdSlot) -> Vec<u8> {
        self.memory.dump_sd_image(slot)
    }

    // Attach a core to existing shared memory and interrupt state. Cores start
    // in kernel mode at the reset vector; secondaries start asleep waiting for
    // an IPI.
    fn from_shared(memory: Arc<Memory>, interrupts: Arc<InterruptController>, core_id: u32) -> Emulator {
        let mut cregfile = [0; CREG_COUNT];
        cregfile[CREG_PSR] = 1;
        cregfile[CREG_CID] = core_id;
        if core_id != 0 {
            cregfile[CREG_IMR] = SECONDARY_CORE_IMR;
        }
        Emulator {
            regfile: [0; 32],
            cregfile,
            memory,
            interrupts,
            tlb: Tlb::new(),
            pc: RESET_PC,
            asleep: core_id != 0,
            sleep_armed: false,
            halted: false,
            count: 0,
            core_id,
            ticks_audio: core_id == 0,
            audio_sink: None,
            pending_tlb_fault: None,
            watchpoints: Vec::new(),
            watchpoint_hit: None,
            profile: None,
        }
    }

    // Kernel mode is derived from the PSR depth, not a cached flag.
    fn get_kmode(&self) -> bool {
        self.cregfile[CREG_PSR] != 0
    }

    // Name the kernel memory-map region containing a physical address.
    fn memmap_region(paddr: u32) -> Option<&'static str> {
        KERNEL_REGIONS
            .iter()
            .find(|(start, end, _)| (*start..*end).contains(&paddr))
            .map(|(_, _, name)| *name)
    }

    // ---- Registers ------------------------------------------------------

    // Read a general-purpose register; r31 reads KSP in kernel mode.
    fn get_reg(&self, regnum: u32) -> u32 {
        if regnum == SP_REG && self.get_kmode() {
            self.cregfile[CREG_KSP]
        } else {
            self.regfile[regnum as usize]
        }
    }

    // Write a general-purpose register; r0 is hardwired to zero and r31
    // writes KSP in kernel mode.
    fn write_reg(&mut self, regnum: u32, value: u32) {
        if regnum == SP_REG && self.get_kmode() {
            self.cregfile[CREG_KSP] = value;
        } else if regnum != 0 {
            self.regfile[regnum as usize] = value;
        }
    }

    // Read a control register.
    fn read_creg(&self, idx: usize) -> u32 {
        self.cregfile[idx]
    }

    // Write a control register through `crmv`. ISR and CID are read-only;
    // interrupts are acknowledged through `eoi`.
    fn write_creg(&mut self, idx: usize, value: u32) {
        match idx {
            CREG_ISR | CREG_CID => {
                println!("Warning: attempt to write read-only register cr{}", idx);
            }
            _ => {
                if idx == CREG_PSR && tracing() {
                    println!(
                        "[core {}] psr write {:08X} -> {:08X} (crmv pc=0x{:08X})",
                        self.core_id, self.cregfile[CREG_PSR], value, self.pc
                    );
                }
                self.cregfile[idx] = value;
            }
        }
    }

    // Update ISR without dropping interrupts that became pending in the
    // controller during software's read-modify-write (matches the hardware
    // cregfile). Cleared bits are reported to the controller so input routing
    // and IPI delivery reopen.
    fn write_isr(&mut self, value: u32) {
        let core = self.core_id as usize;
        let old = self.cregfile[CREG_ISR];
        let pending = self.interrupts.peek_pending(core);
        self.cregfile[CREG_ISR] = value | pending;
        let cleared = old & !self.cregfile[CREG_ISR];
        if cleared != 0 {
            self.interrupts.acknowledge(core, cleared);
        }
    }

    // Advance to the next sequential instruction.
    fn advance(&mut self) {
        self.pc = self.pc.wrapping_add(4);
    }

    // ---- Exceptions and interrupts ---------------------------------------

    // Increment PSR on handler entry.
    fn psr_inc(&mut self, reason: &str) {
        let old = self.cregfile[CREG_PSR];
        assert!(old != u32::MAX, "core {}: PSR overflow entering {} handler at pc 0x{:08X}", self.core_id, reason, self.pc);
        self.cregfile[CREG_PSR] = old + 1;
        if tracing() {
            println!(
                "[core {}] psr inc {:08X} -> {:08X} ({} pc=0x{:08X})",
                self.core_id, old, old + 1, reason, self.pc
            );
        }
    }

    // Enter a handler: snapshot EPC/EFG, disable interrupts, raise PSR, and
    // jump through the IVT. After `psr_inc` the core is in kernel mode, so the
    // vector is read physically (it cannot fault or hit watchpoints).
    fn enter_handler(&mut self, vector: u32, epc: u32, reason: &str) {
        self.cregfile[CREG_EPC] = epc;
        self.cregfile[CREG_EFG] = self.cregfile[CREG_FLG];
        self.cregfile[CREG_IMR] &= !IMR_GLOBAL_ENABLE;
        self.psr_inc(reason);
        self.pc = self.memory.read_u32(vector * 4);
    }

    // Raise a synchronous exception that resumes at the faulting instruction.
    fn raise_exception(&mut self, vector: u32, reason: &str) {
        if tracing() {
            println!(
                "[core {}] exception {} pc=0x{:08X} psr=0x{:08X}",
                self.core_id, reason, self.pc, self.cregfile[CREG_PSR]
            );
        }
        self.enter_handler(vector, self.pc, reason);
    }

    // Raise an invalid-instruction exception for the current instruction.
    fn raise_invalid_instruction(&mut self) {
        self.raise_exception(VEC_INVALID_INSTR, "invalid_instr");
    }

    // Raise a TLB miss for `addr` using the fault flags recorded by the
    // failed translation (0 when the access failed for another reason).
    fn raise_pending_tlb_miss(&mut self, addr: u32) {
        let flags = self.pending_tlb_fault.take().unwrap_or(TLB_FAULT_ABSENT);
        if tracing() {
            println!(
                "[core {}] exception tlb_miss mode={} addr=0x{:08X} flags=0x{:08X} pc=0x{:08X} psr=0x{:08X}",
                self.core_id,
                if self.get_kmode() { "kernel" } else { "user" },
                addr,
                flags,
                self.pc,
                self.cregfile[CREG_PSR]
            );
        }
        self.cregfile[CREG_TLB] = (addr >> 12) | (self.cregfile[CREG_PID] << 20);
        self.cregfile[CREG_TLBF] = flags;
        self.enter_handler(VEC_TLB_MISS, self.pc, "tlb_miss");
    }

    // Take the controller's pending bits into ISR; core 0 also advances the
    // shared devices first. Device interrupts raised this tick become visible
    // on the next tick.
    fn collect_interrupts(&mut self) {
        let core = self.core_id as usize;
        self.interrupts
            .dispatch_input(self.memory.has_pending_input(), self.memory.has_pending_mouse());
        if core == 0 {
            self.tick_devices();
        }
        let pending = self.interrupts.take_pending(core);
        if pending != 0 {
            self.cregfile[CREG_ISR] |= pending;
        }
    }

    // Advance the shared PIT, SD DMA engines, and audio device by one tick.
    // Only core 0 calls this.
    fn tick_devices(&mut self) {
        self.interrupts
            .dispatch_device_interrupts(self.memory.check_interrupts());
        if self.memory.tick_pit() {
            self.interrupts.broadcast_timer();
        }
        self.memory.tick_sd_dma();
        if self.ticks_audio
            && let Some(sample) = self.memory.tick_audio()
                && let Some(sink) = &self.audio_sink {
                    sink.write_sample(sample);
                }
    }

    // Enter the highest-numbered enabled interrupt, waking the core if needed.
    fn handle_interrupts(&mut self) {
        let imr = self.cregfile[CREG_IMR];
        if imr & IMR_GLOBAL_ENABLE == 0 {
            return;
        }
        // Only lines 0..=15 have vectors; the controller never raises others.
        let active = imr & self.cregfile[CREG_ISR] & INTERRUPT_LINES_MASK;
        if active == 0 {
            return;
        }
        if tracing() {
            println!(
                "[core {}] interrupt {} (active={:08X} imr={:08X} pc={:08X})",
                self.core_id,
                format_interrupts(active),
                active,
                imr,
                self.pc
            );
        }
        // Waking from `mode sleep` resumes after the sleep instruction.
        if self.asleep && self.sleep_armed {
            self.advance();
        }
        self.asleep = false;
        self.sleep_armed = false;

        let line = 31 - active.leading_zeros();
        self.enter_handler(VEC_INTERRUPT_BASE + line, self.pc, "interrupt");
    }

    // Collect and deliver interrupts at the start of a tick.
    fn service_interrupts(&mut self) {
        self.collect_interrupts();
        self.handle_interrupts();
    }

    // ---- Memory access ---------------------------------------------------

    // Translate a virtual address. Kernel-mode addresses inside physical
    // memory bypass the TLB. On a fault the TLBF bits are recorded for
    // `raise_pending_tlb_miss` and None is returned.
    fn translate(&mut self, vaddr: u32, access: Access) -> Option<u32> {
        let kmode = self.get_kmode();
        if kmode && vaddr <= PHYSMEM_MAX {
            return Some(vaddr);
        }
        match self
            .tlb
            .access(self.cregfile[CREG_PID], vaddr >> 12, access, kmode)
        {
            TlbAccess::Hit(page) => Some(page | (vaddr & 0xFFF)),
            TlbAccess::Fault(flags) => {
                self.pending_tlb_fault = Some(flags);
                None
            }
        }
    }

    // Emit the unaligned/null warnings for a data access and return the
    // address with its low bits cleared (accesses are naturally aligned).
    fn align_data_addr(&mut self, addr: u32, width: Width, verb: &str) -> u32 {
        self.pending_tlb_fault = None;
        let size = width.bytes();
        if size > 1 && addr & (size - 1) != 0 {
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        if addr == 0 {
            println!(
                "Warning: core {} {} virtual address 0x00000000 from pc 0x{:08X}",
                self.core_id, verb, self.pc
            );
        }
        addr & !(size - 1)
    }

    // Under --trace-ints, warn about stores into read-only kernel regions.
    // Regions are at least 1 KiB aligned, so the first byte decides.
    fn trace_protected_write(&self, vaddr: u32, paddr: u32, size: u32) {
        if !tracing() {
            return;
        }
        if let Some(region @ ("kernel_text" | "kernel_rodata" | "bios" | "ivt")) =
            Self::memmap_region(paddr)
        {
            println!(
                "[core {}] Warning: write to {} vaddr=0x{:08X} paddr=0x{:08X} size={} pc=0x{:08X}",
                self.core_id, region, vaddr, paddr, size, self.pc
            );
        }
    }

    // Record the first watchpoint hit among the bytes of an access.
    fn watch(&mut self, vaddr: u32, access: WatchAccess, bytes: &[u8]) {
        if self.watchpoints.is_empty() {
            return;
        }
        for (i, value) in bytes.iter().enumerate() {
            if self.watchpoint_hit.is_some() {
                return;
            }
            let addr = vaddr.wrapping_add(i as u32);
            let hit = self.watchpoints.iter().any(|wp| {
                wp.addr == addr
                    && matches!(
                        (wp.kind, access),
                        (WatchKind::ReadWrite, _)
                            | (WatchKind::Read, WatchAccess::Read)
                            | (WatchKind::Write, WatchAccess::Write)
                    )
            });
            if hit {
                self.watchpoint_hit = Some(WatchpointHit {
                    addr,
                    access,
                    value: *value,
                });
            }
        }
    }

    // Load `width` bytes (zero-extended). None means a translation fault.
    fn load(&mut self, addr: u32, width: Width) -> Option<u32> {
        let vaddr = self.align_data_addr(addr, width, "reading from");
        let paddr = self.translate(vaddr, Access::Read)?;
        let value = match width {
            Width::Byte => u32::from(self.memory.read(paddr)),
            Width::Half => u32::from(self.memory.read_u16(paddr)),
            Width::Word => self.memory.read_u32(paddr),
        };
        let bytes = value.to_le_bytes();
        self.watch(vaddr, WatchAccess::Read, &bytes[..width.bytes() as usize]);
        Some(value)
    }

    // Store the low `width` bytes of `value`. False means a translation fault.
    fn store(&mut self, addr: u32, width: Width, value: u32) -> bool {
        let vaddr = self.align_data_addr(addr, width, "writing to");
        let Some(paddr) = self.translate(vaddr, Access::Write) else {
            return false;
        };
        self.trace_protected_write(vaddr, paddr, width.bytes());
        let bytes = value.to_le_bytes();
        self.watch(vaddr, WatchAccess::Write, &bytes[..width.bytes() as usize]);
        match width {
            Width::Byte => self.memory.write(paddr, value as u8),
            Width::Half => self.memory.write_u16(paddr, value as u16),
            Width::Word => self.memory.write_u32(paddr, value),
        }
        true
    }

    // Atomic 32-bit read-modify-write; returns the previous value. The page
    // must be both readable and writable through the same mapping.
    fn atomic_rmw(&mut self, addr: u32, op: AtomicOp, operand: u32) -> Option<u32> {
        self.pending_tlb_fault = None;
        if addr & 3 != 0 {
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        let vaddr = addr & !3;
        let read_addr = self.translate(vaddr, Access::Read)?;
        let write_addr = self.translate(vaddr, Access::Write)?;
        if read_addr != write_addr {
            return None;
        }
        self.trace_protected_write(vaddr, write_addr, 4);
        let prev = self.memory.atomic_update_u32(read_addr, |prev| match op {
            AtomicOp::FetchAdd => prev.wrapping_add(operand),
            AtomicOp::Swap => operand,
        });
        let next = match op {
            AtomicOp::FetchAdd => prev.wrapping_add(operand),
            AtomicOp::Swap => operand,
        };
        let (prev_bytes, next_bytes) = (prev.to_le_bytes(), next.to_le_bytes());
        for i in 0..4 {
            let byte_addr = vaddr + i as u32;
            self.watch(byte_addr, WatchAccess::Read, &prev_bytes[i..=i]);
            self.watch(byte_addr, WatchAccess::Write, &next_bytes[i..=i]);
        }
        Some(prev)
    }

    // Read an instruction word for display without raising exceptions.
    fn peek_instruction(&mut self, vaddr: u32) -> Option<u32> {
        if vaddr & 3 != 0 {
            return None;
        }
        let paddr = self.translate(vaddr, Access::Execute);
        self.pending_tlb_fault = None;
        Some(self.memory.read_u32(paddr?))
    }

    // ---- Execution -------------------------------------------------------

    // Fetch and execute the instruction at PC, raising misaligned-PC or
    // TLB-miss exceptions instead when the fetch fails.
    fn execute_next(&mut self) -> StepOutcome {
        let pc = self.pc;
        self.pending_tlb_fault = None;
        if pc & 3 != 0 {
            if tracing() {
                println!(
                    "[core {}] exception misaligned_pc pc=0x{:08X} psr=0x{:08X}",
                    self.core_id, pc, self.cregfile[CREG_PSR]
                );
            }
            self.enter_handler(VEC_MISALIGNED_PC, pc, "misaligned_pc");
            return StepOutcome::MisalignedPc { pc };
        }
        if pc == 0 {
            println!("Warning: fetching from virtual address 0x00000000");
        }
        let Some(paddr) = self.translate(pc, Access::Execute) else {
            self.raise_pending_tlb_miss(pc);
            return StepOutcome::TlbMiss { pc };
        };
        let instr = self.memory.read_u32(paddr);
        if let Some(profile) = self.profile.as_mut() {
            // Sample mode, PID, and link register before `execute` changes them.
            let kmode = self.cregfile[CREG_PSR] != 0;
            let link = self.regfile[profiler::LINK_REGISTER];
            profile.record_instruction(pc, instr, kmode, self.cregfile[CREG_PID], link);
        }
        self.execute(instr);
        StepOutcome::Executed { pc, instr }
    }

    // Advance the core and (on core 0) the shared devices by one tick. The
    // clock-divider register gates instruction issue to every (div + 1)th tick.
    fn tick(&mut self) {
        self.service_interrupts();
        // Sleep state after interrupt delivery decides whether this tick issues.
        let asleep = self.asleep;
        let divider = self.memory.clock_divider().wrapping_add(1).max(1);
        if !asleep && self.count.is_multiple_of(divider) {
            self.execute_next();
        }
        self.count = self.count.wrapping_add(1);
        // Counted after execute so the tick agrees with any window transition
        // this tick's instruction caused.
        if let Some(profile) = self.profile.as_mut() {
            profile.record_tick(asleep);
        }
    }

    // Debugger single step: deliver interrupts, then run one instruction
    // unless asleep. Ignores the clock divider.
    fn step_instruction(&mut self) -> StepOutcome {
        self.service_interrupts();
        if self.asleep {
            return StepOutcome::Sleeping;
        }
        let outcome = self.execute_next();
        self.count = self.count.wrapping_add(1);
        outcome
    }

    // Decode the opcode (top 5 bits) and execute one instruction.
    fn execute(&mut self, instr: u32) {
        const WIDTHS: [Width; 3] = [Width::Word, Width::Half, Width::Byte];
        const MODES: [AddrMode; 3] = [AddrMode::Absolute, AddrMode::Relative, AddrMode::Immediate];
        let opcode = instr >> 27;
        match opcode {
            OPC_ALU => self.alu_instr(instr, false),
            OPC_ALU_IMM => self.alu_instr(instr, true),
            OPC_LUI => {
                // lui: rA <- imm22 << 10
                self.write_reg(field_a(instr), (instr & 0x3F_FFFF) << 10);
                self.advance();
            }
            OPC_MEM_WORD_ABS..=OPC_MEM_BYTE_IMM => {
                let group = (opcode - OPC_MEM_WORD_ABS) as usize;
                self.mem_instr(instr, WIDTHS[group / 3], MODES[group % 3]);
            }
            OPC_BRANCH_IMM => self.branch_imm(instr),
            OPC_BRANCH_ABS_REG => self.branch_reg(instr, false),
            OPC_BRANCH_REL_REG => self.branch_reg(instr, true),
            OPC_TRAP => self.trap_instr(instr),
            OPC_FETCH_ADD_ABS..=OPC_FETCH_ADD_IMM => self.atomic_instr(instr, AtomicOp::FetchAdd, MODES[(opcode - OPC_FETCH_ADD_ABS) as usize]),
            OPC_SWAP_ABS..=OPC_SWAP_IMM => self.atomic_instr(instr, AtomicOp::Swap, MODES[(opcode - OPC_SWAP_ABS) as usize]),
            OPC_ADPC => {
                // adpc: rA <- PC + 4 + sext(imm22)
                let imm = alu::sign_extend(instr & 0x3F_FFFF, 22);
                self.write_reg(field_a(instr), self.pc.wrapping_add(4).wrapping_add(imm));
                self.advance();
            }
            OPC_PRIVILEGED => self.kernel_instr(instr),
            _ => self.raise_invalid_instruction(),
        }
    }

    // ALU instruction; the second operand is rC or a decoded immediate.
    fn alu_instr(&mut self, instr: u32, imm_form: bool) {
        let r_a = field_a(instr);
        let lhs = self.get_reg(field_b(instr));
        let (op, rhs) = if imm_form {
            let op = (instr >> 12) & 0x1F;
            match alu::decode_imm(op, instr & 0xFFF) {
                Some(imm) => (op, imm),
                None => return self.raise_invalid_instruction(),
            }
        } else {
            ((instr >> 5) & 0x1F, self.get_reg(instr & 0x1F))
        };
        let carry_in = self.cregfile[CREG_FLG] & FLAG_CARRY != 0;
        let Some(out) = alu::evaluate(op, lhs, rhs, carry_in, imm_form) else {
            return self.raise_invalid_instruction();
        };
        // Immediate subtractions compute imm - rB, so V uses that order.
        let reversed = imm_form && (op == OP_SUB || op == OP_SUBB);
        let (a, b) = if reversed { (rhs, lhs) } else { (lhs, rhs) };
        let mut flags = 0;
        if out.carry {
            flags |= FLAG_CARRY;
        }
        if out.value == 0 {
            flags |= FLAG_ZERO;
        }
        if out.value >> 31 != 0 {
            flags |= FLAG_SIGN;
        }
        if alu::overflow(op, a, b, out.value) {
            flags |= FLAG_OVERFLOW;
        }
        self.write_reg(r_a, out.value);
        self.cregfile[CREG_FLG] = (self.cregfile[CREG_FLG] & !ALU_FLAGS) | flags;
        self.advance();
    }

    // Load/store with absolute, PC-relative, or immediate addressing.
    fn mem_instr(&mut self, instr: u32, width: Width, mode: AddrMode) {
        let r_a = field_a(instr);
        let (addr, is_load, writeback) = match mode {
            AddrMode::Absolute => {
                // op | rA | rB | load | y(2) | z(2) | imm12; imm is shifted by z.
                // y: 0 = offset, 1 = pre-increment, 2 = post-increment. y = 3
                // is not specified by docs/ISA.md and behaves like y = 0.
                let r_b = field_b(instr);
                let y = (instr >> 14) & 3;
                let imm = alu::sign_extend(instr & 0xFFF, 12) << ((instr >> 12) & 3);
                let base = self.get_reg(r_b);
                let offset_addr = base.wrapping_add(imm);
                let addr = if y == 2 { base } else { offset_addr };
                let writeback = matches!(y, 1 | 2).then_some((r_b, offset_addr));
                (addr, (instr >> 16) & 1 != 0, writeback)
            }
            AddrMode::Relative => {
                let imm = alu::sign_extend(instr & 0xFFFF, 16);
                let addr = self
                    .get_reg(field_b(instr))
                    .wrapping_add(imm)
                    .wrapping_add(self.pc)
                    .wrapping_add(4);
                (addr, (instr >> 16) & 1 != 0, None)
            }
            AddrMode::Immediate => {
                let imm = alu::sign_extend(instr & 0x1F_FFFF, 21);
                let addr = imm.wrapping_add(self.pc).wrapping_add(4);
                (addr, (instr >> 21) & 1 != 0, None)
            }
        };
        let ok = if is_load {
            match self.load(addr, width) {
                Some(value) => {
                    self.write_reg(r_a, value);
                    true
                }
                None => false,
            }
        } else {
            let value = self.get_reg(r_a);
            self.store(addr, width, value)
        };
        if !ok {
            return self.raise_pending_tlb_miss(addr);
        }
        if let Some((r_b, value)) = writeback {
            self.write_reg(r_b, value);
        }
        self.advance();
    }

    // Atomic fetch-add or swap: rA <- old [addr], [addr] <- f(old, rC).
    fn atomic_instr(&mut self, instr: u32, op: AtomicOp, mode: AddrMode) {
        // op | rA | rC | rB | imm12, or op | rA | rC | imm17 for Immediate.
        let r_a = field_a(instr);
        let operand = self.get_reg(field_b(instr));
        let addr = match mode {
            AddrMode::Immediate => alu::sign_extend(instr & 0x1_FFFF, 17)
                .wrapping_add(self.pc)
                .wrapping_add(4),
            _ => {
                let base = self
                    .get_reg((instr >> 12) & 0x1F)
                    .wrapping_add(alu::sign_extend(instr & 0xFFF, 12));
                if mode == AddrMode::Relative {
                    base.wrapping_add(self.pc).wrapping_add(4)
                } else {
                    base
                }
            }
        };
        match self.atomic_rmw(addr, op, operand) {
            Some(prev) => {
                self.write_reg(r_a, prev);
                self.advance();
            }
            None => self.raise_pending_tlb_miss(addr),
        }
    }

    // Evaluate a branch condition code against FLG; None for undefined codes.
    fn branch_condition(&self, cond: u32) -> Option<bool> {
        let flags = self.cregfile[CREG_FLG];
        let carry = flags & FLAG_CARRY != 0;
        let zero = flags & FLAG_ZERO != 0;
        let sign = flags & FLAG_SIGN != 0;
        let overflow = flags & FLAG_OVERFLOW != 0;
        Some(match cond {
            0 => true,                       // br
            1 => zero,                       // bz
            2 => !zero,                      // bnz
            3 => sign,                       // bs
            4 => !sign,                      // bns
            5 => carry,                      // bc
            6 => !carry,                     // bnc
            7 => overflow,                   // bo
            8 => !overflow,                  // bno
            9 => !zero && !sign,             // bps
            10 => zero || sign,              // bnps
            11 => sign == overflow && !zero, // bg
            12 => sign == overflow,          // bge
            13 => sign != overflow && !zero, // bl
            14 => sign != overflow || zero,  // ble
            15 => !zero && carry,            // ba
            16 => carry || zero,             // bae
            17 => !carry && !zero,           // bb
            18 => !carry || zero,            // bbe
            _ => return None,
        })
    }

    // PC-relative branch: target = PC + 4 + sext(imm22) * 4.
    fn branch_imm(&mut self, instr: u32) {
        let Some(taken) = self.branch_condition(field_a(instr)) else {
            return self.raise_invalid_instruction();
        };
        if taken {
            let offset = alu::sign_extend(instr & 0x3F_FFFF, 22).wrapping_mul(4);
            self.pc = self.pc.wrapping_add(4).wrapping_add(offset);
        } else {
            self.advance();
        }
    }

    // Register branch with link: rA <- PC + 4, then PC <- rB (absolute) or
    // PC + 4 + rB (relative). rB is read before rA is written.
    fn branch_reg(&mut self, instr: u32, relative: bool) {
        let target = self.get_reg(instr & 0x1F);
        let Some(taken) = self.branch_condition(field_a(instr)) else {
            return self.raise_invalid_instruction();
        };
        if !taken {
            return self.advance();
        }
        let link = self.pc.wrapping_add(4);
        self.write_reg((instr >> 5) & 0x1F, link);
        self.pc = if relative { link.wrapping_add(target) } else { target };
    }

    // trap: enter the trap vector, resuming at the next instruction.
    fn trap_instr(&mut self, instr: u32) {
        if instr & TRAP_RESERVED_MASK != 0 {
            // Reserved trap encodings are invalid instructions.
            return self.raise_invalid_instruction();
        }
        self.enter_handler(VEC_TRAP, self.pc.wrapping_add(4), "trap");
    }

    // Privileged instructions (`OPC_PRIVILEGED`); user mode raises a privilege fault.
    fn kernel_instr(&mut self, instr: u32) {
        if !self.get_kmode() {
            return self.raise_exception(VEC_PRIVILEGE, "priv");
        }
        let sub = (instr >> 10) & 3;
        match (instr >> 12) & 0x1F {
            0 => self.tlb_op(instr, sub),
            1 => self.crmv_op(instr, sub),
            2 => match sub {
                0 => self.advance(), // mode run
                1 => {
                    // mode sleep: PC stays here until an interrupt wakes the core.
                    self.asleep = true;
                    self.sleep_armed = true;
                }
                _ => self.halted = true, // mode halt
            },
            3 if (instr >> 11) & 1 == 0 => self.rfe(instr),
            4 => self.ipi_op(instr),
            5 => self.eoi_op(instr),
            _ => self.raise_invalid_instruction(),
        }
    }

    // tlbr/tlbw/tlbi/tlbc; rB holds the virtual address, rA the entry.
    fn tlb_op(&mut self, instr: u32, sub: u32) {
        let pid = self.cregfile[CREG_PID];
        let vpn = self.get_reg(field_b(instr)) >> 12;
        match sub {
            0 => {
                let value = self.tlb.read(pid, vpn).unwrap_or(0);
                self.write_reg(field_a(instr), value);
            }
            1 => {
                let value = self.get_reg(field_a(instr)) & TLB_ENTRY_MASK;
                self.tlb.write(pid, vpn, value);
            }
            2 => self.tlb.invalidate(pid, vpn),
            _ => self.tlb.clear(),
        }
        self.advance();
    }

    // crmv between general and control registers. crmv uses the raw register
    // file, so r31 is not aliased to KSP here.
    // Control-register numbers past TLBF, and the reserved cr10/cr11, are not
    // defined by docs/ISA.md; they raise invalid-instruction (the original
    // model indexed out of bounds and crashed the emulator).
    fn crmv_op(&mut self, instr: u32, sub: u32) {
        let ra = field_a(instr) as usize;
        let rb = field_b(instr) as usize;
        let a_is_creg = matches!(sub, 0 | 2);
        let b_is_creg = matches!(sub, 1 | 2);
        if (a_is_creg && !creg_defined(ra)) || (b_is_creg && !creg_defined(rb)) {
            return self.raise_invalid_instruction();
        }
        match sub {
            0 => self.write_creg(ra, self.regfile[rb]),
            1 if ra != 0 => self.regfile[ra] = self.read_creg(rb),
            2 => self.write_creg(ra, self.read_creg(rb)),
            3 if ra != 0 => self.regfile[ra] = self.regfile[rb],
            _ => {}
        }
        self.advance();
    }

    // ipi: interrupt one core or all cores. IPIs carry no payload, always
    // succeed, and write no register; the rA field is ignored.
    fn ipi_op(&mut self, instr: u32) {
        if (instr >> 11) & 1 != 0 {
            self.interrupts.send_ipi_all();
        } else {
            self.interrupts.send_ipi((instr & 0x3) as usize);
        }
        self.advance();
    }

    // eoi: clear one ISR bit (or all of them) and acknowledge the controller.
    fn eoi_op(&mut self, instr: u32) {
        let cleared = if (instr >> 11) & 1 != 0 { u32::MAX } else { 1 << (instr & 0xF) };
        self.write_isr(self.cregfile[CREG_ISR] & !cleared);
        self.advance();
    }

    // rfe: leave the handler, restoring PC and flags and re-enabling interrupts.
    fn rfe(&mut self, instr: u32) {
        if tracing() {
            println!(
                "[core {}] rfe instr=0x{:08X} pc=0x{:08X}",
                self.core_id, instr, self.pc
            );
        }
        let old = self.cregfile[CREG_PSR];
        self.cregfile[CREG_PSR] = old.wrapping_sub(1);
        if tracing() {
            println!(
                "[core {}] psr dec {:08X} -> {:08X} (rfe pc=0x{:08X})",
                self.core_id, old, self.cregfile[CREG_PSR], self.pc
            );
        }
        self.cregfile[CREG_IMR] |= IMR_GLOBAL_ENABLE;
        self.pc = self.cregfile[CREG_EPC];
        self.cregfile[CREG_FLG] = self.cregfile[CREG_EFG];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SD_INTERRUPT_BIT as SD_TEST_BIT;
    use interrupts::TIMER_INTERRUPT_BIT;

    // A single core attached to `cores`-wide interrupt state.
    fn test_core(cores: usize) -> (Emulator, Arc<InterruptController>) {
        let memory = Arc::new(Memory::new(HashMap::new(), false, 1));
        let interrupts = InterruptController::new(cores, false);
        (Emulator::from_shared(memory, Arc::clone(&interrupts), 0), interrupts)
    }

    // Region bounds must match Dioptase-OS/docs/kernel_mem_map.md so trace-mode
    // write warnings name the right region. Checks both edges of every region.
    #[test]
    fn memmap_regions_match_kernel_mem_map() {
        let expected: [(u32, u32, &str); 8] = [
            (0x0000_0000, 0x0000_0400, "ivt"),
            (0x0000_0400, 0x0000_8400, "bios"),
            (0x0000_8400, 0x0001_0000, "bios_stack"),
            (0x0001_0000, 0x000B_0000, "kernel_text"),
            (0x000B_0000, 0x000E_0000, "kernel_data"),
            (0x000E_0000, 0x000E_8000, "kernel_rodata"),
            (0x000E_8000, 0x000F_0000, "kernel_bss"),
            (0x000F_0000, 0x0010_0000, "kernel_stack"),
        ];
        for (start, end, name) in expected {
            assert_eq!(Emulator::memmap_region(start), Some(name), "first byte of {name}");
            assert_eq!(Emulator::memmap_region(end - 1), Some(name), "last byte of {name}");
        }
        // The physical frame pool is not a named kernel region.
        assert_eq!(Emulator::memmap_region(KERNEL_STACK_END), None);
    }

    // Preserve an IPI that becomes pending while software writes ISR state.
    #[test]
    fn write_isr_preserves_concurrently_pending_ipi() {
        let (mut cpu, interrupts) = test_core(2);
        cpu.cregfile[CREG_ISR] = TIMER_INTERRUPT_BIT;
        interrupts.send_ipi(0);

        cpu.write_isr(0);

        assert_eq!(
            cpu.cregfile[CREG_ISR], IPI_INTERRUPT_BIT,
            "writing ISR to clear one interrupt must preserve a concurrently pending IPI",
        );

        cpu.collect_interrupts();

        assert_eq!(
            cpu.cregfile[CREG_ISR], IPI_INTERRUPT_BIT,
            "taking the queued pending IPI on the next tick must not change the visible ISR bit",
        );
    }

    // A second IPI before eoi merges into the first: the handler sees one
    // interrupt, and eoi clears both. An IPI after eoi raises a new one.
    #[test]
    fn ipi_merges_until_target_acknowledges() {
        let (mut cpu, interrupts) = test_core(1);

        interrupts.send_ipi(0);
        cpu.collect_interrupts();
        interrupts.send_ipi(0);
        cpu.collect_interrupts();

        assert_eq!(
            cpu.cregfile[CREG_ISR] & IPI_INTERRUPT_BIT,
            IPI_INTERRUPT_BIT,
            "an IPI sent while the IPI bit is active must merge, not fail or queue",
        );

        cpu.eoi_op((OPC_PRIVILEGED << 27) | (5u32 << 12) | 5);

        assert_eq!(
            cpu.cregfile[CREG_ISR] & IPI_INTERRUPT_BIT,
            0,
            "eoi 5 must clear the merged IPI as a single interrupt",
        );

        interrupts.send_ipi(0);
        cpu.collect_interrupts();

        assert_eq!(
            cpu.cregfile[CREG_ISR] & IPI_INTERRUPT_BIT,
            IPI_INTERRUPT_BIT,
            "an IPI sent after eoi must raise a new interrupt",
        );
    }

    // ipi all reaches every core, including the sender and cores that already
    // have an IPI pending; ipi to a nonexistent core is dropped.
    #[test]
    fn ipi_all_reaches_every_core_and_missing_target_is_dropped() {
        let interrupts = InterruptController::new(3, false);

        interrupts.send_ipi(1);
        interrupts.send_ipi_all();
        interrupts.send_ipi(3);

        for core in 0..3 {
            assert_eq!(
                interrupts.take_pending(core),
                IPI_INTERRUPT_BIT,
                "ipi all must leave exactly the IPI bit pending on core {core}",
            );
        }
    }

    // ipi writes no register, so a nonzero rA field must leave that register
    // unchanged (encodings from the old `ipi rA, n` form still decode).
    #[test]
    fn ipi_ignores_ra_field() {
        let (mut cpu, interrupts) = test_core(2);
        cpu.regfile[3] = 0xDEAD_BEEF;

        cpu.execute((OPC_PRIVILEGED << 27) | (3u32 << 22) | (4u32 << 12) | 1);

        assert_eq!(cpu.regfile[3], 0xDEAD_BEEF, "ipi must not write rA");
        assert_eq!(interrupts.peek_pending(1), IPI_INTERRUPT_BIT, "ipi 1 must reach core 1");
    }

    // cr10 and cr11 (the removed IPI mailboxes) are reserved, so crmv to or
    // from them must raise invalid-instruction rather than act as scratch.
    #[test]
    fn crmv_reserved_creg_raises_invalid_instruction() {
        for (creg, sub) in [(10u32, 0u32), (11, 0), (10, 1), (11, 1)] {
            let (mut cpu, _) = test_core(1);
            cpu.pc = 0x400;
            let (a, b) = if sub == 0 { (creg, 1) } else { (1, creg) };
            let instr = (OPC_PRIVILEGED << 27) | (a << 22) | (b << 17) | (1u32 << 12) | (sub << 10);
            cpu.execute(instr);
            assert_eq!(
                (cpu.cregfile[CREG_PSR], cpu.cregfile[CREG_EPC]),
                (2, 0x400),
                "crmv sub {sub} on reserved cr{creg} must enter the invalid-instruction handler",
            );
        }
    }

    // Ignore CRMV writes to the read-only ISR control register.
    #[test]
    fn crmv_write_to_isr_is_ignored() {
        let (mut cpu, _) = test_core(1);
        cpu.cregfile[CREG_ISR] = TIMER_INTERRUPT_BIT;
        cpu.regfile[1] = 0xFFFF_FFFF;

        let instr = (OPC_PRIVILEGED << 27) | (2u32 << 22) | (1u32 << 17) | (1u32 << 12);
        cpu.execute(instr);

        assert_eq!(
            cpu.cregfile[CREG_ISR], TIMER_INTERRUPT_BIT,
            "crmv writes to ISR must be ignored so interrupt acknowledgement goes through eoi",
        );
    }

    // Clear only the selected in-service bit for an indexed EOI.
    #[test]
    fn eoi_specific_clears_only_selected_isr_bit() {
        let (mut cpu, _) = test_core(1);
        cpu.cregfile[CREG_ISR] = TIMER_INTERRUPT_BIT | SD_TEST_BIT;

        cpu.eoi_op((OPC_PRIVILEGED << 27) | (5u32 << 12));

        assert_eq!(
            cpu.cregfile[CREG_ISR], SD_TEST_BIT,
            "eoi n must clear only the requested ISR bit",
        );
    }

    // Preserve a concurrently arriving IPI while EOI clears existing service state.
    #[test]
    fn eoi_all_preserves_concurrently_pending_ipi() {
        let (mut cpu, interrupts) = test_core(2);
        cpu.cregfile[CREG_ISR] = TIMER_INTERRUPT_BIT | SD_TEST_BIT;
        interrupts.send_ipi(0);

        cpu.eoi_op((OPC_PRIVILEGED << 27) | (5u32 << 12) | (1u32 << 11));

        assert_eq!(
            cpu.cregfile[CREG_ISR], IPI_INTERRUPT_BIT,
            "eoi all must clear handled ISR bits without dropping a concurrently pending IPI",
        );
    }

    // A misaligned PC must enter exactly one exception handler. The debugger
    // step path used to raise a TLB miss on top of the misaligned-PC entry.
    #[test]
    fn misaligned_pc_enters_one_handler() {
        let (mut cpu, _) = test_core(1);
        cpu.pc = 0x402;
        assert_eq!(cpu.step_instruction(), StepOutcome::MisalignedPc { pc: 0x402 });
        assert_eq!(cpu.cregfile[CREG_PSR], 2, "one nested entry from PSR=1");
        assert_eq!(cpu.cregfile[CREG_EPC], 0x402);
    }

    // An undefined ALU op must raise invalid-instruction with FLG untouched,
    // so EFG captures the flags as they were before the instruction.
    #[test]
    fn invalid_alu_op_preserves_flags() {
        let (mut cpu, _) = test_core(1);
        cpu.cregfile[CREG_FLG] = FLAG_CARRY | FLAG_ZERO;
        cpu.execute(22 << 5); // register-form ALU op 22 is undefined
        assert_eq!(cpu.cregfile[CREG_EFG], FLAG_CARRY | FLAG_ZERO);
        assert_eq!(cpu.cregfile[CREG_PSR], 2);
    }
}
