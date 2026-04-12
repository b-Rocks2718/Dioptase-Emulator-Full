use std::cmp;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::memory::{
    CLK_REG_START, Memory, PHYSMEM_MAX, SD_INTERRUPT_BIT, SD2_INTERRUPT_BIT, SdSlot,
    VGA_INTERRUPT_BIT,
};

use crate::graphics::Graphics;

mod debugger;

// Reset vector for kernel entry (see docs/mem_map.md).
const RESET_PC: u32 = 0x0000_0400;

// Memory map ranges from Dioptase-OS/docs/kernel_mem_map.md.
const IVT_START: u32 = 0x0000_0000;
const IVT_END: u32 = 0x0000_0400;
const BIOS_START: u32 = 0x0000_0400;
const BIOS_SIZE: u32 = 0x0001_0000; // 64KB reserved; kernel can overwrite after entry.
const BIOS_END: u32 = BIOS_START + BIOS_SIZE;
const KERNEL_TEXT_START: u32 = 0x0001_0000;
const KERNEL_TEXT_END: u32 = 0x0009_0000;
const KERNEL_DATA_START: u32 = 0x0009_0000;
const KERNEL_DATA_END: u32 = 0x000A_0000;
const KERNEL_RODATA_START: u32 = 0x000A_0000;
const KERNEL_RODATA_END: u32 = 0x000B_0000;
const KERNEL_BSS_START: u32 = 0x000B_0000;
const KERNEL_BSS_END: u32 = 0x000E_0000;
const KERNEL_INT_STACK_START: u32 = 0x000E_0000;
const KERNEL_INT_STACK_END: u32 = 0x000F_0000;
const KERNEL_STACK_START: u32 = 0x000F_0000;
const KERNEL_STACK_END: u32 = 0x0010_0000;
const TLB_ENTRIES: usize = 16;
const TLB_FLAG_READ: u32 = 0x1;
const TLB_FLAG_WRITE: u32 = 0x2;
const TLB_FLAG_EXEC: u32 = 0x4;
const TLB_FLAG_USER: u32 = 0x8;
const TLB_FAULT_ABSENT: u32 = 0x0;
const EXC_TLB_MISS_VECTOR: u32 = 0x82;
const EXC_MISALIGNED_PC_VECTOR: u32 = 0x84;
const PSR_REASON_TLB_MISS: &str = "tlb_miss";
const PSR_REASON_MISALIGNED_PC: &str = "misaligned_pc";
const CREG_PID: usize = 1;
const CREG_IMR: usize = 3;
const CREG_EPC: usize = 4;
const CREG_FLG: usize = 5;
const CREG_EFG: usize = 6;
const CREG_TLB: usize = 7;
const CREG_CID: usize = 9;
const CREG_MBI: usize = 10;
const CREG_TLBF: usize = 12;

// Global toggle for interrupt tracing output.
static TRACE_INTERRUPTS: AtomicBool = AtomicBool::new(false);

pub fn set_trace_interrupts(enabled: bool) {
    TRACE_INTERRUPTS.store(enabled, Ordering::Relaxed);
}

#[derive(Debug)]
pub struct RandomCache {
    private_table: HashMap<(u32, u32), u32>,
    global_table: HashMap<u32, u32>,
    total_capacity: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TlbAccess {
    Hit(u32),
    Fault(u32),
}

impl RandomCache {
    fn total_size(&self) -> usize {
        self.private_table.len() + self.global_table.len()
    }

    fn evict_one(&mut self, prefer_global: bool) {
        // Replacement policy is implementation-defined; this emulator uses a
        // deterministic first-key eviction and prefers evicting from the same
        // class (global/private) as the incoming entry when possible.
        if prefer_global {
            if let Some(evict) = self.global_table.keys().next().cloned() {
                self.global_table.remove(&evict);
                return;
            }
            if let Some(evict) = self.private_table.keys().next().cloned() {
                self.private_table.remove(&evict);
            }
        } else {
            if let Some(evict) = self.private_table.keys().next().cloned() {
                self.private_table.remove(&evict);
                return;
            }
            if let Some(evict) = self.global_table.keys().next().cloned() {
                self.global_table.remove(&evict);
            }
        }
    }

    pub fn new(capacity: usize) -> RandomCache {
        RandomCache {
            private_table: HashMap::new(),
            global_table: HashMap::new(),
            total_capacity: capacity,
        }
    }

    fn fault_flags(entry: u32, operation: u32, kmode: bool) -> u32 {
        let mut flags = 0;
        match operation {
            0 => {
                if entry & TLB_FLAG_READ == 0 {
                    flags |= TLB_FLAG_READ;
                }
            }
            1 => {
                if entry & TLB_FLAG_WRITE == 0 {
                    flags |= TLB_FLAG_WRITE;
                }
            }
            2 => {
                if entry & TLB_FLAG_EXEC == 0 {
                    flags |= TLB_FLAG_EXEC;
                }
            }
            _ => panic!("invalid operation code"),
        }

        if !kmode && entry & TLB_FLAG_USER == 0 {
            flags |= TLB_FLAG_USER;
        }

        flags
    }

    fn classify_entry(entry: u32, operation: u32, kmode: bool) -> TlbAccess {
        let flags = Self::fault_flags(entry, operation, kmode);
        if flags == 0 {
            TlbAccess::Hit(entry & 0xFFFFF000)
        } else {
            TlbAccess::Fault(flags)
        }
    }

    fn access(&self, pid: u32, vpn: u32, operation: u32, kmode: bool) -> TlbAccess {
        // Memory access keeps the existing private-then-global lookup order so
        // emulator behavior does not change for duplicate private/global entries.
        assert!(self.total_size() <= self.total_capacity);

        let key = (pid, vpn);
        let mut private_fault = None;
        if let Some(entry) = self.private_table.get(&key).copied() {
            match Self::classify_entry(entry, operation, kmode) {
                TlbAccess::Hit(ppn) => return TlbAccess::Hit(ppn),
                TlbAccess::Fault(flags) => private_fault = Some(flags),
            }
        }

        if let Some(entry) = self.global_table.get(&vpn).copied() {
            return Self::classify_entry(entry, operation, kmode);
        }

        TlbAccess::Fault(private_fault.unwrap_or(TLB_FAULT_ABSENT))
    }

    pub fn read(&self, pid: u32, vpn: u32) -> Option<u32> {
        // used by tlbr instruction

        assert!(self.total_size() <= self.total_capacity);
        let result = self.private_table.get(&(pid, vpn)).copied();

        if result.is_some() {
            return result;
        } else {
            // try global table
            self.global_table.get(&vpn).copied()
        }
    }

    pub fn write(&mut self, pid: u32, vpn: u32, ppn: u32) {
        if ppn & 0x00000010 != 0 {
            // global entry
            if !self.global_table.contains_key(&vpn) && self.total_size() >= self.total_capacity {
                self.evict_one(true);
            }

            // will replace old mapping if one existed
            self.global_table.insert(vpn, ppn);
            assert!(self.total_size() <= self.total_capacity);
        } else {
            // private entry
            if !self.private_table.contains_key(&(pid, vpn))
                && self.total_size() >= self.total_capacity
            {
                self.evict_one(false);
            }

            // will replace old mapping if one existed
            self.private_table.insert((pid, vpn), ppn);

            assert!(self.total_size() <= self.total_capacity);
        }
    }

    pub fn invalidate(&mut self, pid: u32, vpn: u32) {
        self.private_table.remove(&(pid, vpn));
        self.global_table.remove(&vpn);
    }

    pub fn clear(&mut self) {
        self.private_table.drain();
        self.global_table.drain();
    }

    fn debug_dump(&self) {
        println!("TLB private: {} entries", self.private_table.len());
        if self.private_table.is_empty() {
            println!("  (empty)");
        } else {
            for ((pid, vpn), entry) in &self.private_table {
                println!("  pid {:08X} vpn {:08X} -> {:08X}", pid, vpn, entry);
            }
        }
        println!("TLB global: {} entries", self.global_table.len());
        if self.global_table.is_empty() {
            println!("  (empty)");
        } else {
            for (vpn, entry) in &self.global_table {
                println!("  vpn {:08X} -> {:08X}", vpn, entry);
            }
        }
        println!(
            "TLB total: {}/{} entries",
            self.total_size(),
            self.total_capacity
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Scheduler policy for multicore execution.
pub enum ScheduleMode {
    Free,
    RoundRobin,
    Random,
}

impl ScheduleMode {
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "free" => Some(ScheduleMode::Free),
            "rr" | "round-robin" | "roundrobin" => Some(ScheduleMode::RoundRobin),
            "rand" | "random" => Some(ScheduleMode::Random),
            _ => None,
        }
    }
}

struct SchedulerState {
    // Next core allowed to execute in non-free scheduling modes.
    next_core: usize,
    // Per-core halt tracking for scheduling decisions.
    halted: Vec<bool>,
    // Global stop flag shared by all cores.
    done: bool,
    // RNG seed used by random scheduling.
    seed: u64,
}

struct Scheduler {
    mode: ScheduleMode,
    cores: usize,
    state: Mutex<SchedulerState>,
    cv: Condvar,
}

impl Scheduler {
    fn new(mode: ScheduleMode, cores: usize) -> Arc<Scheduler> {
        let mut seed = seed_from_time();
        let halted = vec![false; cores];
        let next_core = if mode == ScheduleMode::Random {
            choose_random_core(&mut seed, &halted).unwrap_or(0)
        } else {
            0
        };
        Arc::new(Scheduler {
            mode,
            cores,
            state: Mutex::new(SchedulerState {
                next_core,
                halted,
                done: false,
                seed,
            }),
            cv: Condvar::new(),
        })
    }

    fn wait_turn(&self, core_id: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.done || state.halted[core_id] {
                return false;
            }
            if state.next_core == core_id {
                return true;
            }
            // Block until the scheduler hands this core the next turn.
            state = self.cv.wait(state).unwrap();
        }
    }

    fn finish_turn(&self, core_id: usize) {
        let mut state = self.state.lock().unwrap();
        if state.done {
            self.cv.notify_all();
            return;
        }
        // Pick the next runnable core based on the chosen scheduling policy.
        match pick_next_core(self.mode, self.cores, core_id, &mut state) {
            Some(next) => state.next_core = next,
            None => state.done = true,
        }
        self.cv.notify_all();
    }

    fn mark_halted(&self, core_id: usize) {
        let mut state = self.state.lock().unwrap();
        state.halted[core_id] = true;
        if state.halted.iter().all(|halted| *halted) {
            state.done = true;
            self.cv.notify_all();
            return;
        }
        // If the scheduled core halted, advance to a still-runnable core.
        if state.halted[state.next_core] {
            match pick_next_core(self.mode, self.cores, state.next_core, &mut state) {
                Some(next) => state.next_core = next,
                None => state.done = true,
            }
        }
        self.cv.notify_all();
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.done = true;
        self.cv.notify_all();
    }
}

fn seed_from_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn next_rand_u32(seed: &mut u64) -> u32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*seed >> 32) as u32
}

fn choose_random_core(seed: &mut u64, halted: &[bool]) -> Option<usize> {
    let active = halted.iter().filter(|h| !**h).count();
    if active == 0 {
        return None;
    }
    let target = (next_rand_u32(seed) as usize) % active;
    let mut seen = 0usize;
    for (idx, is_halted) in halted.iter().enumerate() {
        if !*is_halted {
            if seen == target {
                return Some(idx);
            }
            seen += 1;
        }
    }
    None
}

fn pick_next_core(
    mode: ScheduleMode,
    cores: usize,
    current: usize,
    state: &mut SchedulerState,
) -> Option<usize> {
    match mode {
        ScheduleMode::Random => choose_random_core(&mut state.seed, &state.halted),
        ScheduleMode::RoundRobin => {
            for offset in 1..=cores {
                let idx = (current + offset) % cores;
                if !state.halted[idx] {
                    return Some(idx);
                }
            }
            None
        }
        ScheduleMode::Free => {
            for (idx, halted) in state.halted.iter().enumerate() {
                if !*halted {
                    return Some(idx);
                }
            }
            None
        }
    }
}

const TIMER_INTERRUPT_BIT: u32 = 1 << 0;
const KB_INTERRUPT_BIT: u32 = 1 << 1;
const UART_INTERRUPT_BIT: u32 = 1 << 2;
const IPI_INTERRUPT_BIT: u32 = 1 << 5;

fn format_interrupts(bits: u32) -> String {
    let mut parts = Vec::new();
    if (bits & TIMER_INTERRUPT_BIT) != 0 {
        parts.push("timer");
    }
    if (bits & KB_INTERRUPT_BIT) != 0 {
        parts.push("keyboard");
    }
    if (bits & UART_INTERRUPT_BIT) != 0 {
        parts.push("uart");
    }
    if (bits & SD_INTERRUPT_BIT) != 0 {
        parts.push("sd0");
    }
    if (bits & SD2_INTERRUPT_BIT) != 0 {
        parts.push("sd1");
    }
    if (bits & VGA_INTERRUPT_BIT) != 0 {
        parts.push("vga");
    }
    if (bits & IPI_INTERRUPT_BIT) != 0 {
        parts.push("ipi");
    }
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("|")
    }
}

struct InterruptRouteState {
    // Round-robin pointers for device interrupts routed to a single core.
    next_kb: usize,
    next_uart: usize,
    next_sd: usize,
    next_sd2: usize,
    next_vga: usize,
    // Track which core currently has a pending KB/UART interrupt.
    kb_inflight: Option<usize>,
    uart_inflight: Option<usize>,
}

struct InterruptController {
    cores: usize,
    // Per-core pending interrupt bits delivered on the next tick.
    pending: Vec<AtomicU32>,
    // Per-core IPI payload storage (copied into MBI on delivery).
    ipi_payload: Vec<AtomicU32>,
    routes: Mutex<InterruptRouteState>,
}

impl InterruptController {
    fn new(cores: usize) -> Arc<InterruptController> {
        Arc::new(InterruptController {
            cores,
            pending: (0..cores).map(|_| AtomicU32::new(0)).collect(),
            ipi_payload: (0..cores).map(|_| AtomicU32::new(0)).collect(),
            routes: Mutex::new(InterruptRouteState {
                next_kb: 0,
                next_uart: 0,
                next_sd: 0,
                next_sd2: 0,
                next_vga: 0,
                kb_inflight: None,
                uart_inflight: None,
            }),
        })
    }

    fn set_pending_bits(&self, core: usize, bits: u32) {
        self.pending[core].fetch_or(bits, Ordering::Release);
    }

    fn peek_pending(&self, core: usize) -> u32 {
        self.pending[core].load(Ordering::Acquire)
    }

    fn take_pending(&self, core: usize) -> u32 {
        self.pending[core].swap(0, Ordering::AcqRel)
    }

    fn read_ipi_payload(&self, core: usize) -> u32 {
        self.ipi_payload[core].load(Ordering::Acquire)
    }

    fn write_ipi_payload(&self, core: usize, value: u32) {
        self.ipi_payload[core].store(value, Ordering::Release);
    }

    fn send_ipi(&self, target: usize, value: u32) -> bool {
        if target >= self.cores {
            return false;
        }
        // MBI carries the payload, ISR bit signals delivery.
        self.write_ipi_payload(target, value);
        self.set_pending_bits(target, IPI_INTERRUPT_BIT);
        true
    }

    fn send_ipi_all(&self, sender: usize, value: u32) -> u32 {
        let mut mask = 0u32;
        for core in 0..self.cores {
            if core == sender {
                continue;
            }
            if self.send_ipi(core, value) {
                mask |= 1u32 << core;
            }
        }
        mask
    }

    fn dispatch_input(&self, use_uart_rx: bool, io_nonempty: bool) {
        let mut routes = self.routes.lock().unwrap();
        if use_uart_rx {
            let bit = UART_INTERRUPT_BIT;
            if io_nonempty && routes.uart_inflight.is_none() {
                // Route the next UART interrupt to a single core in round-robin order.
                let core = routes.next_uart % self.cores;
                routes.next_uart = (routes.next_uart + 1) % self.cores;
                routes.uart_inflight = Some(core);
                self.set_pending_bits(core, bit);
            }
        } else {
            let bit = KB_INTERRUPT_BIT;
            if io_nonempty && routes.kb_inflight.is_none() {
                // Route the next keyboard interrupt to a single core in round-robin order.
                let core = routes.next_kb % self.cores;
                routes.next_kb = (routes.next_kb + 1) % self.cores;
                routes.kb_inflight = Some(core);
                self.set_pending_bits(core, bit);
            }
        }
    }

    fn dispatch_device_interrupts(&self, pending: u32) {
        if pending == 0 {
            return;
        }
        let mut routes = self.routes.lock().unwrap();
        if pending & SD_INTERRUPT_BIT != 0 {
            // SD interrupts go to one core at a time, round-robin.
            let core = routes.next_sd % self.cores;
            routes.next_sd = (routes.next_sd + 1) % self.cores;
            self.set_pending_bits(core, SD_INTERRUPT_BIT);
        }
        if pending & SD2_INTERRUPT_BIT != 0 {
            // SD2 interrupts go to one core at a time, round-robin.
            let core = routes.next_sd2 % self.cores;
            routes.next_sd2 = (routes.next_sd2 + 1) % self.cores;
            self.set_pending_bits(core, SD2_INTERRUPT_BIT);
        }
        if pending & VGA_INTERRUPT_BIT != 0 {
            // VGA interrupts go to one core at a time, round-robin.
            let core = routes.next_vga % self.cores;
            routes.next_vga = (routes.next_vga + 1) % self.cores;
            self.set_pending_bits(core, VGA_INTERRUPT_BIT);
        }
    }

    fn broadcast_timer(&self) {
        for core in 0..self.cores {
            self.set_pending_bits(core, TIMER_INTERRUPT_BIT);
        }
    }

    fn ack_input(&self, core: usize, cleared_bits: u32) {
        if cleared_bits == 0 {
            return;
        }
        let mut routes = self.routes.lock().unwrap();
        if (cleared_bits & KB_INTERRUPT_BIT) != 0 {
            if routes.kb_inflight == Some(core) {
                routes.kb_inflight = None;
            }
        }
        if (cleared_bits & UART_INTERRUPT_BIT) != 0 {
            if routes.uart_inflight == Some(core) {
                routes.uart_inflight = None;
            }
        }
    }
}

struct RunShared {
    // Global stop signal shared by all cores.
    stop: AtomicBool,
    // Track how many cores have exited their run loops.
    halted: AtomicUsize,
    // Per-core return values (r1) recorded on exit.
    results: Mutex<Vec<Option<u32>>>,
    // Shared completion flag for graphics and multi-core coordination.
    finished: Arc<Mutex<bool>>,
    cores: usize,
}

impl RunShared {
    fn new(cores: usize, finished: Arc<Mutex<bool>>) -> RunShared {
        RunShared {
            stop: AtomicBool::new(false),
            halted: AtomicUsize::new(0),
            results: Mutex::new(vec![None; cores]),
            finished,
            cores,
        }
    }

    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        *self.finished.lock().unwrap() = true;
    }

    fn record_exit(&self, core_id: usize, value: u32) {
        self.results.lock().unwrap()[core_id] = Some(value);
        let halted = self.halted.fetch_add(1, Ordering::Relaxed) + 1;
        if halted == self.cores {
            *self.finished.lock().unwrap() = true;
        }
    }
}

pub struct Emulator {
    regfile: [u32; 32],  // r0 - r31
    cregfile: [u32; 13], // PSR, PID, ISR, IMR, EPC, FLG, EFG, TLB, KSP, CID, MBI, MBO, TLBF
    // in FLG, flags are: carry | zero | sign | overflow
    memory: Arc<Memory>,
    interrupts: Arc<InterruptController>,
    tlb: RandomCache,
    pc: u32,
    asleep: bool,
    // Distinguish "mode sleep" from a core that starts asleep.
    sleep_armed: bool,
    halted: bool,
    count: u32,
    core_id: u32,
    use_uart_rx: bool,
    pending_tlb_fault: Option<u32>,
    watchpoints: Vec<Watchpoint>,
    watchpoint_hit: Option<WatchpointHit>,
}

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where
    P: AsRef<Path>,
{
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

// Label -> address list (labels can appear multiple times across sections).
type LabelMap = HashMap<String, Vec<u32>>;

#[derive(Clone, Debug)]
// Source line marker emitted by the assembler debug pipeline.
struct DebugLine {
    file: String,
    line: u32,
    addr: u32,
}

#[derive(Clone, Debug)]
// Stack local debug metadata anchored to a code address.
struct DebugLocal {
    name: String,
    offset: i32,
    size: u32,
}

#[derive(Clone, Debug)]
// Global data symbol debug metadata.
struct DebugGlobal {
    name: String,
    addr: u32,
}

#[derive(Clone, Debug, Default)]
// Aggregated C debug info parsed from a .debug file.
struct DebugInfo {
    lines: Vec<DebugLine>,
    locals_by_addr: HashMap<u32, Vec<DebugLocal>>,
    globals: Vec<DebugGlobal>,
    missing_line_addrs: bool,
    missing_local_addrs: bool,
    missing_local_sizes: bool,
}

#[derive(Clone)]
// Loader output: bytes + labels + C debug metadata.
struct ProgramImage {
    instructions: HashMap<u32, u8>,
    labels: LabelMap,
    debug: DebugInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchKind {
    Read,
    Write,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchAccess {
    Read,
    Write,
}

// Single-byte watchpoints tracked by exact address.
#[derive(Clone, Copy, Debug)]
struct Watchpoint {
    addr: u32,
    kind: WatchKind,
}

#[derive(Clone, Copy, Debug)]
struct WatchpointHit {
    addr: u32,
    access: WatchAccess,
    value: u8,
}

fn parse_hex_u32(token: &str) -> Option<u32> {
    let s = token.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

fn add_label(labels: &mut LabelMap, name: &str, addr: u32) {
    let entry = labels.entry(name.to_string()).or_default();
    if !entry.contains(&addr) {
        entry.push(addr);
    }
}

// Parse assembler debug label lines: "#label <name> <addr>".
fn parse_label_line(line: &str, labels: &mut LabelMap) -> bool {
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("#label") => {
            if let (Some(name), Some(addr_str)) = (parts.next(), parts.next()) {
                if let Some(addr) = parse_hex_u32(addr_str) {
                    add_label(labels, name, addr);
                    return true;
                }
            }
        }
        _ => {}
    }
    false
}

// Parse C debug metadata lines emitted by the assembler.
fn parse_debug_line(line: &str, debug: &mut DebugInfo) -> bool {
    const DEFAULT_LOCAL_SIZE_BYTES: u32 = 4;
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("#line") => {
            let Some(file) = parts.next() else {
                return true;
            };
            let Some(line_str) = parts.next() else {
                return true;
            };
            let line_num = match line_str.parse::<u32>() {
                Ok(value) => value,
                Err(_) => {
                    debug.missing_line_addrs = true;
                    return true;
                }
            };
            let addr = match parts.next().and_then(parse_hex_u32) {
                Some(value) => value,
                None => {
                    debug.missing_line_addrs = true;
                    return true;
                }
            };
            debug.lines.push(DebugLine {
                file: file.to_string(),
                line: line_num,
                addr,
            });
            true
        }
        Some("#local") => {
            let Some(name) = parts.next() else {
                return true;
            };
            let Some(offset_str) = parts.next() else {
                return true;
            };
            let offset = match offset_str.parse::<i32>() {
                Ok(value) => value,
                Err(_) => {
                    debug.missing_local_addrs = true;
                    return true;
                }
            };
            let remaining: Vec<&str> = parts.collect();
            let (size, addr_str) = match remaining.as_slice() {
                [] => {
                    debug.missing_local_sizes = true;
                    debug.missing_local_addrs = true;
                    return true;
                }
                [addr_only] => {
                    debug.missing_local_sizes = true;
                    (DEFAULT_LOCAL_SIZE_BYTES, *addr_only)
                }
                [size_str, addr_str, ..] => {
                    let mut size = match size_str.parse::<u32>() {
                        Ok(value) if value > 0 => value,
                        _ => {
                            debug.missing_local_sizes = true;
                            DEFAULT_LOCAL_SIZE_BYTES
                        }
                    };
                    if size == 0 {
                        debug.missing_local_sizes = true;
                        size = DEFAULT_LOCAL_SIZE_BYTES;
                    }
                    (size, *addr_str)
                }
            };
            let addr = match parse_hex_u32(addr_str) {
                Some(value) => value,
                None => {
                    debug.missing_local_addrs = true;
                    return true;
                }
            };
            debug
                .locals_by_addr
                .entry(addr)
                .or_default()
                .push(DebugLocal {
                    name: name.to_string(),
                    offset,
                    size,
                });
            true
        }
        Some("#data") => {
            let Some(name) = parts.next() else {
                return true;
            };
            let Some(addr_str) = parts.next() else {
                return true;
            };
            if let Some(addr) = parse_hex_u32(addr_str) {
                if !debug
                    .globals
                    .iter()
                    .any(|g| g.name == name && g.addr == addr)
                {
                    debug.globals.push(DebugGlobal {
                        name: name.to_string(),
                        addr,
                    });
                }
            }
            true
        }
        _ => false,
    }
}

// Load hex (or .debug) program and collect any embedded labels.
fn load_program(path: &str) -> ProgramImage {
    let mut instructions = HashMap::new();
    let mut labels = LabelMap::new();
    let mut debug = DebugInfo::default();

    let lines = read_lines(path).expect("Couldn't open input file");
    let mut pc: u32 = 0;
    for line in lines.map_while(Result::ok) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if line.starts_with('#') {
            // Debug metadata lines are prefixed with '#'.
            parse_label_line(line, &mut labels);
            parse_debug_line(line, &mut debug);
            continue;
        }
        if line.starts_with(';') || line.starts_with("//") {
            continue;
        }

        if let Some(rest) = line.strip_prefix('@') {
            let addr_str = rest.trim();
            let addr = u32::from_str_radix(addr_str, 16).expect("Invalid address") * 4;
            pc = addr;
            continue;
        }

        let instruction = u32::from_str_radix(line, 16).expect("Error parsing hex file");

        instructions.insert(pc, instruction as u8);
        instructions.insert(pc + 1, (instruction >> 8) as u8);
        instructions.insert(pc + 2, (instruction >> 16) as u8);
        instructions.insert(pc + 3, (instruction >> 24) as u8);

        pc += 4;
    }

    ProgramImage {
        instructions,
        labels,
        debug,
    }
}

impl Emulator {
    pub fn new(
        path: String,
        use_uart_rx: bool,
        sd_dma_ticks_per_word: u32,
        sd0_image: Option<&[u8]>,
        sd1_image: Option<&[u8]>,
    ) -> Emulator {
        let image = load_program(&path);
        Emulator::from_instructions(
            image.instructions,
            use_uart_rx,
            sd_dma_ticks_per_word,
            sd0_image,
            sd1_image,
        )
    }

    pub fn from_instructions(
        instructions: HashMap<u32, u8>,
        use_uart_rx: bool,
        sd_dma_ticks_per_word: u32,
        sd0_image: Option<&[u8]>,
        sd1_image: Option<&[u8]>,
    ) -> Emulator {
        let memory: Arc<Memory> = Arc::new(Memory::new(
            instructions,
            use_uart_rx,
            sd_dma_ticks_per_word,
        ));
        if let Some(image) = sd0_image {
            memory.load_sd_image(SdSlot::Sd0, image);
        }
        if let Some(image) = sd1_image {
            memory.load_sd_image(SdSlot::Sd1, image);
        }
        let interrupts = InterruptController::new(1);
        Emulator::from_shared(memory, interrupts, use_uart_rx, 0)
    }

    // Purpose: export one SD device from this emulator instance as a raw host image.
    // Inputs: slot selector.
    // Outputs: contiguous bytes representing the tracked SD image contents.
    pub fn dump_sd_image(&self, slot: SdSlot) -> Vec<u8> {
        self.memory.dump_sd_image(slot)
    }

    // Purpose: expose the shared memory backing this emulator instance.
    // Inputs: none.
    // Outputs: an Arc clone so callers can inspect memory after `run(self, ...)`.
    pub fn shared_memory(&self) -> Arc<Memory> {
        Arc::clone(&self.memory)
    }

    fn from_shared(
        memory: Arc<Memory>,
        interrupts: Arc<InterruptController>,
        use_uart_rx: bool,
        core_id: u32,
    ) -> Emulator {
        let mut cregfile = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]; // start cores in kernel mode
        // CID is a read-only core identifier.
        cregfile[CREG_CID] = core_id;
        if core_id != 0 {
            // Allow IPI wakeups on secondary cores by default.
            cregfile[CREG_IMR] = 0x80000020;
        }

        Emulator {
            regfile: [
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0,
            ],
            cregfile,
            memory,
            interrupts,
            tlb: RandomCache::new(TLB_ENTRIES),
            pc: RESET_PC,
            asleep: core_id != 0,
            sleep_armed: false,
            halted: false,
            count: 0,
            core_id,
            use_uart_rx,
            pending_tlb_fault: None,
            watchpoints: Vec::new(),
            watchpoint_hit: None,
        }
    }

    fn read_isr(&self) -> u32 {
        self.cregfile[2]
    }

    fn write_isr(&mut self, value: u32) {
        let old = self.cregfile[2];
        // Match the hardware cregfile semantics: software ISR writes must not
        // drop interrupts that become pending during a read/modify/write clear.
        let pending = self.interrupts.peek_pending(self.core_id as usize);
        if (pending & IPI_INTERRUPT_BIT) != 0 {
            self.cregfile[10] = self.interrupts.read_ipi_payload(self.core_id as usize);
        }
        self.cregfile[2] = value | pending;
        // Let the interrupt controller know when input interrupts are cleared.
        let cleared = old & !self.cregfile[2];
        if cleared != 0 {
            self.interrupts.ack_input(self.core_id as usize, cleared);
        }
    }

    fn read_mbi(&self) -> u32 {
        self.cregfile[10]
    }

    fn write_mbi(&mut self, value: u32) {
        self.cregfile[10] = value;
    }

    fn read_creg(&self, idx: usize) -> u32 {
        match idx {
            // ISR and MBI are core-local control registers.
            2 => self.read_isr(),
            CREG_MBI => self.read_mbi(),
            _ => self.cregfile[idx],
        }
    }

    fn write_creg(&mut self, idx: usize, value: u32) {
        match idx {
            // Route ISR/MBI through helpers so we can track clears and core-local state.
            2 | CREG_CID => {
                // CID is read-only.
                println!("Warning: attempt to write read-only register cr{}", idx);
            }
            CREG_MBI => self.write_mbi(value),

            _ => {
                if idx == 0 && TRACE_INTERRUPTS.load(Ordering::Relaxed) {
                    println!(
                        "[core {}] psr write {:08X} -> {:08X} (crmv pc=0x{:08X})",
                        self.core_id, self.cregfile[0], value, self.pc
                    );
                }
                self.cregfile[idx] = value;
            }
        }
    }

    fn clear_pending_tlb_fault(&mut self) {
        self.pending_tlb_fault = None;
    }

    fn record_pending_tlb_fault(&mut self, flags: u32) {
        self.pending_tlb_fault = Some(flags);
    }

    fn take_pending_tlb_fault(&mut self) -> u32 {
        self.pending_tlb_fault.take().unwrap_or(TLB_FAULT_ABSENT)
    }

    // Kernel mode is derived from the PSR (cr0) depth, not a cached flag.
    fn get_kmode(&self) -> bool {
        self.cregfile[0] != 0
    }

    fn psr_inc_checked(&mut self, reason: &str) {
        if self.cregfile[0] == u32::MAX {
            panic!("too many nested exceptions!");
        }
        let old = self.cregfile[0];
        self.cregfile[0] = self.cregfile[0].wrapping_add(1);
        if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            println!(
                "[core {}] psr inc {:08X} -> {:08X} ({} pc=0x{:08X})",
                self.core_id, old, self.cregfile[0], reason, self.pc
            );
        }
    }

    fn psr_dec(&mut self, reason: &str) {
        let old = self.cregfile[0];
        self.cregfile[0] = self.cregfile[0].wrapping_sub(1);
        if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            println!(
                "[core {}] psr dec {:08X} -> {:08X} ({} pc=0x{:08X})",
                self.core_id, old, self.cregfile[0], reason, self.pc
            );
        }
    }

    fn memmap_region(paddr: u32) -> Option<&'static str> {
        if paddr >= KERNEL_TEXT_START && paddr < KERNEL_TEXT_END {
            Some("kernel_text")
        } else if paddr >= KERNEL_RODATA_START && paddr < KERNEL_RODATA_END {
            Some("kernel_rodata")
        } else if paddr >= KERNEL_DATA_START && paddr < KERNEL_DATA_END {
            Some("kernel_data")
        } else if paddr >= KERNEL_BSS_START && paddr < KERNEL_BSS_END {
            Some("kernel_bss")
        } else if paddr >= KERNEL_INT_STACK_START && paddr < KERNEL_INT_STACK_END {
            Some("kernel_int_stack")
        } else if paddr >= KERNEL_STACK_START && paddr < KERNEL_STACK_END {
            Some("kernel_stack")
        } else if paddr >= BIOS_START && paddr < BIOS_END {
            Some("bios")
        } else if paddr >= IVT_START && paddr < IVT_END {
            Some("ivt")
        } else {
            None
        }
    }

    fn warn_on_write(region: &str) -> bool {
        matches!(region, "kernel_text" | "kernel_rodata" | "bios" | "ivt")
    }

    fn maybe_log_memmap_write(&self, vaddr: u32, paddr: u32, size: u8) {
        if !TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            return;
        }
        if let Some(region) = Self::memmap_region(paddr) {
            if Self::warn_on_write(region) {
                println!(
                    "[core {}] Warning: write to {} vaddr=0x{:08X} paddr=0x{:08X} size={} pc=0x{:08X}",
                    self.core_id, region, vaddr, paddr, size, self.pc
                );
            }
        }
    }

    // Record the first watchpoint hit so the debugger can stop after stepping.
    fn maybe_watch(&mut self, addr: u32, access: WatchAccess, value: u8) {
        if self.watchpoint_hit.is_some() || self.watchpoints.is_empty() {
            return;
        }
        for wp in &self.watchpoints {
            if wp.addr == addr {
                let matches = match (wp.kind, access) {
                    (WatchKind::Read, WatchAccess::Read) => true,
                    (WatchKind::Write, WatchAccess::Write) => true,
                    (WatchKind::ReadWrite, _) => true,
                    _ => false,
                };
                if matches {
                    self.watchpoint_hit = Some(WatchpointHit {
                        addr,
                        access,
                        value,
                    });
                    break;
                }
            }
        }
    }

    fn convert_mem_address(&mut self, addr: u32, operation: u32) -> Option<u32> {
        let kmode = self.get_kmode();
        if kmode {
            if addr <= PHYSMEM_MAX {
                Some(addr)
            } else {
                match self.tlb.access(self.cregfile[CREG_PID], addr >> 12, operation, kmode) {
                    TlbAccess::Hit(result) => Some(result | (addr & 0xFFF)),
                    TlbAccess::Fault(flags) => {
                        self.record_pending_tlb_fault(flags);
                        None
                    }
                }
            }
        } else {
            match self.tlb.access(self.cregfile[CREG_PID], addr >> 12, operation, kmode) {
                TlbAccess::Hit(result) => Some(result | (addr & 0xFFF)),
                TlbAccess::Fault(flags) => {
                    self.record_pending_tlb_fault(flags);
                    None
                }
            }
        }
    }

    fn save_state(&mut self) {
        // save state as an interrupt happens

        // save pc
        self.cregfile[CREG_EPC] = self.pc;

        // save flags
        self.cregfile[CREG_EFG] = self.cregfile[CREG_FLG];

        // disable interrupts
        self.cregfile[CREG_IMR] &= 0x7FFFFFFF;
    }

    fn raise_tlb_miss(&mut self, addr: u32, flags: u32) {
        if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            println!(
                "[core {}] exception tlb_miss mode={} addr=0x{:08X} flags=0x{:08X} pc=0x{:08X} psr=0x{:08X}",
                self.core_id,
                if self.get_kmode() { "kernel" } else { "user" },
                addr,
                flags,
                self.pc,
                self.cregfile[0]
            );
        }

        // save address and pid that caused exception
        self.cregfile[CREG_TLB] = (addr >> 12) | (self.cregfile[CREG_PID] << 20);
        self.cregfile[CREG_TLBF] = flags;

        self.save_state();

        self.psr_inc_checked(PSR_REASON_TLB_MISS);
        self.pc = self
            .mem_read32(EXC_TLB_MISS_VECTOR * 4)
            .expect("shouldnt fail");
    }

    fn raise_pending_tlb_miss(&mut self, addr: u32) {
        let flags = self.take_pending_tlb_fault();
        self.raise_tlb_miss(addr, flags);
    }

    fn raise_misaligned_pc(&mut self, pc: u32) {
        if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            println!(
                "[core {}] exception misaligned_pc pc=0x{:08X} psr=0x{:08X}",
                self.core_id, pc, self.cregfile[0]
            );
        }

        self.save_state();
        self.psr_inc_checked(PSR_REASON_MISALIGNED_PC);
        self.pc = self
            .mem_read32(EXC_MISALIGNED_PC_VECTOR * 4)
            .expect("misaligned-pc vector read should succeed");
    }

    // memory operations must be aligned
    fn mem_write8(&mut self, addr: u32, data: u8) -> bool {
        self.clear_pending_tlb_fault();
        if addr == 0 {
            println!(
                "Warning: core {} writing to virtual address 0x00000000 from pc 0x{:08X}",
                self.cregfile[9], self.pc
            );
        }

        let vaddr = addr;
        let addr = self.convert_mem_address(addr, 1);

        if let Some(addr) = addr {
            self.maybe_log_memmap_write(vaddr, addr, 1);
            self.maybe_watch(vaddr, WatchAccess::Write, data);
            self.memory.write(addr, data);
            true
        } else {
            false
        }
    }

    fn mem_write16(&mut self, addr: u32, data: u16) -> bool {
        self.clear_pending_tlb_fault();
        if (addr & 1) != 0 {
            // unaligned access
            println!("Warning: unaligned memory access at 0x{:08x}", addr);
        }
        if addr == 0 {
            println!(
                "Warning: core {} writing to virtual address 0x00000000 from pc 0x{:08X}",
                self.cregfile[9], self.pc
            );
        }
        let addr = addr & 0xFFFFFFFE;
        let bytes = data.to_le_bytes();
        let Some(paddr) = self.convert_mem_address(addr, 1) else {
            return false;
        };
        if paddr > PHYSMEM_MAX - 1 {
            return false;
        }
        let addrs = [paddr, paddr + 1];
        for (i, paddr) in addrs.iter().enumerate() {
            if let Some(region) = Self::memmap_region(*paddr) {
                if Self::warn_on_write(region) {
                    self.maybe_log_memmap_write(addr + i as u32, *paddr, 2);
                    break;
                }
            }
        }
        self.maybe_watch(addr, WatchAccess::Write, bytes[0]);
        self.maybe_watch(addr + 1, WatchAccess::Write, bytes[1]);
        self.memory.write_u16(paddr, data);
        true
    }

    fn mem_write32(&mut self, addr: u32, data: u32) -> bool {
        self.clear_pending_tlb_fault();
        if (addr & 3) != 0 {
            // unaligned access
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        if addr == 0 {
            println!(
                "Warning: core {} writing to virtual address 0x00000000 from pc 0x{:08X}",
                self.cregfile[9], self.pc
            );
        }
        let addr = addr & 0xFFFFFFFC;
        let bytes = data.to_le_bytes();
        let Some(paddr) = self.convert_mem_address(addr, 1) else {
            return false;
        };
        if paddr > PHYSMEM_MAX - 3 {
            return false;
        }
        let addrs = [paddr, paddr + 1, paddr + 2, paddr + 3];
        for (i, paddr) in addrs.iter().enumerate() {
            if let Some(region) = Self::memmap_region(*paddr) {
                if Self::warn_on_write(region) {
                    self.maybe_log_memmap_write(addr + i as u32, *paddr, 4);
                    break;
                }
            }
        }
        for (i, byte) in bytes.iter().enumerate() {
            self.maybe_watch(addr + i as u32, WatchAccess::Write, *byte);
        }
        self.memory.write_u32(paddr, data);
        true
    }

    fn mem_read8(&mut self, addr: u32) -> Option<u8> {
        self.clear_pending_tlb_fault();
        if addr == 0 {
            println!(
                "Warning: core {} reading from virtual address 0x00000000 from pc 0x{:08X}",
                self.cregfile[9], self.pc
            );
        }

        let vaddr = addr;
        let addr = self.convert_mem_address(addr, 0);

        if let Some(addr) = addr {
            let value = self.memory.read(addr);
            self.maybe_watch(vaddr, WatchAccess::Read, value);
            Some(value)
        } else {
            None
        }
    }

    fn mem_read16(&mut self, addr: u32) -> Option<u16> {
        self.clear_pending_tlb_fault();
        if (addr & 1) != 0 {
            // unaligned access
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        if addr == 0 {
            println!(
                "Warning: core {} reading from virtual address 0x00000000 from pc 0x{:08X}",
                self.cregfile[9], self.pc
            );
        }
        let addr = addr & 0xFFFFFFFE;
        let paddr = self.convert_mem_address(addr, 0)?;
        if paddr > PHYSMEM_MAX - 1 {
            return None;
        }
        let bytes = self.memory.read_u16(paddr).to_le_bytes();
        self.maybe_watch(addr, WatchAccess::Read, bytes[0]);
        self.maybe_watch(addr + 1, WatchAccess::Read, bytes[1]);
        Some(u16::from_le_bytes(bytes))
    }

    fn mem_read32(&mut self, addr: u32) -> Option<u32> {
        self.clear_pending_tlb_fault();
        if (addr & 3) != 0 {
            // unaligned access
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        if addr == 0 {
            println!(
                "Warning: core {} reading from virtual address 0x00000000 from pc 0x{:08X}",
                self.cregfile[9], self.pc
            );
        }
        let addr = addr & 0xFFFFFFFC;
        let paddr = self.convert_mem_address(addr, 0)?;
        if paddr > PHYSMEM_MAX - 3 {
            return None;
        }
        let bytes = self.memory.read_u32(paddr).to_le_bytes();
        for (i, byte) in bytes.iter().enumerate() {
            self.maybe_watch(addr + i as u32, WatchAccess::Read, *byte);
        }
        Some(u32::from_le_bytes(bytes))
    }

    fn mem_atomic_swap32(&mut self, addr: u32, value: u32) -> Option<u32> {
        self.clear_pending_tlb_fault();
        if (addr & 3) != 0 {
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        let addr = addr & 0xFFFFFFFC;
        let read_addr = self.convert_mem_address(addr, 0)?;
        let write_addr = self.convert_mem_address(addr, 1)?;
        if read_addr != write_addr {
            return None;
        }
        self.maybe_log_memmap_write(addr, write_addr, 4);
        let prev = self.memory.atomic_swap_u32(read_addr, value);
        let prev_bytes = prev.to_le_bytes();
        let new_bytes = value.to_le_bytes();
        for i in 0..4 {
            let vaddr = addr + i as u32;
            self.maybe_watch(vaddr, WatchAccess::Read, prev_bytes[i]);
            self.maybe_watch(vaddr, WatchAccess::Write, new_bytes[i]);
        }
        Some(prev)
    }

    fn mem_atomic_add32(&mut self, addr: u32, value: u32) -> Option<u32> {
        self.clear_pending_tlb_fault();
        if (addr & 3) != 0 {
            println!("Warning: unaligned memory access at {:08x}", addr);
        }
        let addr = addr & 0xFFFFFFFC;
        let read_addr = self.convert_mem_address(addr, 0)?;
        let write_addr = self.convert_mem_address(addr, 1)?;
        if read_addr != write_addr {
            return None;
        }
        self.maybe_log_memmap_write(addr, write_addr, 4);
        let prev = self.memory.atomic_add_u32(read_addr, value);
        let next = u32::wrapping_add(prev, value);
        let prev_bytes = prev.to_le_bytes();
        let next_bytes = next.to_le_bytes();
        for i in 0..4 {
            let vaddr = addr + i as u32;
            self.maybe_watch(vaddr, WatchAccess::Read, prev_bytes[i]);
            self.maybe_watch(vaddr, WatchAccess::Write, next_bytes[i]);
        }
        Some(prev)
    }

    fn read_phys32(&mut self, addr: u32) -> Option<u32> {
        if addr > PHYSMEM_MAX || addr + 3 > PHYSMEM_MAX {
            return None;
        }
        Some(self.memory.read_u32(addr))
    }

    // Debug reads bypass watchpoints so inspection doesn't change execution flow.
    fn read_phys8_debug(&mut self, addr: u32) -> Option<u8> {
        if addr > PHYSMEM_MAX {
            return None;
        }
        Some(self.memory.read(addr))
    }

    // Debug reads bypass watchpoints so inspection doesn't change execution flow.
    fn read_virt8_debug(&mut self, addr: u32) -> Option<u8> {
        self.convert_mem_address(addr, 0)
            .map(|paddr| self.memory.read(paddr))
    }

    fn fetch(&mut self, vaddr: u32) -> Option<u32> {
        self.clear_pending_tlb_fault();
        if (vaddr & 3) != 0 {
            self.raise_misaligned_pc(vaddr);
            return None;
        }
        if vaddr == 0 {
            println!("Warning: fetching from virtual address 0x00000000");
        }

        let paddr = self.convert_mem_address(vaddr, 2);

        if let Some(addr) = paddr {
            Some(self.memory.read_u32(addr))
        } else {
            None
        }
    }

    fn tick(&mut self) {
        self.check_for_interrupts();
        self.handle_interrupts();

        let clk_divider = self.memory.read_u32(CLK_REG_START);

        if !self.asleep && ((self.count % cmp::max(u32::wrapping_add(clk_divider, 1), 1)) == 0) {
            let fetch_pc = self.pc;
            let instr = self.fetch(fetch_pc);

            // Fetch can raise a synchronous exception before any instruction is
            // decoded, so avoid reclassifying that cycle as a TLB miss.
            if self.pc != fetch_pc {
                // Exception redirect already installed by fetch.
            } else if let Some(instr) = instr {
                self.execute(instr);
            } else {
                self.raise_pending_tlb_miss(fetch_pc);
            }
        }
        self.count = self.count.wrapping_add(1);
    }

    pub fn run(mut self, max_iters: u32, with_graphics: bool) -> Option<u32> {
        let mut graphics: Option<Graphics> = None;
        if with_graphics {
            graphics = Some(Graphics::new(
                self.memory.get_pixel_frame_buffer(),
                self.memory.get_tile_frame_buffer(),
                self.memory.get_tile_map(),
                self.memory.get_io_buffer(),
                self.memory.get_input_pending(),
                self.memory.get_tile_vscroll_register(),
                self.memory.get_tile_hscroll_register(),
                self.memory.get_pixel_vscroll_register(),
                self.memory.get_pixel_hscroll_register(),
                self.memory.get_sprite_map(),
                self.memory.get_tile_scale_register(),
                self.memory.get_pixel_scale_register(),
                self.memory.get_sprite_scale_registers(),
                self.memory.get_vga_status_register(),
                self.memory.get_vga_frame_register(),
                self.memory.get_pending_interrupt(),
            ));
        }

        // Return value and termination signal
        let ret: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
        let finished: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

        // Runs emulator on thread because graphics must use main thread
        let handle = thread::spawn({
            let ret_clone = Arc::clone(&ret);
            let finished_clone = Arc::clone(&finished);
            move || {
                self.count = 0;
                while !self.halted {
                    self.tick();
                    if max_iters != 0 && self.count > max_iters {
                        *ret_clone.lock().unwrap() = None;
                        *finished_clone.lock().unwrap() = true;
                        return;
                    }
                }

                // return the value in r3
                *ret_clone.lock().unwrap() = Some(self.regfile[1]);
                *finished_clone.lock().unwrap() = true;
            }
        });

        if with_graphics {
            graphics.unwrap().start(finished, false);
        }

        handle.join().unwrap();

        // return the value in r3
        return *ret.lock().unwrap();
    }

    // Purpose: run the multicore emulator and keep the shared memory alive for inspection.
    // Inputs: program path, runtime configuration, and optional SD preload images.
    // Outputs: core-0 r1 plus the shared memory state after all cores exit.
    pub fn run_multicore_with_memory(
        path: String,
        cores: usize,
        sched: ScheduleMode,
        max_iters: u32,
        with_graphics: bool,
        use_uart_rx: bool,
        sd_dma_ticks_per_word: u32,
        sd0_image: Option<&[u8]>,
        sd1_image: Option<&[u8]>,
    ) -> (Option<u32>, Arc<Memory>) {
        assert!((1..=4).contains(&cores), "cores must be in 1..=4");
        let image = load_program(&path);
        let memory: Arc<Memory> = Arc::new(Memory::new(
            image.instructions,
            use_uart_rx,
            sd_dma_ticks_per_word,
        ));
        if let Some(image) = sd0_image {
            memory.load_sd_image(SdSlot::Sd0, image);
        }
        if let Some(image) = sd1_image {
            memory.load_sd_image(SdSlot::Sd1, image);
        }
        let interrupts = InterruptController::new(cores);

        let finished = Arc::new(Mutex::new(false));
        let shared = Arc::new(RunShared::new(cores, Arc::clone(&finished)));

        let scheduler = match sched {
            ScheduleMode::Free => None,
            _ => Some(Scheduler::new(sched, cores)),
        };

        let mut graphics = None;
        if with_graphics {
            graphics = Some(Graphics::new(
                memory.get_pixel_frame_buffer(),
                memory.get_tile_frame_buffer(),
                memory.get_tile_map(),
                memory.get_io_buffer(),
                memory.get_input_pending(),
                memory.get_tile_vscroll_register(),
                memory.get_tile_hscroll_register(),
                memory.get_pixel_vscroll_register(),
                memory.get_pixel_hscroll_register(),
                memory.get_sprite_map(),
                memory.get_tile_scale_register(),
                memory.get_pixel_scale_register(),
                memory.get_sprite_scale_registers(),
                memory.get_vga_status_register(),
                memory.get_vga_frame_register(),
                memory.get_pending_interrupt(),
            ));
        }

        let mut handles = Vec::new();
        for core_id in 0..cores {
            let cpu = Emulator::from_shared(
                Arc::clone(&memory),
                Arc::clone(&interrupts),
                use_uart_rx,
                core_id as u32,
            );
            // Each core runs in its own thread to allow real races.
            let shared_clone = Arc::clone(&shared);
            let scheduler_clone = scheduler.clone();
            let handle = thread::spawn(move || {
                run_core_loop(cpu, max_iters, scheduler_clone, shared_clone, core_id);
            });
            handles.push(handle);
        }

        if let Some(mut graphics) = graphics {
            graphics.start(Arc::clone(&finished), false);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Return value is r1 from core 0.
        let results = shared.results.lock().unwrap();
        (results.get(0).copied().unwrap_or(None), memory)
    }

    // Purpose: run the multicore emulator to completion and return core 0's result.
    // Inputs: program path, runtime configuration, and optional SD preload images.
    // Outputs: core-0 r1, or None if the program failed to terminate.
    pub fn run_multicore(
        path: String,
        cores: usize,
        sched: ScheduleMode,
        max_iters: u32,
        with_graphics: bool,
        use_uart_rx: bool,
        sd_dma_ticks_per_word: u32,
        sd0_image: Option<&[u8]>,
        sd1_image: Option<&[u8]>,
    ) -> Option<u32> {
        let (result, _) = Self::run_multicore_with_memory(
            path,
            cores,
            sched,
            max_iters,
            with_graphics,
            use_uart_rx,
            sd_dma_ticks_per_word,
            sd0_image,
            sd1_image,
        );
        result
    }

    fn check_for_interrupts(&mut self) {
        // Input routing only needs a queue-empty check, not the full queue lock.
        let io_nonempty = self.memory.has_pending_input();
        self.interrupts
            .dispatch_input(self.use_uart_rx, io_nonempty);

        let ints = self.memory.check_interrupts();
        self.interrupts.dispatch_device_interrupts(ints);

        let pending = self.interrupts.take_pending(self.core_id as usize);
        if pending != 0 {
            // IPI payloads are copied into the core-local MBI register.
            if (pending & IPI_INTERRUPT_BIT) != 0 {
                self.cregfile[10] = self.interrupts.read_ipi_payload(self.core_id as usize);
            }
            self.cregfile[2] |= pending;
        }

        // Shared PIT countdown is advanced by core 0 only.
        if self.core_id == 0 && self.memory.tick_pit() {
            self.interrupts.broadcast_timer();
        }

        // Advance SD DMA after the timer update to share the same tick cadence.
        if self.core_id == 0 {
            self.memory.tick_sd_dma();
        }
    }

    fn handle_interrupts(&mut self) {
        if self.cregfile[3] >> 31 != 0 {
            // top bit activates/disables all interrupts
            let active_ints = self.cregfile[3] & self.read_isr();

            if active_ints == 0 {
                return;
            }

            if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
                println!(
                    "[core {}] interrupt {} (active={:08X} imr={:08X} pc={:08X})",
                    self.core_id,
                    format_interrupts(active_ints),
                    active_ints,
                    self.cregfile[3],
                    self.pc
                );
            }

            // Undo sleep; "mode sleep" advances to the next instruction.
            if self.asleep {
                if self.sleep_armed {
                    self.pc += 4;
                }
            }
            self.asleep = false;
            self.sleep_armed = false;

            self.save_state();

            // enter kernel mode
            self.psr_inc_checked("interrupt");

            // disable interrupts
            self.cregfile[3] &= 0x7FFFFFFF;

            if (active_ints >> 15) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xFF * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 14) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xFE * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 13) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xFD * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 12) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xFC * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 11) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xFB * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 10) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xFA * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 9) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF9 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 8) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF8 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 7) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF7 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 6) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF6 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 5) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF5 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 4) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF4 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 3) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF3 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 2) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF2 * 4)
                    .expect("this address shouldn't error");
            } else if (active_ints >> 1) & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF1 * 4)
                    .expect("this address shouldn't error");
            } else if active_ints & 1 != 0 {
                self.pc = self
                    .mem_read32(0xF0 * 4)
                    .expect("this address shouldn't error");
            }
        }
    }

    fn raise_exc_instr(&mut self) {
        // exec_instr

        if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            println!(
                "[core {}] exception invalid_instr pc=0x{:08X} psr=0x{:08X}",
                self.core_id, self.pc, self.cregfile[0]
            );
        }

        self.save_state();

        self.psr_inc_checked("invalid_instr");

        self.pc = self.mem_read32(0x80 * 4).expect("shouldn't fail");
        return;
    }

    fn execute(&mut self, instr: u32) {
        let opcode = instr >> 27; // opcode is top 5 bits of instruction

        match opcode {
            0 => self.alu_op(instr, false),
            1 => self.alu_op(instr, true),
            2 => self.load_upper_immediate(instr),

            // 32 bit mem instructions
            3 => self.mem_absolute(instr, 2),
            4 => self.mem_relative(instr, 2),
            5 => self.mem_imm(instr, 2),

            // 16 bit mem instructions
            6 => self.mem_absolute(instr, 1),
            7 => self.mem_relative(instr, 1),
            8 => self.mem_imm(instr, 1),

            // 8 bit mem instructions
            9 => self.mem_absolute(instr, 0),
            10 => self.mem_relative(instr, 0),
            11 => self.mem_imm(instr, 0),

            12 => self.branch_imm(instr),
            13 => self.branch_absolute(instr),
            14 => self.branch_relative(instr),

            15 => self.trap_instr(instr),

            22 => self.adpc(instr),

            // fadd
            16 => self.atomic_absolute(instr, 0),
            17 => self.atomic_relative(instr, 0),
            18 => self.atomic_imm(instr, 0),

            // swap
            19 => self.atomic_absolute(instr, 1),
            20 => self.atomic_relative(instr, 1),
            21 => self.atomic_imm(instr, 1),

            31 => self.kernel_instr(instr),
            _ => self.raise_exc_instr(),
        }
    }

    fn get_reg(&self, regnum: u32) -> u32 {
        if self.get_kmode() && regnum == 31 {
            // use ISP while handling exceptions or interrupts in kernel mode
            self.cregfile[8]
        } else {
            // normal register access
            self.regfile[regnum as usize]
        }
    }

    fn adpc(&mut self, instr: u32) {
        // adpc rA, i
        // rA <- pc + 4 + sign-extended 22-bit immediate (pc-relative to next instruction).
        let r_a = (instr >> 22) & 0x1F;
        let imm = (instr & 0x3FFFFF) as i32;
        let imm = (imm << 10) >> 10; // sign-extend 22 bits
        let pc = self.pc as i32;
        let value = pc.wrapping_add(4).wrapping_add(imm) as u32;
        self.write_reg(r_a, value);
        self.pc += 4;
    }

    fn write_reg(&mut self, regnum: u32, value: u32) {
        if self.get_kmode() && regnum == 31 {
            // use ISP while handling exceptions or interrupts in kernel mode
            self.cregfile[8] = value;
        } else {
            // normal register access
            if regnum != 0 {
                // r0 is always zero
                self.regfile[regnum as usize] = value;
            }
        }
    }

    fn decode_alu_imm(&mut self, op: u32, imm: u32) -> Option<u32> {
        match op {
            0..=6 => {
                // Bitwise op
                Some((imm & 0xFF) << (8 * ((imm >> 8) & 3)))
            }
            7..=13 => {
                // Shift op
                Some(imm & 0x1F)
            }
            14..=18 => {
                // Arithmetic op
                Some(imm | (0xFFFFF000 * ((imm >> 11) & 1))) // sign extend
            }
            _ => {
                self.raise_exc_instr();
                return None;
            }
        }
    }

    // 2nd operand is either register or immediate
    fn alu_op(&mut self, instr: u32, imm: bool) {
        // instruction format is
        // 00000aaaaabbbbbxxxxxxx?????ccccc
        // op (5 bits) | r_a (5 bits) | r_b (5 bits) | unused (7 bits) | op (5 bits) | r_c (5 bits)
        let r_a = (instr >> 22) & 0x1F;
        let r_b = (instr >> 17) & 0x1F;
        let op = if imm {
            (instr >> 12) & 0x1F
        } else {
            (instr >> 5) & 0x1F
        };

        // retrieve arguments
        let r_b = self.get_reg(r_b);

        let r_c = if imm {
            self.decode_alu_imm(op, instr & 0xFFF)
                .expect("immediate decoding failed")
        } else {
            let r_c = instr & 0x1F;
            self.get_reg(r_c)
        };

        let prev_carry = self.cregfile[5] & 1;

        self.cregfile[5] &= 0xFFFFFFF0; // clear arithmetic flags

        // carry flag is set differently for each instruction,
        // so its handled here. The other flags are all handled together
        let result = match op {
            0 => {
                r_b & r_c // and
            }
            1 => {
                !(r_b & r_c) // nand
            }
            2 => {
                r_b | r_c // or
            }
            3 => {
                !(r_b | r_c) // nor
            }
            4 => {
                r_b ^ r_c // xor
            }
            5 => {
                !(r_b ^ r_c) // xnor
            }
            6 => {
                !r_c // not
            }
            7 => {
                // set carry flag
                self.cregfile[5] |= (r_b >> if r_c > 0 { 32 - r_c } else { 0 } != 0) as u32;
                r_b << r_c // lsl
            }
            8 => {
                // set carry flag
                self.cregfile[5] |= (r_b & ((1 << r_c) - 1) != 0) as u32;
                r_b >> r_c // lsr
            }
            9 => {
                // set carry flag
                let carry = r_b & 1;
                let sign = r_b >> 31;
                self.cregfile[5] |= carry;
                (r_b >> r_c) | (0xFFFFFFFF * sign << if r_c > 0 { 32 - r_c } else { 0 }) // asr
            }
            10 => {
                // set carry flag
                let carry = r_b >> if r_c > 0 { 32 - r_c } else { 0 };
                self.cregfile[5] |= (carry != 0) as u32;
                (r_b << r_c) | carry // rotl
            }
            11 => {
                // set carry flag
                let carry = r_b & ((1 << r_c) - 1);
                self.cregfile[5] |= (carry != 0) as u32;
                (r_b >> r_c) | (carry << if r_c > 0 { 32 - r_c } else { 0 }) // rotr
            }
            12 => {
                // set carry flag
                let carry = if r_c > 0 { r_b >> (32 - r_c) } else { 0 };
                self.cregfile[5] |= (carry != 0) as u32;
                (r_b << r_c) | if r_c > 0 { prev_carry << (r_c - 1) } else { 0 } // lslc
            }
            13 => {
                // set carry flag
                let carry = r_b & ((1 << r_c) - 1);
                self.cregfile[5] |= (carry != 0) as u32;
                (r_b >> r_c) | (prev_carry << if r_c > 0 { 32 - r_c } else { 0 }) // lsrc
            }
            14 => {
                // add
                let result = u64::from(r_b) + u64::from(r_c);

                // set the carry flag
                self.cregfile[5] |= (result >> 32 != 0) as u32;

                result as u32
            }
            15 => {
                // addc
                let result = u64::from(r_c) + u64::from(r_b) + u64::from(prev_carry);

                // set the carry flag
                self.cregfile[5] |= (result >> 32 != 0) as u32;

                result as u32
            }
            16 => {
                // sub

                // two's complement
                // sub with immediate does imm - reg
                let result = if imm {
                    let r_b = 1 + u64::from(!r_b);
                    u64::from(r_c) + r_b
                } else {
                    let r_c = 1 + u64::from(!r_c);
                    r_c + u64::from(r_b)
                };

                // set the carry flag
                self.cregfile[5] |= (result >> 32 != 0) as u32;

                result as u32
            }
            17 => {
                // subb

                // two's complement
                let result = if imm {
                    let r_b = 1 + u64::from(!(u32::wrapping_add(u32::from(prev_carry == 0), r_b)));
                    u64::from(imm) + r_b
                } else {
                    let r_c = 1 + u64::from(!(u32::wrapping_add(u32::from(prev_carry == 0), r_c)));
                    r_c + u64::from(r_b)
                };

                // set the carry flag
                self.cregfile[5] |= (result >> 32 != 0) as u32;

                result as u32
            }
            18 => {
                // sxtb (sign extend byte)
                let byte = r_c & 0xFF;
                if (byte & 0x80) != 0 {
                    byte | 0xFFFFFF00
                } else {
                    byte
                }
            }
            19 => {
                // sxtd (sign extend double)
                let half = r_c & 0xFFFF;
                if (half & 0x8000) != 0 {
                    half | 0xFFFF0000
                } else {
                    half
                }
            }
            20 => {
                // tncb (truncate to byte)
                r_c & 0xFF
            }
            21 => {
                // tncd (truncate to double)
                r_c & 0xFFFF
            }
            _ => {
                self.raise_exc_instr();
                return;
            }
        };

        // never update r0
        self.write_reg(r_a, result);

        self.update_flags(result, r_b, r_c, op);

        self.pc += 4;
    }

    fn load_upper_immediate(&mut self, instr: u32) {
        // store imm << 10 in r_a
        let r_a = (instr >> 22) & 0x1F;
        let imm = (instr & 0x03FFFFF) << 10;

        self.write_reg(r_a, imm);

        self.pc += 4;
    }

    fn mem_absolute(&mut self, instr: u32, size: u8) {
        // instruction format is
        // 00011aaaaabbbbb?yyzziiiiiiiiiiii
        // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | y (2 bits) | z (2 bits) | imm (12 bits)

        let r_a = (instr >> 22) & 0x1F;
        let r_b = (instr >> 17) & 0x1F;
        let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
        let y = (instr >> 14) & 3; // offset type: 0 = signed offset, 1 = preinc, 2 = postinc, 3 = reserved
        let z = (instr >> 12) & 3; // shift amount
        let imm = instr & 0xFFF;

        // sign extend imm
        let imm = imm | (0xFFFFF000 * ((imm >> 11) & 1));
        // shift imm
        let imm = imm << z;

        if y >= 4 {
            self.raise_exc_instr();
            return;
        };

        // get addr
        let r_b_out = self.get_reg(r_b);
        let addr = if y == 2 {
            r_b_out
        } else {
            u32::wrapping_add(r_b_out, imm)
        }; // check for postincrement

        if is_load {
            let data = match size {
                0 => {
                    // byte
                    self.mem_read8(addr).map(|v| u32::from(v))
                }
                1 => {
                    // halfword
                    self.mem_read16(addr).map(|v| u32::from(v))
                }
                2 => {
                    // word
                    self.mem_read32(addr)
                }
                _ => {
                    panic!("invalid size for mem instruction");
                }
            };

            if let Some(data) = data {
                self.write_reg(r_a, data);
            } else {
                // TLB Miss
                self.raise_pending_tlb_miss(addr);
                return;
            };
        } else {
            // is a store
            let data = self.get_reg(r_a);
            let success = match size {
                0 => {
                    // byte
                    self.mem_write8(addr, data as u8)
                }
                1 => {
                    // halfword
                    self.mem_write16(addr, data as u16)
                }
                2 => {
                    // word
                    self.mem_write32(addr, data)
                }
                _ => {
                    panic!("invalid size for mem instruction");
                }
            };
            if !success {
                // TLB Miss
                self.raise_pending_tlb_miss(addr);
                return;
            }
        }

        if y == 1 || y == 2 {
            // pre or post increment
            self.write_reg(r_b, u32::wrapping_add(r_b_out, imm));
        }

        self.pc += 4;
    }

    fn mem_relative(&mut self, instr: u32, size: u8) {
        // instruction format is
        // 00100aaaaabbbbb?iiiiiiiiiiiiiiii
        // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | imm (16 bits)

        let r_a = (instr >> 22) & 0x1F;
        let r_b = (instr >> 17) & 0x1F;
        let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
        let imm = instr & 0xFFFF;

        // sign extend imm
        let imm = imm | (0xFFFF0000 * ((imm >> 15) & 1));

        // get addr
        let r_b_out = self.get_reg(r_b);
        let addr = u32::wrapping_add(r_b_out, imm);

        // make addr pc-relative
        let addr = u32::wrapping_add(addr, self.pc);
        let addr = u32::wrapping_add(addr, 4);

        if is_load {
            let data = match size {
                0 => {
                    // byte
                    self.mem_read8(addr).map(|v| u32::from(v))
                }
                1 => {
                    // halfword
                    self.mem_read16(addr).map(|v| u32::from(v))
                }
                2 => {
                    // word
                    self.mem_read32(addr)
                }
                _ => {
                    panic!("invalid size for mem instruction");
                }
            };

            if let Some(data) = data {
                self.write_reg(r_a, data);
            } else {
                // TLB Miss
                self.raise_pending_tlb_miss(addr);
                return;
            };
        } else {
            // is a store
            let data = self.get_reg(r_a);

            let success = match size {
                0 => {
                    // byte
                    self.mem_write8(addr, data as u8)
                }
                1 => {
                    // halfword
                    self.mem_write16(addr, data as u16)
                }
                2 => {
                    // word
                    self.mem_write32(addr, data)
                }
                _ => {
                    panic!("invalid size for mem instruction");
                }
            };

            if !success {
                // TLB Miss
                self.raise_pending_tlb_miss(addr);
                return;
            }
        }

        self.pc += 4;
    }

    fn mem_imm(&mut self, instr: u32, size: u8) {
        // instruction format is
        // 00101aaaaa?iiiiiiiiiiiiiiiiiiiii
        // op (5 bits) | r_a (5 bits) | op (1 bit) | imm (21 bits)

        let r_a = (instr >> 22) & 0x1F;
        let is_load = ((instr >> 21) & 1) != 0; // is this a load? else is store
        let imm = instr & 0x1FFFFF;

        // sign extend imm
        let imm = imm | (0xFFE00000 * ((imm >> 20) & 1));

        // get addr
        let addr = u32::wrapping_add(imm, self.pc);
        let addr = u32::wrapping_add(addr, 4);

        if is_load {
            let data = match size {
                0 => {
                    // byte
                    self.mem_read8(addr).map(|v| u32::from(v))
                }
                1 => {
                    // halfword
                    self.mem_read16(addr).map(|v| u32::from(v))
                }
                2 => {
                    // word
                    self.mem_read32(addr)
                }
                _ => {
                    panic!("invalid size for mem instruction");
                }
            };

            if let Some(data) = data {
                self.write_reg(r_a, data);
            } else {
                // TLB Miss
                self.raise_pending_tlb_miss(addr);
                return;
            };
        } else {
            // is a store
            let data = self.get_reg(r_a);

            let success = match size {
                0 => {
                    // byte
                    self.mem_write8(addr, data as u8)
                }
                1 => {
                    // halfword
                    self.mem_write16(addr, data as u16)
                }
                2 => {
                    // word
                    self.mem_write32(addr, data)
                }
                _ => {
                    panic!("invalid size for mem instruction");
                }
            };

            if !success {
                // TLB Miss
                self.raise_pending_tlb_miss(addr);
                return;
            }
        }

        self.pc += 4;
    }

    fn atomic_absolute(&mut self, instr: u32, type_: u8) {
        // instruction format is
        // 10000aaaaabbbbbccccciiiiiiiiiiii - fadd
        // opcode is 10011 for swap
        // op (5 bits) | r_a (5 bits) | r_c (5 bits) | r_b (5 bits) | imm (12 bits)

        let r_a = (instr >> 22) & 0x1F;
        let r_c = (instr >> 17) & 0x1F;
        let r_b = (instr >> 12) & 0x1F;
        let imm = instr & 0xFFF;

        // sign extend imm
        let imm = imm | (0xFFFFF000 * ((imm >> 11) & 1));

        // get addr
        let r_b_out = self.get_reg(r_b);
        let r_c_out = self.get_reg(r_c);
        let addr = u32::wrapping_add(r_b_out, imm);

        let data = match type_ {
            0 => self.mem_atomic_add32(addr, r_c_out),
            1 => self.mem_atomic_swap32(addr, r_c_out),
            _ => panic!("invalid atomic type"),
        };
        if let Some(data) = data {
            self.write_reg(r_a, data);
        } else {
            // TLB Miss
            self.raise_pending_tlb_miss(addr);
            return;
        }

        self.pc += 4;
    }

    fn atomic_relative(&mut self, instr: u32, type_: u8) {
        // instruction format is
        // 10001aaaaabbbbbccccciiiiiiiiiiii
        // or opcode is 10100
        // op (5 bits) | r_a (5 bits) | r_c (5 bits) | r_b (5 bits) | imm (12 bits)

        let r_a = (instr >> 22) & 0x1F;
        let r_c = (instr >> 17) & 0x1F;
        let r_b = (instr >> 12) & 0x1F;
        let imm = instr & 0xFFF;

        // sign extend imm
        let imm = imm | (0xFFFFF000 * ((imm >> 11) & 1));

        // get addr
        let r_b_out = self.get_reg(r_b);
        let r_c_out = self.get_reg(r_c);
        let addr = u32::wrapping_add(r_b_out, imm);

        // make addr pc-relative
        let addr = u32::wrapping_add(addr, self.pc);
        let addr = u32::wrapping_add(addr, 4);

        let data = match type_ {
            0 => self.mem_atomic_add32(addr, r_c_out),
            1 => self.mem_atomic_swap32(addr, r_c_out),
            _ => panic!("invalid atomic type"),
        };
        if let Some(data) = data {
            self.write_reg(r_a, data);
        } else {
            // TLB Miss
            self.raise_pending_tlb_miss(addr);
            return;
        }

        self.pc += 4;
    }

    fn atomic_imm(&mut self, instr: u32, type_: u8) {
        // instruction format is
        // 10010aaaaabbbbbiiiiiiiiiiiiiiiii
        // or opcode is 10101
        // op (5 bits) | r_a (5 bits) | r_b (5 bits) | imm (17 bits)

        let r_a = (instr >> 22) & 0x1F;
        let r_c = (instr >> 17) & 0x1F;
        let imm = instr & 0x1FFFF;

        // sign extend imm
        let imm = imm | (0xFFFE0000 * ((imm >> 16) & 1));

        // get addr
        let r_c_out = self.get_reg(r_c);

        // make addr pc-relative
        let addr = u32::wrapping_add(imm, self.pc);
        let addr = u32::wrapping_add(addr, 4);

        let data = match type_ {
            0 => self.mem_atomic_add32(addr, r_c_out),
            1 => self.mem_atomic_swap32(addr, r_c_out),
            _ => panic!("invalid atomic type"),
        };
        if let Some(data) = data {
            self.write_reg(r_a, data);
        } else {
            // TLB Miss
            self.raise_pending_tlb_miss(addr);
            return;
        }

        self.pc += 4;
    }

    fn get_branch_condition(&mut self, op: u32) -> Option<bool> {
        let carry = (self.cregfile[5] & 1) != 0;
        let zero = (self.cregfile[5] & 2) != 0;
        let sign = (self.cregfile[5] & 4) != 0;
        let overflow = (self.cregfile[5] & 8) != 0;

        match op {
            0 => Some(true),                       // br
            1 => Some(zero),                       // bz
            2 => Some(!zero),                      // bnz
            3 => Some(sign),                       // bs
            4 => Some(!sign),                      // bns
            5 => Some(carry),                      // bc
            6 => Some(!carry),                     // bnc
            7 => Some(overflow),                   // bo
            8 => Some(!overflow),                  // bno
            9 => Some(!zero && !sign),             // bps
            10 => Some(zero || sign),              // bnps
            11 => Some(sign == overflow && !zero), // bg
            12 => Some(sign == overflow),          // bge
            13 => Some(sign != overflow && !zero), // bl
            14 => Some(sign != overflow || zero),  // ble
            15 => Some(!zero && carry),            // ba
            16 => Some(carry || zero),             // bae
            17 => Some(!carry && !zero),           // bb
            18 => Some(!carry || zero),            // bbe
            _ => {
                self.raise_exc_instr();
                return None;
            }
        }
    }

    fn branch_imm(&mut self, instr: u32) {
        // instruction format is
        // 01100?????iiiiiiiiiiiiiiiiiiiiii
        // op (5 bits) | op (5 bits) | imm (22 bits)
        let op = (instr >> 22) & 0x1F;
        let imm = instr & 0x3FFFFF;

        // sign extend
        let imm = imm | (0xFFC00000 * ((imm >> 21) & 1));

        if let Some(branch) = self.get_branch_condition(op) {
            if branch {
                self.pc =
                    u32::wrapping_add(self.pc, u32::wrapping_add(4, u32::wrapping_mul(imm, 4)));
            } else {
                self.pc += 4;
            }
        } else {
            return;
        }
    }

    fn branch_absolute(&mut self, instr: u32) {
        // instruction format is
        // 01101?????xxxxxxxxxxxxaaaaabbbbb
        // op (5 bits) | op (5 bits) | unused (12 bits) | r_a (5 bits) | r_b (5 bits)
        let op = (instr >> 22) & 0x1F;
        let r_a = (instr >> 5) & 0x1F;
        let r_b = instr & 0x1F;

        // get address
        let r_b = self.get_reg(r_b);

        if let Some(branch) = self.get_branch_condition(op) {
            if branch {
                self.write_reg(r_a, self.pc + 4);
                self.pc = r_b;
            } else {
                self.pc += 4;
            }
        } else {
            return;
        }
    }

    fn branch_relative(&mut self, instr: u32) {
        // instruction format is
        // 01110?????xxxxxxxxxxxxaaaaabbbbb
        // op (5 bits) | op (5 bits) | unused (12 bits) | r_a (5 bits) | r_b (5 bits)
        let op = (instr >> 22) & 0x1F;
        let r_a = (instr >> 5) & 0x1F;
        let r_b = instr & 0x1F;

        // get address
        let r_b = self.get_reg(r_b);

        if let Some(branch) = self.get_branch_condition(op) {
            if branch {
                self.write_reg(r_a, self.pc + 4);
                self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4, r_b));
            } else {
                self.pc += 4;
            }
        } else {
            return;
        }
    }

    fn trap_instr(&mut self, instr: u32) {
        const TRAP_PAYLOAD_MASK: u32 = 0x07FF_FFFF;
        const TRAP_VECTOR_ADDR: u32 = 0x04;

        if (instr & TRAP_PAYLOAD_MASK) != 0 {
            // Reserved trap encodings are invalid instructions, not nested
            // trap+invalid-instruction entries.
            self.raise_exc_instr();
            return;
        }

        // Trap entry resumes at the following instruction, but otherwise
        // snapshots architectural trap state like any other exception entry.
        self.save_state();
        self.cregfile[4] = self.pc.wrapping_add(4);
        self.psr_inc_checked("trap");

        self.pc = self
            .mem_read32(TRAP_VECTOR_ADDR)
            .expect("trap vector read should succeed");
    }

    // carry flag handled separately in each alu operation
    fn update_flags(&mut self, result: u32, lhs: u32, rhs: u32, op: u32) {
        let result_sign = result >> 31;
        let lhs_sign = lhs >> 31;
        let rhs_sign = rhs >> 31;

        let is_sub = op == 16 || op == 17;

        // set the zero flag
        self.cregfile[5] |= ((result == 0) as u32) << 1;
        // set the sign flag
        self.cregfile[5] |= ((result_sign != 0) as u32) << 2;
        // set the overflow flag
        self.cregfile[5] |= if is_sub {
            (((result_sign != lhs_sign) && (lhs_sign != rhs_sign)) as u32) << 3
        } else {
            (((result_sign != lhs_sign) && (lhs_sign == rhs_sign)) as u32) << 3
        }
    }

    fn kernel_instr(&mut self, instr: u32) {
        if !self.get_kmode() {
            // exec_priv
            assert!(self.cregfile[0] == 0);

            if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
                println!(
                    "[core {}] exception priv pc=0x{:08X} psr=0x{:08X}",
                    self.core_id, self.pc, self.cregfile[0]
                );
            }

            self.save_state();

            self.psr_inc_checked("priv");

            self.pc = self.mem_read32(0x81 * 4).expect("shouldn't fail");
            return;
        }

        assert!(self.cregfile[0] > 0);

        let op = (instr >> 12) & 0x1F;

        match op {
            0 => self.tlb_op(instr),
            1 => self.crmv_op(instr),
            2 => self.mode_op(instr),
            3 => {
                if ((instr >> 11) & 1) != 0 {
                    self.raise_exc_instr();
                    return;
                }
                self.rfe(instr)
            }
            4 => self.ipi_op(instr),
            5 => self.eoi_op(instr),
            _ => {
                self.raise_exc_instr();
                return;
            }
        }
    }

    fn tlb_op(&mut self, instr: u32) {
        let op = (instr >> 10) & 3;
        let ra = (instr >> 22) & 0x1F;
        let rb = (instr >> 17) & 0x1F;

        let rb = self.get_reg(rb);
        // ra has PPN, rb has VPN
        if op == 0 {
            // tlbr
            if ra != 0 {
                if let Some(val) = self.tlb.read(self.cregfile[1], rb >> 12) {
                    self.write_reg(ra, val);
                } else {
                    self.write_reg(ra, 0);
                }
            }
        } else if op == 1 {
            // tlbw
            let ra = self.get_reg(ra);
            self.tlb.write(self.cregfile[1], rb >> 12, ra & 0x7FFFFFF);
        } else if op == 2 {
            // tlbi
            self.tlb.invalidate(self.cregfile[1], rb >> 12);
        } else {
            // tlbc
            self.tlb.clear();
        }
        self.pc += 4;
    }

    fn crmv_op(&mut self, instr: u32) {
        let op = (instr >> 10) & 3;
        let ra = (instr >> 22) & 0x1F;
        let rb = (instr >> 17) & 0x1F;

        // don't use get_reg/write_reg here because
        // crmv doesn't respect the r31 => kernel stack pointer alias

        if op == 0 {
            // crmv crA, rB
            let rb = self.regfile[rb as usize];
            self.write_creg(ra as usize, rb);
        } else if op == 1 {
            // crmv rA, crB
            if ra != 0 {
                let rb = self.read_creg(rb as usize);
                self.regfile[ra as usize] = rb;
            }
        } else if op == 2 {
            // crmv crA, crB
            let rb = self.read_creg(rb as usize);
            self.write_creg(ra as usize, rb);
        } else {
            // crmv rA, rB
            if ra != 0 {
                let rb = self.regfile[rb as usize];
                self.regfile[ra as usize] = rb;
            }
        }
        self.pc += 4;
    }

    fn ipi_op(&mut self, instr: u32) {
        let ra = (instr >> 22) & 0x1F;
        let all = ((instr >> 11) & 1) != 0;
        // Payload comes from MBO (cr11).
        let payload = self.cregfile[11];

        if all {
            let mask = self.interrupts.send_ipi_all(self.core_id as usize, payload);
            if ra != 0 {
                self.write_reg(ra, mask);
            }
        } else {
            let target = (instr & 0x3) as usize;
            let success = self.interrupts.send_ipi(target, payload);
            if ra != 0 {
                self.write_reg(ra, if success { 1 } else { 0 });
            }
        }

        self.pc += 4;
    }

    fn eoi_op(&mut self, instr: u32) {
        let all = ((instr >> 11) & 1) != 0;
        let cleared_mask = if all {
            u32::MAX
        } else {
            1u32 << (instr & 0xF)
        };
        let next_isr = self.cregfile[2] & !cleared_mask;
        self.write_isr(next_isr);
        self.pc += 4;
    }

    fn mode_op(&mut self, instr: u32) {
        let op = (instr >> 10) & 3;

        if op == 0 {
            // mode run
            self.pc += 4;
        } else if op == 1 {
            // mode sleep
            self.asleep = true;
            // Mark as a sleep instruction so interrupts advance PC.
            self.sleep_armed = true;
        } else {
            // mode halt
            self.halted = true;
        }
    }

    fn rfe(&mut self, instr: u32) {
        if TRACE_INTERRUPTS.load(Ordering::Relaxed) {
            println!(
                "[core {}] rfe instr=0x{:08X} pc=0x{:08X}",
                self.core_id, instr, self.pc
            );
        }
        // update kernel mode
        self.psr_dec("rfe");

        // Both trap-return encodings restore the global interrupt-enable bit.
        self.cregfile[3] |= 0x80000000;

        // restore pc
        self.pc = self.cregfile[4];

        // restore flags
        self.cregfile[5] = self.cregfile[6];
    }
}

fn run_core_loop(
    mut cpu: Emulator,
    max_iters: u32,
    scheduler: Option<Arc<Scheduler>>,
    shared: Arc<RunShared>,
    core_id: usize,
) {
    cpu.count = 0;
    loop {
        if shared.should_stop() {
            if let Some(sched) = &scheduler {
                sched.stop();
            }
            break;
        }
        if let Some(sched) = &scheduler {
            // Non-free scheduling blocks until this core is chosen.
            if !sched.wait_turn(core_id) {
                break;
            }
        }
        if shared.should_stop() {
            if let Some(sched) = &scheduler {
                sched.stop();
            }
            break;
        }
        if cpu.halted {
            // Any core halting stops the entire system.
            shared.request_stop();
            if let Some(sched) = &scheduler {
                sched.mark_halted(core_id);
                sched.stop();
            }
            break;
        }

        // Advance one CPU tick per scheduling turn.
        cpu.tick();

        if cpu.halted {
            // Any core halting stops the entire system.
            shared.request_stop();
            if let Some(sched) = &scheduler {
                sched.mark_halted(core_id);
                sched.stop();
            }
            break;
        }

        if max_iters != 0 && cpu.count > max_iters {
            shared.request_stop();
            if let Some(sched) = &scheduler {
                sched.stop();
            }
            break;
        }

        if let Some(sched) = &scheduler {
            sched.finish_turn(core_id);
        }
    }

    shared.record_exit(core_id, cpu.regfile[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_isr_preserves_concurrently_pending_ipi() {
        let memory = Arc::new(Memory::new(HashMap::new(), false, 1));
        let interrupts = InterruptController::new(2);
        let mut cpu = Emulator::from_shared(Arc::clone(&memory), Arc::clone(&interrupts), false, 0);

        cpu.cregfile[2] = TIMER_INTERRUPT_BIT;

        assert!(interrupts.send_ipi(0, 0x1234_5678));

        cpu.write_isr(0);

        assert_eq!(
            cpu.cregfile[2], IPI_INTERRUPT_BIT,
            "writing ISR to clear one interrupt must preserve a concurrently pending IPI",
        );
        assert_eq!(
            cpu.cregfile[10], 0x1234_5678,
            "MBI must reflect the visible pending IPI payload",
        );

        cpu.check_for_interrupts();

        assert_eq!(
            cpu.cregfile[2], IPI_INTERRUPT_BIT,
            "taking the queued pending IPI on the next tick must not change the visible ISR bit",
        );
        assert_eq!(
            cpu.cregfile[10], 0x1234_5678,
            "the queued IPI payload must remain stable after the next tick snapshots it",
        );
    }

    #[test]
    fn crmv_write_to_isr_is_ignored() {
        let memory = Arc::new(Memory::new(HashMap::new(), false, 1));
        let interrupts = InterruptController::new(1);
        let mut cpu = Emulator::from_shared(memory, interrupts, false, 0);

        cpu.cregfile[2] = TIMER_INTERRUPT_BIT;
        cpu.regfile[1] = 0xFFFF_FFFF;

        let instr = (31u32 << 27) | (2u32 << 22) | (1u32 << 17) | (1u32 << 12);
        cpu.crmv_op(instr);

        assert_eq!(
            cpu.cregfile[2], TIMER_INTERRUPT_BIT,
            "crmv writes to ISR must be ignored so interrupt acknowledgement goes through eoi",
        );
    }

    #[test]
    fn eoi_specific_clears_only_selected_isr_bit() {
        let memory = Arc::new(Memory::new(HashMap::new(), false, 1));
        let interrupts = InterruptController::new(1);
        let mut cpu = Emulator::from_shared(memory, interrupts, false, 0);

        cpu.cregfile[2] = TIMER_INTERRUPT_BIT | SD_INTERRUPT_BIT;

        let instr = (31u32 << 27) | (5u32 << 12);
        cpu.eoi_op(instr);

        assert_eq!(
            cpu.cregfile[2], SD_INTERRUPT_BIT,
            "eoi n must clear only the requested ISR bit",
        );
    }

    #[test]
    fn eoi_all_preserves_concurrently_pending_ipi() {
        let memory = Arc::new(Memory::new(HashMap::new(), false, 1));
        let interrupts = InterruptController::new(2);
        let mut cpu = Emulator::from_shared(Arc::clone(&memory), Arc::clone(&interrupts), false, 0);

        cpu.cregfile[2] = TIMER_INTERRUPT_BIT | SD_INTERRUPT_BIT;
        assert!(interrupts.send_ipi(0, 0xCAFE_BABE));

        let instr = (31u32 << 27) | (5u32 << 12) | (1u32 << 11);
        cpu.eoi_op(instr);

        assert_eq!(
            cpu.cregfile[2], IPI_INTERRUPT_BIT,
            "eoi all must clear handled ISR bits without dropping a concurrently pending IPI",
        );
        assert_eq!(
            cpu.cregfile[10], 0xCAFE_BABE,
            "eoi all must expose the visible pending IPI payload in MBI",
        );
    }
}
