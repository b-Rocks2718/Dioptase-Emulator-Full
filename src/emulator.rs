use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::cmp;

use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::memory::{
  Memory, 
  PHYSMEM_MAX, 
  PIT_START, CLK_REG_START,
  SD_INTERRUPT_BIT, VGA_INTERRUPT_BIT
};

use crate::graphics::Graphics;

mod debugger;

// Global toggle for interrupt tracing output.
static TRACE_INTERRUPTS: AtomicBool = AtomicBool::new(false);

pub fn set_trace_interrupts(enabled: bool) {
  TRACE_INTERRUPTS.store(enabled, Ordering::Relaxed);
}

#[derive(Debug)]
pub struct RandomCache {
    private_table : HashMap<(u32, u32), u32>,
    private_size : usize,
    private_capacity : usize,
    global_table : HashMap<u32, u32>,
    global_size : usize,
    global_capacity : usize,
}

impl RandomCache {
  pub fn new(capacity : usize) -> RandomCache  {
    RandomCache {
      private_table : HashMap::new(),
      private_size : 0,
      private_capacity : capacity,
      global_table : HashMap::new(),
      global_size : 0,
      global_capacity : capacity,
    }
  }

  pub fn access(&self, pid : u32, vpn : u32, operation : u32, kmode : bool) -> Option<u32> {
    // used whenever a memory access is made

    assert!(self.private_size <= self.private_capacity);
    assert!(self.global_size <= self.global_capacity);

    // operations: 0 => read, 1 => write, 2 => fetch

    let key = (pid, vpn);
    let result = self.private_table.get(&key).copied().and_then(|v|
      if operation == 0 {
        // read operation 
        if v & 0x00000001 == 0 {
          // not readable
          None
        } else {
          Some(v)
        }
      } else if operation == 1 {
        // write operation
        if v & 0x00000002 == 0 {
          // not writable
          None
        } else {
          Some(v)
        }
      } else if operation == 2 {
        // fetch operation
        if v & 0x00000004 == 0 {
          // not executable
          None
        } else {
          Some(v)
        }
      } else {
        panic!("invalid operation code");
      }
    ).and_then(|v|
      if !kmode {
        if v & 0x00000008 == 0 {
          // user mode access not allowed
          None
        } else {
          Some(v)
        }
      } else {
        Some(v)
      }
    ).map(|v| v & 0xFFFFF000);

    if result.is_some() {
      return result;
    } else {
      // try global table
      self.global_table.get(&vpn).copied().and_then(|v|
        if operation == 0 {
          // read operation 
          if v & 0x00000001 == 0 {
            // not readable
            None
          } else {
            Some(v)
          }
        } else if operation == 1 {
          // write operation
          if v & 0x00000002 == 0 {
            // not writable
            None
          } else {
            Some(v)
          }
        } else if operation == 2 {
          // fetch operation
          if v & 0x00000004 == 0 {
            // not executable
            None
          } else {
            Some(v)
          }
        } else {
          panic!("invalid operation code");
        }
      ).and_then(|v|
        if !kmode {
          if v & 0x00000008 == 0 {
            // user mode access not allowed
            None
          } else {
            Some(v)
          }
        } else {
          Some(v)
        }
      ).map(|v| v & 0xFFFFF000)
    }
  }

  pub fn read(&self, pid : u32, vpn : u32) -> Option<u32> {
    // used by tlbr instruction

    assert!(self.private_size <= self.private_capacity);
    assert!(self.global_size <= self.global_capacity);
    let result = self.private_table.get(&(pid, vpn)).copied();

    if result.is_some() {
      return result;
    } else {
      // try global table
      self.global_table.get(&vpn).copied()
    }
  }

  pub fn write(&mut self, pid : u32, vpn: u32, ppn : u32){
    if ppn & 0x00000010 != 0 {
      // global entry
      if !self.global_table.contains_key(&vpn) {
        if self.global_size < self.global_capacity {
          self.global_size += 1;
        } else {
          // remove an entry
          let evict = {
            let mut keys = self.global_table.keys();
            keys.next().cloned().expect("size was nonzero, this should work")
          };
          self.global_table.remove(&evict);
        }
      }

      // will replace old mapping if one existed
      self.global_table.insert(vpn, ppn);
      assert!(self.global_size <= self.global_capacity);

    } else {
      // private entry
      if !self.private_table.contains_key(&(pid, vpn)) {
        if self.private_size < self.private_capacity {
          self.private_size += 1;
        } else {
          // remove an entry
          let evict = {
            let mut keys = self.private_table.keys();
            keys.next().cloned().expect("size was nonzero, this should work")
          };
          self.private_table.remove(&evict);
        }
      }

      // will replace old mapping if one existed
      self.private_table.insert((pid, vpn), ppn);

      assert!(self.private_size <= self.private_capacity);
    }
  }

  pub fn invalidate(&mut self, pid : u32, vpn : u32){
    if self.private_table.contains_key(&(pid, vpn)) {
      self.private_size -= 1;
    }
    if self.global_table.contains_key(&vpn) {
      self.global_size -= 1;
    }

    self.private_table.remove(&(pid, vpn));
    self.global_table.remove(&vpn);
  }

  pub fn clear(&mut self){
    self.private_size = 0;
    self.global_size = 0;
    self.private_table.drain();
    self.global_table.drain();
  }

  fn debug_dump(&self) {
    println!(
      "TLB private: {}/{} entries",
      self.private_size, self.private_capacity
    );
    if self.private_table.is_empty() {
      println!("  (empty)");
    } else {
      for ((pid, vpn), entry) in &self.private_table {
        println!("  pid {:08X} vpn {:08X} -> {:08X}", pid, vpn, entry);
      }
    }
    println!(
      "TLB global: {}/{} entries",
      self.global_size, self.global_capacity
    );
    if self.global_table.is_empty() {
      println!("  (empty)");
    } else {
      for (vpn, entry) in &self.global_table {
        println!("  vpn {:08X} -> {:08X}", vpn, entry);
      }
    }
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
    parts.push("sd");
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
        next_vga: 0,
        kb_inflight: None,
        uart_inflight: None,
      }),
    })
  }

  fn set_pending_bits(&self, core: usize, bits: u32) {
    self.pending[core].fetch_or(bits, Ordering::Release);
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
    if pending & VGA_INTERRUPT_BIT != 0 {
      // VGA interrupts go to one core at a time, round-robin.
      let core = routes.next_vga % self.cores;
      routes.next_vga = (routes.next_vga + 1) % self.cores;
      self.set_pending_bits(core, VGA_INTERRUPT_BIT);
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
  kmode : bool,
  regfile : [u32; 32], // r0 - r31
  cregfile : [u32; 12], // PSR, PID, ISR, IMR, EPC, FLG, unused, TLB, KSP, CID, MBI, MBO
  // in FLG, flags are: carry | zero | sign | overflow
  memory : Arc<Memory>,
  interrupts: Arc<InterruptController>,
  tlb : RandomCache,
  pc : u32,
  asleep : bool,
  // Distinguish "mode sleep" from a core that starts asleep.
  sleep_armed: bool,
  halted : bool,
  timer : u32,
  count : u32,
  core_id: u32,
  use_uart_rx: bool,
  watchpoints: Vec<Watchpoint>,
  watchpoint_hit: Option<WatchpointHit>,
}

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where P: AsRef<Path>, {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

// Label -> address list (labels can appear multiple times across sections).
type LabelMap = HashMap<String, Vec<u32>>;

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
  let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
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

// Load hex (or .debug) program and collect any embedded labels.
fn load_program(path: &str) -> (HashMap<u32, u8>, LabelMap) {
  let mut instructions = HashMap::new();
  let mut labels = LabelMap::new();

  let lines = read_lines(path).expect("Couldn't open input file");
  let mut pc: u32 = 0;
  for line in lines.map_while(Result::ok) {
    let line = line.trim();
    if line.is_empty() {
      continue;
    }

    if line.starts_with('#') {
      // Debug label lines are prefixed with '#'.
      parse_label_line(line, &mut labels);
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

  (instructions, labels)
}

impl Emulator {
  pub fn new(path : String, use_uart_rx: bool) -> Emulator {
    let (instructions, _labels) = load_program(&path);
    Emulator::from_instructions(instructions, use_uart_rx)
  }

  pub fn from_instructions(instructions: HashMap<u32, u8>, use_uart_rx: bool) -> Emulator {
    let memory: Arc<Memory> = Arc::new(Memory::new(instructions, use_uart_rx));
    let interrupts = InterruptController::new(1);
    Emulator::from_shared(memory, interrupts, use_uart_rx, 0)
  }

  fn from_shared(
    memory: Arc<Memory>,
    interrupts: Arc<InterruptController>,
    use_uart_rx: bool,
    core_id: u32,
  ) -> Emulator {
    let mut cregfile = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    // CID is a read-only core identifier.
    cregfile[9] = core_id;
    if core_id != 0 {
      // Allow IPI wakeups on secondary cores by default.
      cregfile[3] = 0x80000020;
    }

    Emulator {
      kmode: true,
      regfile: [0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0],
      cregfile,
      memory,
      interrupts,
      tlb: RandomCache::new(32),
      pc: 0x400,
      asleep: core_id != 0,
      sleep_armed: false,
      halted: false,
      timer: 0,
      count: 0,
      core_id,
      use_uart_rx,
      watchpoints: Vec::new(),
      watchpoint_hit: None,
    }
  }

  fn read_isr(&self) -> u32 {
    self.cregfile[2]
  }

  fn write_isr(&mut self, value: u32) {
    let old = self.cregfile[2];
    self.cregfile[2] = value;
    // Let the interrupt controller know when input interrupts are cleared.
    let cleared = old & !value;
    if cleared != 0 {
      self.interrupts.ack_input(self.core_id as usize, cleared);
    }
  }

  fn set_isr_bits(&mut self, bits: u32) {
    self.cregfile[2] |= bits;
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
      10 => self.read_mbi(),
      _ => self.cregfile[idx],
    }
  }

  fn write_creg(&mut self, idx: usize, value: u32) {
    match idx {
      // Route ISR/MBI through helpers so we can track clears and core-local state.
      2 => self.write_isr(value),
      9 => {
        // CID is read-only.
        println!("Warning: attempt to write read-only CID register");
      }
      10 => self.write_mbi(value),
      _ => self.cregfile[idx] = value,
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
          self.watchpoint_hit = Some(WatchpointHit { addr, access, value });
          break;
        }
      }
    }
  }

  fn convert_mem_address(&self, addr : u32, operation : u32) -> Option<u32> {
    if self.kmode {
      if addr <= PHYSMEM_MAX {
        Some(addr)
      } else if let Some(result) = self.tlb.access(self.cregfile[1], addr >> 12, operation, self.kmode) {
        Some(result | (addr & 0xFFF))
      } else {
        // TLB_KMISS
        None
      }
    } else {
      if let Some(result) = self.tlb.access(self.cregfile[1], addr >> 12, operation, self.kmode) {
        Some(result | (addr & 0xFFF))
      } else {
        // TLB_UMISS
        None
      }
    }
  }

  fn save_state(&mut self){
    // save state as an interrupt happens

    // save pc
    self.cregfile[4] = self.pc;

    // disable interrupts
    self.cregfile[3] &= 0x7FFFFFFF;
  }

  fn raise_tlb_miss(&mut self, addr : u32) {
    // TLB_UMISS = 0x82
    // TLB_KMISS = 0x83

    // save address and pid that caused exception
    self.cregfile[7] = (addr >> 12) | (self.cregfile[1] << 20);

    self.save_state();

    if self.cregfile[0] == u32::MAX {
      panic!("too many nested exceptions!");
    }

    if self.kmode {
      self.kmode = true;
      self.cregfile[0] += 1;
      self.pc = self.mem_read32(0x83 * 4).expect("shouldnt fail");
    } else {
      self.kmode = true;
      self.cregfile[0] += 1;
      self.pc = self.mem_read32(0x82 * 4).expect("shouldnt fail");
    }
  }

  // memory operations must be aligned
  fn mem_write8(&mut self, addr : u32, data : u8) -> bool {
    let vaddr = addr;
    let addr = self.convert_mem_address(addr, 1);

    if let Some(addr) = addr {
      self.maybe_watch(vaddr, WatchAccess::Write, data);
      self.memory.write(addr, data);
      true
    } else {
      false
    }
  }

  fn mem_write16(&mut self, addr : u32, data : u16) -> bool {
    if (addr & 1) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    let addr = addr & 0xFFFFFFFE;
    let bytes = data.to_le_bytes();
    let mut addrs = [0u32; 2];
    for (i, slot) in addrs.iter_mut().enumerate() {
      if let Some(paddr) = self.convert_mem_address(addr + i as u32, 1) {
        *slot = paddr;
      } else {
        return false;
      }
    }
    self.maybe_watch(addr, WatchAccess::Write, bytes[0]);
    self.maybe_watch(addr + 1, WatchAccess::Write, bytes[1]);
    self.memory.write_phys_bytes(&addrs, &bytes);
    true
  }

  fn mem_write32(&mut self, addr : u32, data : u32) -> bool {
    if (addr & 3) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }

    let addr = addr & 0xFFFFFFFC;
    let bytes = data.to_le_bytes();
    let mut addrs = [0u32; 4];
    for (i, slot) in addrs.iter_mut().enumerate() {
      if let Some(paddr) = self.convert_mem_address(addr + i as u32, 1) {
        *slot = paddr;
      } else {
        return false;
      }
    }
    for (i, byte) in bytes.iter().enumerate() {
      self.maybe_watch(addr + i as u32, WatchAccess::Write, *byte);
    }
    self.memory.write_phys_bytes(&addrs, &bytes);
    true
  }

  fn mem_read8(&mut self, addr : u32) -> Option<u8> {
    if addr == 0 {
      println!("Warning: reading from virtual address 0x00000000");
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
    if (addr & 1) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    if addr == 0 {
      println!("Warning: reading from virtual address 0x00000000");
    }
    let mut addrs = [0u32; 2];
    for (i, slot) in addrs.iter_mut().enumerate() {
      if let Some(paddr) = self.convert_mem_address(addr + i as u32, 0) {
        *slot = paddr;
      } else {
        return None;
      }
    }
    let mut bytes = [0u8; 2];
    self.memory.read_phys_bytes(&addrs, &mut bytes);
    self.maybe_watch(addr, WatchAccess::Read, bytes[0]);
    self.maybe_watch(addr + 1, WatchAccess::Read, bytes[1]);
    Some(u16::from_le_bytes(bytes))
  }

  fn mem_read32(&mut self, addr: u32) -> Option<u32> {
    if (addr & 3) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    if addr == 0 {
      println!("Warning: reading from virtual address 0x00000000");
    }
    let mut addrs = [0u32; 4];
    for (i, slot) in addrs.iter_mut().enumerate() {
      if let Some(paddr) = self.convert_mem_address(addr + i as u32, 0) {
        *slot = paddr;
      } else {
        return None;
      }
    }
    let mut bytes = [0u8; 4];
    self.memory.read_phys_bytes(&addrs, &mut bytes);
    for (i, byte) in bytes.iter().enumerate() {
      self.maybe_watch(addr + i as u32, WatchAccess::Read, *byte);
    }
    Some(u32::from_le_bytes(bytes))
  }

  fn mem_atomic_swap32(&mut self, addr: u32, value: u32) -> Option<u32> {
    if (addr & 3) != 0 {
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    let addr = addr & 0xFFFFFFFC;
    let read_addr = self.convert_mem_address(addr, 0)?;
    let write_addr = self.convert_mem_address(addr, 1)?;
    if read_addr != write_addr {
      return None;
    }
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
    if (addr & 3) != 0 {
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    let addr = addr & 0xFFFFFFFC;
    let read_addr = self.convert_mem_address(addr, 0)?;
    let write_addr = self.convert_mem_address(addr, 1)?;
    if read_addr != write_addr {
      return None;
    }
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
    self.convert_mem_address(addr, 0).map(|paddr| self.memory.read(paddr))
  }

  fn fetch(&mut self, vaddr: u32) -> Option<u32> {
    if (vaddr & 3) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", vaddr);
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
      let instr = self.fetch(self.pc);

      if let Some(instr) = instr {
        self.execute(instr);
      } else {
        self.raise_tlb_miss(self.pc);
      }
    }
    self.count = self.count.wrapping_add(1);
  }

  pub fn run(mut self, max_iters : u32, with_graphics : bool) -> Option<u32> {
    let mut graphics: Option<Graphics> = None;
    if with_graphics {
      graphics = Some(Graphics::new(
        self.memory.get_frame_buffer(), 
        self.memory.get_tile_map(), 
        self.memory.get_io_buffer(),
        self.memory.get_vscroll_register(),
        self.memory.get_hscroll_register(),
        self.memory.get_sprite_map(),
        self.memory.get_scale_register(),
        self.memory.get_vga_mode_register(),
        self.memory.get_vga_status_register(),
        self.memory.get_vga_frame_register(),
        self.memory.get_pending_interrupt()
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

  pub fn run_multicore(
    path: String,
    cores: usize,
    sched: ScheduleMode,
    max_iters: u32,
    with_graphics: bool,
    use_uart_rx: bool,
  ) -> Option<u32> {
    assert!((1..=4).contains(&cores), "cores must be in 1..=4");
    let (instructions, _labels) = load_program(&path);
    let memory: Arc<Memory> = Arc::new(Memory::new(instructions, use_uart_rx));
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
        memory.get_frame_buffer(),
        memory.get_tile_map(),
        memory.get_io_buffer(),
        memory.get_vscroll_register(),
        memory.get_hscroll_register(),
        memory.get_sprite_map(),
        memory.get_scale_register(),
        memory.get_vga_mode_register(),
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
    results.get(0).copied().unwrap_or(None)
  }

  fn check_for_interrupts(&mut self) {

    // check if io buf is nonempty
    let io_nonempty = {
      let binding = self.memory.get_io_buffer();
      let io_buf = binding.read().unwrap();
      !io_buf.is_empty()
    };
    self.interrupts.dispatch_input(self.use_uart_rx, io_nonempty);

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

    // check for timer interrupt
    if self.timer == 0 {
      // check if timer was set
      let old_kmode = self.kmode;
      self.kmode = true;
      let v = self.memory.read_u32(PIT_START);
      self.kmode = old_kmode;
      if v != 0 {
        // reset timer
        self.timer = v;

        // trigger timer interrupt
        self.set_isr_bits(TIMER_INTERRUPT_BIT);
      }
    } else {
      self.timer -= 1;
    }
  }

  fn handle_interrupts(&mut self){
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

      if self.cregfile[0] == u32::MAX {
        panic!("too many nested exceptions!");
      }

      // enter kernel mode
      self.cregfile[0] += 1;
      self.kmode = true;

      // disable interrupts
      self.cregfile[3] &= 0x7FFFFFFF;

      if (active_ints >> 15) & 1 != 0 {
        self.pc = self.mem_read32(0xFF * 4).expect("this address shouldn't error");
      } else if (active_ints >> 14) & 1 != 0 {
        self.pc = self.mem_read32(0xFE * 4).expect("this address shouldn't error");
      } else if (active_ints >> 13) & 1 != 0 {
        self.pc = self.mem_read32(0xFD * 4).expect("this address shouldn't error");
      } else if (active_ints >> 12) & 1 != 0 {
        self.pc = self.mem_read32(0xFC * 4).expect("this address shouldn't error");
      } else if (active_ints >> 11) & 1 != 0 {
        self.pc = self.mem_read32(0xFB * 4).expect("this address shouldn't error");
      } else if (active_ints >> 10) & 1 != 0 {
        self.pc = self.mem_read32(0xFA * 4).expect("this address shouldn't error");
      } else if (active_ints >> 9) & 1 != 0 {
        self.pc = self.mem_read32(0xF9 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 8) & 1 != 0{
        self.pc = self.mem_read32(0xF8 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 7) & 1 != 0 {
        self.pc = self.mem_read32(0xF7 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 6) & 1 != 0 {
        self.pc = self.mem_read32(0xF6 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 5) & 1 != 0 {
        self.pc = self.mem_read32(0xF5 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 4) & 1 != 0 {
        self.pc = self.mem_read32(0xF4 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 3) & 1 != 0 {
        self.pc = self.mem_read32(0xF3 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 2) & 1 != 0 {
        self.pc = self.mem_read32(0xF2 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 1) & 1 != 0 {
        self.pc = self.mem_read32(0xF1 * 4).expect("this address shouldn't error");
      } else if active_ints & 1 != 0 {
        self.pc = self.mem_read32(0xF0 * 4).expect("this address shouldn't error");
      }
    }
  }

  fn raise_exc_instr(&mut self){
    // exec_instr

    self.save_state();

    if self.cregfile[0] == u32::MAX {
      panic!("too many nested exceptions!");
    }

    self.kmode = true;
    self.cregfile[0] += 1;

    self.pc = self.mem_read32(0x80 * 4).expect("shouldn't fail");
    return;
  }

  fn execute(&mut self, instr : u32) {
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

      15 => self.syscall(instr),

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

  fn get_reg(&self, regnum : u32) -> u32 {
    if self.kmode && regnum == 31 {
      // use kernel stack pointer
      self.cregfile[8]
    } else {
      // normal register access
      self.regfile[regnum as usize]
    }
  }

  fn write_reg(&mut self, regnum : u32, value : u32) {
    if self.kmode && regnum == 31 {
      // use kernel stack pointer
      self.cregfile[8] = value;
    } else {
      // normal register access
      if regnum != 0 {
        // r0 is always zero
        self.regfile[regnum as usize] = value;
      }
    }
  }

  fn decode_alu_imm(&mut self, op : u32, imm : u32) -> Option<u32> {
    match op {
      0..=6 => {
        // Bitwise op
        Some((imm & 0xFF) << (8 * ((imm >> 8) & 3)))
      },
      7..=13 => {
        // Shift op
        Some(imm & 0x1F)
      },
      14..=18 => {
        // Arithmetic op
        Some(imm | (0xFFFFF000 * ((imm >> 11) & 1))) // sign extend
      },
      _ => {
        self.raise_exc_instr();
        return None
      }
    }
  }

  // 2nd operand is either register or immediate
  fn alu_op(&mut self, instr : u32, imm : bool) {
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
      self.decode_alu_imm(op, instr & 0xFFF).expect("immediate decoding failed")
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
      }, 
      1 => {
        !(r_b & r_c)  // nand
      },
      2 => {
        r_b | r_c // or
      },
      3 => {
        !(r_b | r_c) // nor
      },
      4 => {
        r_b ^ r_c // xor
      },
      5 => {
        !(r_b ^ r_c) // xnor
      },
      6 => {
        !r_c // not
      },
      7 => {
        // set carry flag
        self.cregfile[5] |= (r_b >> if r_c > 0 {32 - r_c} else {0} != 0) as u32;
        r_b << r_c // lsl
      },
      8 => {
        // set carry flag
        self.cregfile[5] |= (r_b & ((1 << r_c) - 1) != 0) as u32;
        r_b >> r_c // lsr
      },
      9 => {
        // set carry flag
        let carry = r_b & 1;
        let sign = r_b >> 31;
        self.cregfile[5] |= carry;
        (r_b >> r_c) | (0xFFFFFFFF * sign << if r_c > 0 {32 - r_c} else {0}) // asr
      },
      10 => {
        // set carry flag
        let carry = r_b >> if r_c > 0 {32 - r_c} else {0};
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b << r_c) | carry // rotl
      },
      11 => {
        // set carry flag
        let carry = r_b & ((1 << r_c) - 1);
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b >> r_c) | (carry << if r_c > 0 {32 - r_c} else {0}) // rotr
      },
      12 => {
        // set carry flag
        let carry = if r_c > 0 {r_b >> (32 - r_c)} else {0};
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b << r_c) | if r_c > 0 {prev_carry << (r_c - 1)} else {0} // lslc
      },
      13 => {
        // set carry flag
        let carry = r_b & ((1 << r_c) - 1);
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b >> r_c) | (prev_carry << if r_c > 0 {32 - r_c} else {0}) // lsrc
      },
      14 => {
        // add
        let result = u64::from(r_b) + u64::from(r_c);

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      15 => {
        // addc
        let result = u64::from(r_c) + u64::from(r_b) + u64::from(prev_carry);

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      16 => {
        // sub

        // two's complement
        // sub with immediate does imm - reg
        let result = if imm {
          let r_b = (1 + u64::from(!r_b)) as u32;
          u64::from(r_c) + u64::from(r_b)
        } else {
          let r_c = (1 + u64::from(!r_c)) as u32;
          u64::from(r_c) + u64::from(r_b)
        };

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      17 => {
        // subb

        // two's complement
        let result = if imm {
          let r_b = (1 + u64::from(
          !(u32::wrapping_add(
          u32::from(prev_carry == 0), r_b)))) as u32;
          u64::from(imm) + u64::from(r_b)
        } else {
          let r_c = (1 + u64::from(
          !(u32::wrapping_add(u32::from(prev_carry == 0), r_c)))
          ) as u32;
          u64::from(r_c) + u64::from(r_b)
        };

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
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

  fn load_upper_immediate(&mut self, instr : u32){
    // store imm << 10 in r_a
    let r_a = (instr >> 22) & 0x1F;
    let imm = (instr & 0x03FFFFF) << 10;

    self.write_reg(r_a, imm);

    self.pc += 4;
  }

  fn mem_absolute(&mut self, instr : u32, size : u8){
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
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      let data = match size {
        0 => {
          // byte
          self.mem_read8(addr).map(|v| u32::from(v))
        },
        1 => {
          // halfword
          self.mem_read16(addr).map(|v| u32::from(v))
        },
        2 => {
          // word
          self.mem_read32(addr)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if let Some(data) = data {
        self.write_reg(r_a, data);
      } else{
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      };
    } else {
      // is a store
      let data = self.get_reg(r_a);
      let success = match size {
        0 => {
          // byte
          self.mem_write8(addr, data as u8)
        },
        1 => {
          // halfword
          self.mem_write16(addr, data as u16)
        },
        2 => {
          // word
          self.mem_write32(addr, data)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };
      if !success {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    if y == 1 || y == 2 {
      // pre or post increment
      self.write_reg(r_b, u32::wrapping_add(r_b_out, imm));
    }

    self.pc += 4;
  }

  fn mem_relative(&mut self, instr : u32, size : u8){
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
        },
        1 => {
          // halfword
          self.mem_read16(addr).map(|v| u32::from(v))
        },
        2 => {
          // word
          self.mem_read32(addr)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if let Some(data) = data {
        self.write_reg(r_a, data);
      } else{
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      };
    } else {
      // is a store
      let data = self.get_reg(r_a);

      let success = match size {
        0 => {
          // byte
          self.mem_write8(addr, data as u8)
        },
        1 => {
          // halfword
          self.mem_write16(addr, data as u16)
        },
        2 => {
          // word
          self.mem_write32(addr, data)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if !success {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_imm(&mut self, instr : u32, size : u8){
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
        },
        1 => {
          // halfword
          self.mem_read16(addr).map(|v| u32::from(v))
        },
        2 => {
          // word
          self.mem_read32(addr)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if let Some(data) = data {
        self.write_reg(r_a, data);
      } else{
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      };
    } else {
      // is a store
      let data = self.get_reg(r_a);

      let success = match size {
        0 => {
          // byte
          self.mem_write8(addr, data as u8)
        },
        1 => {
          // halfword
          self.mem_write16(addr, data as u16)
        },
        2 => {
          // word
          self.mem_write32(addr, data)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if !success {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn atomic_absolute(&mut self, instr : u32, type_ : u8){
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
      self.raise_tlb_miss(addr);
      return;
    }

    self.pc += 4;
  }

  fn atomic_relative(&mut self, instr : u32, type_ : u8){
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
      self.raise_tlb_miss(addr);
      return;
    }

    self.pc += 4;
  }

  fn atomic_imm(&mut self, instr : u32, type_ : u8){
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
      self.raise_tlb_miss(addr);
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
      0 => Some(true), // br
      1 => Some(zero), // bz
      2 => Some(!zero), // bnz
      3 => Some(sign), // bs
      4 => Some(!sign), // bns
      5 => Some(carry), // bc
      6 => Some(!carry), // bnc
      7 => Some(overflow), // bo
      8 => Some(!overflow), // bno
      9 => Some(!zero && !sign), // bps
      10 => Some(zero || sign), // bnps
      11 => Some(sign == overflow && !zero), // bg
      12 => Some(sign == overflow), // bge
      13 => Some(sign != overflow && !zero), // bl
      14 => Some(sign != overflow || zero), // ble
      15 => Some(!zero && carry), // ba
      16 => Some(carry || zero), // bae
      17 => Some(!carry && !zero), // bb
      18 => Some(!carry || zero), // bbe
      _ => {
        self.raise_exc_instr();
        return None;
      }
    }
  }

  fn branch_imm(&mut self, instr : u32){
    // instruction format is
    // 01100?????iiiiiiiiiiiiiiiiiiiiii
    // op (5 bits) | op (5 bits) | imm (22 bits)
    let op = (instr >> 22) & 0x1F;
    let imm = instr & 0x3FFFFF;

    // sign extend
    let imm = imm | (0xFFC00000 * ((imm >> 21) & 1));

    if let Some(branch) = self.get_branch_condition(op) {
      if branch {
        self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4 , imm));
      } else {
        self.pc += 4;
      }
    } else {
      return;
    }

  }

  fn branch_absolute(&mut self, instr : u32){
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

  fn branch_relative(&mut self, instr : u32){
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

  fn syscall(&mut self, instr : u32){
    let imm = instr & 0xFF;

    self.kmode = true;
    if self.cregfile[0] == u32::MAX {
      panic!("too many nested exceptions!");
    }
    self.cregfile[0] += 1;

    match imm {
      1 => {
        // sys EXIT

        // save pc and flags
        self.cregfile[4] = self.pc + 4;

        self.pc = self.mem_read32(0x01 * 4).expect("shouldnt fail");
      }
      _ => {
        self.raise_exc_instr();
        return;
      }
    }
  }

  // carry flag handled separately in each alu operation
  fn update_flags(&mut self, result : u32, lhs : u32, rhs : u32, op : u32) {
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


  fn kernel_instr(&mut self, instr : u32){
    if !self.kmode {
      // exec_priv
      assert!(self.cregfile[0] == 0);

      self.save_state();

      self.kmode = true;
      if self.cregfile[0] == u32::MAX {
        panic!("too many nested exceptions!");
      }
      self.cregfile[0] += 1;

      self.pc = self.mem_read32(0x81 * 4).expect("shouldn't fail");
      return;
    }

    assert!(self.cregfile[0] > 0);

    let op = (instr >> 12) & 0x1F;

    match op {
      0 => self.tlb_op(instr),
      1 => self.crmv_op(instr),
      2 => self.mode_op(instr),
      3 => self.rfe(instr),
      4 => self.ipi_op(instr),
      _ => {
        self.raise_exc_instr();
        return;
      }
    }
  }

  fn tlb_op(&mut self, instr : u32) {
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

  fn crmv_op(&mut self, instr : u32) {
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

  fn mode_op(&mut self, instr : u32) {
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

  fn rfe(&mut self, instr : u32) {
    // update kernel mode
    self.cregfile[0] -= 1;
    if self.cregfile[0] == 0 {
      self.kmode = false;
    }

    if ((instr >> 11) & 1) == 1 {
      // was rfi
      // re-enable interrupts
      self.cregfile[3] |= 0x80000000;
    }

    // restore pc
    self.pc = self.cregfile[4];
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
