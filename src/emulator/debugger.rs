// Debugger written by Codex

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use crate::disassembler::disassemble;
use crate::memory::PHYSMEM_MAX;

use super::{
  load_program,
  DebugInfo,
  DebugLine,
  DebugLocal,
  Emulator,
  LabelMap,
  WatchAccess,
  WatchKind,
  Watchpoint,
  WatchpointHit,
};

fn parse_addr(token: &str) -> Option<u32> {
  let s = token.trim();
  if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
    return u32::from_str_radix(hex, 16).ok();
  }
  if let Ok(v) = s.parse::<u32>() {
    return Some(v);
  }
  if s.chars().all(|c| c.is_ascii_hexdigit()) {
    return u32::from_str_radix(s, 16).ok();
  }
  None
}

fn resolve_source_path(file: &str) -> Result<PathBuf, String> {
  let path = Path::new(file);
  if path.is_absolute() {
    return Ok(path.to_path_buf());
  }
  let cwd = env::current_dir().map_err(|err| format!("Failed to resolve cwd: {}", err))?;
  Ok(cwd.join(file))
}

fn read_source_line(path: &Path, line: u32) -> Result<String, String> {
  if line == 0 {
    return Err("Line numbers start at 1".to_string());
  }
  let file = File::open(path)
    .map_err(|err| format!("Failed to open {}: {}", path.display(), err))?;
  let reader = io::BufReader::new(file);
  for (idx, line_result) in reader.lines().enumerate() {
    let idx = idx + 1;
    let text = line_result
      .map_err(|err| format!("Failed to read {}: {}", path.display(), err))?;
    if idx as u32 == line {
      return Ok(text);
    }
  }
  Err(format!("File {} has no line {}", path.display(), line))
}

fn build_labels_by_addr(labels: &LabelMap) -> HashMap<u32, Vec<String>> {
  let mut by_addr: HashMap<u32, Vec<String>> = HashMap::new();
  for (name, addrs) in labels {
    for addr in addrs {
      by_addr.entry(*addr).or_default().push(name.clone());
    }
  }
  by_addr
}

enum StepOutcome {
  Executed { pc: u32, instr: u32 },
  Sleeping,
  TlbMiss { pc: u32 },
}

enum RunOutcome {
  Breakpoint(u32),
  Halted,
  Watchpoint(WatchpointHit),
}

fn run_until_breakpoint(cpu: &mut Emulator, breakpoints: &HashSet<u32>) -> RunOutcome {
  loop {
    if cpu.halted {
      return RunOutcome::Halted;
    }
    if breakpoints.contains(&cpu.pc) {
      return RunOutcome::Breakpoint(cpu.pc);
    }
    match cpu.step_instruction() {
      StepOutcome::Executed { .. } => {}
      StepOutcome::Sleeping => {}
      StepOutcome::TlbMiss { .. } => {}
    }
    if let Some(hit) = cpu.take_watchpoint_hit() {
      return RunOutcome::Watchpoint(hit);
    }
  }
}

fn format_addr_list(addrs: &[u32]) -> String {
  let mut parts = Vec::new();
  for addr in addrs {
    parts.push(format!("{:08X}", addr));
  }
  parts.join(", ")
}

fn format_breakpoint(addr: u32, labels_by_addr: &HashMap<u32, Vec<String>>) -> String {
  if let Some(names) = labels_by_addr.get(&addr) {
    format!("{:08X} ({})", addr, names.join(", "))
  } else {
    format!("{:08X}", addr)
  }
}

fn list_breakpoints(breakpoints: &HashSet<u32>, labels_by_addr: &HashMap<u32, Vec<String>>) {
  if breakpoints.is_empty() {
    println!("No breakpoints set.");
    return;
  }
  let mut list: Vec<u32> = breakpoints.iter().copied().collect();
  list.sort_unstable();
  for addr in list {
    println!("{}", format_breakpoint(addr, labels_by_addr));
  }
}

fn watch_kind_label(kind: WatchKind) -> &'static str {
  match kind {
    WatchKind::Read => "r",
    WatchKind::Write => "w",
    WatchKind::ReadWrite => "rw",
  }
}

fn watch_access_label(access: WatchAccess) -> &'static str {
  match access {
    WatchAccess::Read => "read",
    WatchAccess::Write => "write",
  }
}

fn parse_watch_kind(token: &str) -> Option<WatchKind> {
  match token {
    "r" => Some(WatchKind::Read),
    "w" => Some(WatchKind::Write),
    "rw" | "wr" => Some(WatchKind::ReadWrite),
    _ => None,
  }
}

fn merge_watch_kind(existing: WatchKind, new_kind: WatchKind) -> WatchKind {
  if existing == new_kind {
    existing
  } else {
    WatchKind::ReadWrite
  }
}

fn add_watchpoint(list: &mut Vec<Watchpoint>, addr: u32, kind: WatchKind) -> WatchKind {
  for wp in list.iter_mut() {
    if wp.addr == addr {
      wp.kind = merge_watch_kind(wp.kind, kind);
      return wp.kind;
    }
  }
  list.push(Watchpoint { addr, kind });
  kind
}

fn remove_watchpoint(list: &mut Vec<Watchpoint>, addr: u32) -> bool {
  let before = list.len();
  list.retain(|wp| wp.addr != addr);
  before != list.len()
}

fn list_watchpoints(list: &[Watchpoint]) {
  if list.is_empty() {
    println!("No watchpoints set.");
    return;
  }
  let mut sorted = list.to_vec();
  sorted.sort_by_key(|wp| wp.addr);
  for wp in sorted {
    println!("{:08X} ({})", wp.addr, watch_kind_label(wp.kind));
  }
}

fn print_watchpoint_hit(hit: WatchpointHit, pc: u32) {
  println!(
    "Watchpoint hit ({} at {:08X} = {:02X}) pc {:08X}",
    watch_access_label(hit.access),
    hit.addr,
    hit.value,
    pc
  );
}

fn delete_breakpoint(target: &str, breakpoints: &mut HashSet<u32>, labels: &LabelMap) {
  match resolve_label_or_addr(target, labels) {
    Ok(addrs) => {
      if addrs.len() == 1 {
        let addr = addrs[0];
        if breakpoints.remove(&addr) {
          println!("Breakpoint removed at {:08X}", addr);
        } else {
          println!("No breakpoint set at {:08X}", addr);
        }
      } else {
        println!("Ambiguous label {} -> {}", target, format_addr_list(&addrs));
      }
    }
    Err(msg) => println!("{}", msg),
  }
}

fn dump_bytes<F>(base: u32, len: u32, mut read_byte: F)
where
  F: FnMut(u32) -> Option<u8>,
{
  if len == 0 {
    println!("(empty range)");
    return;
  }
  for offset in 0..len {
    if offset % 16 == 0 {
      print!("{:08X}: ", base.wrapping_add(offset));
    }
    match read_byte(base.wrapping_add(offset)) {
      Some(val) => print!("{:02X} ", val),
      None => print!("?? "),
    }
    if offset % 16 == 15 || offset + 1 == len {
      println!();
    }
  }
}

fn resolve_label_or_addr(target: &str, labels: &LabelMap) -> Result<Vec<u32>, String> {
  if let Some(addr) = parse_addr(target) {
    return Ok(vec![addr]);
  }
  if let Some(addrs) = labels.get(target) {
    return Ok(addrs.clone());
  }
  Err(format!("Unknown label {}", target))
}

fn print_step(pc: u32, instr: u32, labels_by_addr: &HashMap<u32, Vec<String>>) {
  let disasm = disassemble(instr);
  if let Some(names) = labels_by_addr.get(&pc) {
    println!("{:08X}: {:08X}  {} ({})", pc, instr, disasm, names.join(", "));
  } else {
    println!("{:08X}: {:08X}  {}", pc, instr, disasm);
  }
}

fn print_breakpoint(addr: u32, labels_by_addr: &HashMap<u32, Vec<String>>, cpu: &mut Emulator) {
  if let Some(instr) = cpu.fetch(addr) {
    print_step(addr, instr, labels_by_addr);
  } else {
    println!("Breakpoint hit at {:08X}", addr);
  }
}

// Avoid infinite loops when source lines do not advance.
const MAX_STEP_INSTRUCTIONS: u32 = 1_000_000;
// ABI base pointer register (r30).
const BP_REG: u32 = 30;
// Debug display uses word-sized (4-byte) reads.
const DEBUG_WORD_BYTES: u32 = 4;

fn build_line_index(lines: &[DebugLine]) -> HashMap<String, HashMap<u32, Vec<u32>>> {
  let mut index: HashMap<String, HashMap<u32, Vec<u32>>> = HashMap::new();
  for line in lines {
    index
      .entry(line.file.clone())
      .or_default()
      .entry(line.line)
      .or_default()
      .push(line.addr);
  }
  for file_map in index.values_mut() {
    for addrs in file_map.values_mut() {
      addrs.sort_unstable();
      addrs.dedup();
    }
  }
  index
}

// Requires lines sorted by addr ascending.
fn line_for_pc<'a>(lines: &'a [DebugLine], pc: u32) -> Option<&'a DebugLine> {
  if lines.is_empty() {
    return None;
  }
  let mut lo = 0;
  let mut hi = lines.len();
  while lo < hi {
    let mid = (lo + hi) / 2;
    if lines[mid].addr <= pc {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  if lo == 0 {
    None
  } else {
    Some(&lines[lo - 1])
  }
}

fn same_source_line(a: Option<&DebugLine>, b: Option<&DebugLine>) -> bool {
  match (a, b) {
    (Some(a), Some(b)) => a.line == b.line && a.file == b.file,
    (None, None) => true,
    _ => false,
  }
}

fn format_source_line(line: &DebugLine) -> String {
  format!("{}:{}", line.file, line.line)
}

fn print_c_location(pc: u32, line: Option<&DebugLine>) {
  if let Some(line) = line {
    match resolve_source_path(&line.file) {
      Ok(path) => match read_source_line(&path, line.line) {
        Ok(text) => println!("{:08X}: {}:{}: {}", pc, line.file, line.line, text),
        Err(err) => println!("{:08X}: {}:{}: <{}>", pc, line.file, line.line, err),
      },
      Err(err) => println!("{:08X}: {}:{}: <{}>", pc, line.file, line.line, err),
    }
  } else {
    println!("{:08X}: <no line info>", pc);
  }
}

fn format_breakpoint_c(addr: u32, lines: &[DebugLine]) -> String {
  if let Some(line) = line_for_pc(lines, addr) {
    format!("{:08X} ({})", addr, format_source_line(line))
  } else {
    format!("{:08X}", addr)
  }
}

fn list_breakpoints_c(breakpoints: &HashSet<u32>, lines: &[DebugLine]) {
  if breakpoints.is_empty() {
    println!("No breakpoints set.");
    return;
  }
  let mut list: Vec<u32> = breakpoints.iter().copied().collect();
  list.sort_unstable();
  for addr in list {
    println!("{}", format_breakpoint_c(addr, lines));
  }
}

fn build_locals_by_addr(debug: &DebugInfo) -> Vec<(u32, Vec<DebugLocal>)> {
  let mut locals: Vec<(u32, Vec<DebugLocal>)> = debug
    .locals_by_addr
    .iter()
    .map(|(addr, locals)| {
      let mut locals = locals.clone();
      locals.sort_by_key(|local| local.offset);
      (*addr, locals)
    })
    .collect();
  locals.sort_by_key(|(addr, _)| *addr);
  locals
}

// Heuristic: C function entry markers emit a duplicate .line at the function label.
// Use lines that appear multiple times and are anchored by a label without '.' in its name.
fn build_function_entries(
  line_index: &HashMap<String, HashMap<u32, Vec<u32>>>,
  labels_by_addr: &HashMap<u32, Vec<String>>,
) -> Vec<u32> {
  let mut entries = Vec::new();
  for file_map in line_index.values() {
    for addrs in file_map.values() {
      if addrs.len() < 2 {
        continue;
      }
      let entry_addr = addrs[0];
      let Some(labels) = labels_by_addr.get(&entry_addr) else {
        continue;
      };
      if labels.iter().any(|name| !name.contains('.')) {
        entries.push(entry_addr);
      }
    }
  }
  entries.sort_unstable();
  entries.dedup();
  entries
}

// Requires entries sorted by addr ascending.
fn function_entry_for_pc(entries: &[u32], pc: u32) -> Option<u32> {
  if entries.is_empty() {
    return None;
  }
  let mut lo = 0;
  let mut hi = entries.len();
  while lo < hi {
    let mid = (lo + hi) / 2;
    if entries[mid] <= pc {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  if lo == 0 {
    None
  } else {
    Some(entries[lo - 1])
  }
}

// Requires entries sorted by addr ascending.
fn function_range_for_pc(entries: &[u32], pc: u32) -> Option<(u32, Option<u32>)> {
  if entries.is_empty() {
    return None;
  }
  let mut lo = 0;
  let mut hi = entries.len();
  while lo < hi {
    let mid = (lo + hi) / 2;
    if entries[mid] <= pc {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  if lo == 0 {
    return None;
  }
  let start = entries[lo - 1];
  let end = if lo < entries.len() { Some(entries[lo]) } else { None };
  Some((start, end))
}

// Requires locals sorted by addr ascending.
fn first_locals_addr_in_range(
  locals: &[(u32, Vec<DebugLocal>)],
  start: u32,
  end: Option<u32>,
) -> Option<u32> {
  if locals.is_empty() {
    return None;
  }
  let mut lo = 0;
  let mut hi = locals.len();
  while lo < hi {
    let mid = (lo + hi) / 2;
    if locals[mid].0 < start {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  if lo >= locals.len() {
    return None;
  }
  let addr = locals[lo].0;
  if let Some(end) = end {
    if addr >= end {
      return None;
    }
  }
  Some(addr)
}

// Requires locals sorted by addr ascending.
fn locals_for_pc<'a>(
  locals: &'a [(u32, Vec<DebugLocal>)],
  func_entries: &[u32],
  pc: u32,
) -> Option<&'a Vec<DebugLocal>> {
  if locals.is_empty() {
    return None;
  }
  let mut lo = 0;
  let mut hi = locals.len();
  while lo < hi {
    let mid = (lo + hi) / 2;
    if locals[mid].0 <= pc {
      lo = mid + 1;
    } else {
      hi = mid;
    }
  }
  if lo == 0 {
    None
  } else {
    let entry_addr = locals[lo - 1].0;
    if let Some(func_start) = function_entry_for_pc(func_entries, pc) {
      if entry_addr < func_start {
        return None;
      }
    }
    Some(&locals[lo - 1].1)
  }
}

// Strip the compiler's numeric suffix (".N") from local names for display.
fn display_local_name(name: &str) -> &str {
  if let Some((base, suffix)) = name.rsplit_once('.') {
    if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) {
      return base;
    }
  }
  name
}

fn resolve_break_targets_c(
  token: &str,
  labels: &LabelMap,
  line_index: &HashMap<String, HashMap<u32, Vec<u32>>>,
  default_file: Option<&str>,
) -> Result<Vec<u32>, String> {
  if let Some(rest) = token.strip_prefix('*') {
    let addr = parse_addr(rest).ok_or_else(|| format!("Invalid address {}", rest))?;
    return Ok(vec![addr]);
  }
  if token.starts_with("0x") || token.starts_with("0X") {
    let addr = parse_addr(token).ok_or_else(|| format!("Invalid address {}", token))?;
    return Ok(vec![addr]);
  }
  if let Some((file, line_str)) = token.rsplit_once(':') {
    let line_num = line_str
      .parse::<u32>()
      .map_err(|_| format!("Invalid line number {}", line_str))?;
    let file_map = line_index
      .get(file)
      .ok_or_else(|| format!("Unknown source file {}", file))?;
    let addrs = file_map
      .get(&line_num)
      .ok_or_else(|| format!("No debug line {} in {}", line_num, file))?;
    return Ok(addrs.clone());
  }
  if token.chars().all(|c| c.is_ascii_digit()) {
    let line_num = token
      .parse::<u32>()
      .map_err(|_| format!("Invalid line number {}", token))?;
    let file = default_file.ok_or_else(|| {
      "No default source file; use break <file>:<line> instead".to_string()
    })?;
    let file_map = line_index
      .get(file)
      .ok_or_else(|| format!("Unknown source file {}", file))?;
    let addrs = file_map
      .get(&line_num)
      .ok_or_else(|| format!("No debug line {} in {}", line_num, file))?;
    return Ok(addrs.clone());
  }
  if let Some(addrs) = labels.get(token) {
    return Ok(addrs.clone());
  }
  Err(format!("Unknown label {}", token))
}

// Debug reads bypass watchpoints; returns None when the address is unmapped.
fn read_debug32_virt(cpu: &mut Emulator, addr: u32) -> Option<u32> {
  let mut value: u32 = 0;
  for i in 0..DEBUG_WORD_BYTES {
    let byte = cpu.read_virt8_debug(addr.wrapping_add(i))?;
    value |= u32::from(byte) << (8 * i);
  }
  Some(value)
}

fn read_debug_bytes_virt(cpu: &mut Emulator, addr: u32, size: u32) -> Option<Vec<u8>> {
  let mut bytes = Vec::with_capacity(size as usize);
  for i in 0..size {
    let byte = cpu.read_virt8_debug(addr.wrapping_add(i))?;
    bytes.push(byte);
  }
  Some(bytes)
}

// Format bytes as a hex value for small scalars; larger values stay byte-oriented.
fn format_bytes(bytes: &[u8]) -> String {
  if bytes.is_empty() {
    return "<empty>".to_string();
  }
  if bytes.len() <= 4 {
    let mut value: u32 = 0;
    for (idx, byte) in bytes.iter().enumerate() {
      value |= u32::from(*byte) << (8 * idx);
    }
    return format!("0x{:0width$X}", value, width = bytes.len() * 2);
  }
  let mut out = String::new();
  for (idx, byte) in bytes.iter().enumerate() {
    if idx != 0 {
      out.push(' ');
    }
    out.push_str(&format!("{:02X}", byte));
  }
  format!("[{}]", out)
}

impl Emulator {
  fn set_watchpoints(&mut self, watchpoints: &[Watchpoint]) {
    self.watchpoints.clear();
    self.watchpoints.extend_from_slice(watchpoints);
  }

  fn take_watchpoint_hit(&mut self) -> Option<WatchpointHit> {
    self.watchpoint_hit.take()
  }

  fn step_instruction(&mut self) -> StepOutcome {
    self.check_for_interrupts();
    self.handle_interrupts();

    if self.asleep {
      return StepOutcome::Sleeping;
    }

    let pc = self.pc;
    match self.fetch(pc) {
      Some(instr) => {
        self.execute(instr);
        self.count = self.count.wrapping_add(1);
        StepOutcome::Executed { pc, instr }
      }
      None => {
        self.raise_tlb_miss(pc);
        self.count = self.count.wrapping_add(1);
        StepOutcome::TlbMiss { pc }
      }
    }
  }

  fn print_regs(&self) {
    println!("pc: {:08X} kmode: {}", self.pc, self.kmode);
    for row in 0..8 {
      let base = row * 4;
      let r0 = self.get_reg(base as u32);
      let r1 = self.get_reg((base + 1) as u32);
      let r2 = self.get_reg((base + 2) as u32);
      let r3 = self.get_reg((base + 3) as u32);
      println!("r{:02}: {:08X} r{:02}: {:08X} r{:02}: {:08X} r{:02}: {:08X}",
        base, r0, base + 1, r1, base + 2, r2, base + 3, r3);
    }
    println!(
      "PSR: {:08X} PID: {:08X} ISR: {:08X} IMR: {:08X} EPC: {:08X}",
      self.read_creg(0), self.read_creg(1), self.read_creg(2), self.read_creg(3), self.read_creg(4)
    );
    println!(
      "FLG: {:08X} CDV: {:08X} TLB: {:08X} KSP: {:08X}",
      self.read_creg(5), self.read_creg(6), self.read_creg(7), self.read_creg(8)
    );
  }

  fn print_cregs(&self) {
    println!("kmode: {}", self.kmode);
    println!("cr0 (psr): {:08X}", self.read_creg(0));
    println!("cr1 (pid): {:08X}", self.read_creg(1));
    println!("cr2 (isr): {:08X}", self.read_creg(2));
    println!("cr3 (imr): {:08X}", self.read_creg(3));
    println!("cr4 (epc): {:08X}", self.read_creg(4));
    println!("cr5 (flg): {:08X}", self.read_creg(5));
    println!("cr6 (cdv): {:08X}", self.read_creg(6));
    println!("cr7 (tlb): {:08X}", self.read_creg(7));
    println!("cr8 (ksp): {:08X}", self.read_creg(8));
    println!("cr9 (cid): {:08X}", self.read_creg(9));
    println!("cr10 (mbi): {:08X}", self.read_creg(10));
    println!("cr11 (mbo): {:08X}", self.read_creg(11));
  }

  fn print_single_reg(&self, token: &str) -> bool {
    let token = token.to_ascii_lowercase();
    match token.as_str() {
      "pc" => {
        println!("pc = {:08X}", self.pc);
        return true;
      }
      "sp" => {
        println!("sp (r31) = {:08X}", self.get_reg(31));
        return true;
      }
      "bp" => {
        println!("bp (r30) = {:08X}", self.get_reg(30));
        return true;
      }
      "ra" => {
        println!("ra (r29) = {:08X}", self.get_reg(29));
        return true;
      }
      "ksp" => {
        println!("ksp (cr8) = {:08X}", self.read_creg(8));
        return true;
      }
      "psr" => {
        println!("psr (cr0) = {:08X}", self.read_creg(0));
        return true;
      }
      "pid" => {
        println!("pid (cr1) = {:08X}", self.read_creg(1));
        return true;
      }
      "isr" => {
        println!("isr (cr2) = {:08X}", self.read_creg(2));
        return true;
      }
      "imr" => {
        println!("imr (cr3) = {:08X}", self.read_creg(3));
        return true;
      }
      "epc" => {
        println!("epc (cr4) = {:08X}", self.read_creg(4));
        return true;
      }
      "flg" => {
        println!("flg (cr5) = {:08X}", self.read_creg(5));
        return true;
      }
      "cdv" => {
        println!("cdv (cr6) = {:08X}", self.read_creg(6));
        return true;
      }
      "tlb" => {
        println!("tlb (cr7) = {:08X}", self.read_creg(7));
        return true;
      }
      "cid" => {
        println!("cid (cr9) = {:08X}", self.read_creg(9));
        return true;
      }
      "mbi" => {
        println!("mbi (cr10) = {:08X}", self.read_creg(10));
        return true;
      }
      "mbo" => {
        println!("mbo (cr11) = {:08X}", self.read_creg(11));
        return true;
      }
      _ => {}
    }

    if let Some(num) = token.strip_prefix("r") {
      if let Ok(idx) = num.parse::<u32>() {
        if idx < 32 {
          println!("r{} = {:08X}", idx, self.get_reg(idx));
          return true;
        }
      }
    }

    if let Some(num) = token.strip_prefix("cr") {
      if let Ok(idx) = num.parse::<usize>() {
        if idx < self.cregfile.len() {
          println!("cr{} = {:08X}", idx, self.read_creg(idx));
          return true;
        }
      }
    }

    false
  }

  fn set_reg_value(&mut self, token: &str, value: u32) -> bool {
    let token = token.to_ascii_lowercase();
    match token.as_str() {
      "pc" => {
        self.pc = value;
        return true;
      }
      "sp" => {
        self.write_reg(31, value);
        return true;
      }
      "bp" => {
        self.write_reg(30, value);
        return true;
      }
      "ra" => {
        self.write_reg(29, value);
        return true;
      }
      "psr" => {
        self.write_creg(0, value);
        return true;
      }
      "pid" => {
        self.write_creg(1, value);
        return true;
      }
      "isr" => {
        self.write_creg(2, value);
        return true;
      }
      "imr" => {
        self.write_creg(3, value);
        return true;
      }
      "epc" => {
        self.write_creg(4, value);
        return true;
      }
      "flg" => {
        self.write_creg(5, value);
        return true;
      }
      "cdv" => {
        self.write_creg(6, value);
        return true;
      }
      "tlb" => {
        self.write_creg(7, value);
        return true;
      }
      "ksp" => {
        self.write_creg(8, value);
        return true;
      }
      "cid" => {
        self.write_creg(9, value);
        return true;
      }
      "mbi" => {
        self.write_creg(10, value);
        return true;
      }
      "mbo" => {
        self.write_creg(11, value);
        return true;
      }
      _ => {}
    }

    if let Some(num) = token.strip_prefix("r") {
      if let Ok(idx) = num.parse::<u32>() {
        if idx < 32 {
          self.write_reg(idx, value);
          return true;
        }
      }
    }

    if let Some(num) = token.strip_prefix("cr") {
      if let Ok(idx) = num.parse::<usize>() {
        if idx < self.cregfile.len() {
          self.write_creg(idx, value);
          return true;
        }
      }
    }

    false
  }

  fn print_tlb(&self) {
    self.tlb.debug_dump();
  }

  fn print_phys(&mut self, addr: u32) {
    if addr > PHYSMEM_MAX {
      println!("Warning: physical address out of range 0x{:08X}", addr);
      return;
    }
    match self.read_phys32(addr) {
      Some(word) => println!("paddr {:08X} = {:08X}", addr, word),
      None => println!("Warning: physical address out of range 0x{:08X}", addr),
    }
  }

  fn print_virt(&mut self, addr: u32) {
    match self.convert_mem_address(addr, 0) {
      Some(paddr) => {
        match self.read_phys32(paddr) {
          Some(word) => println!("vaddr {:08X} -> paddr {:08X} = {:08X}", addr, paddr, word),
          None => println!("Warning: physical address out of range 0x{:08X}", paddr),
        }
      }
      None => println!("Warning: no TLB mapping for vaddr 0x{:08X}", addr),
    }
  }

  pub fn debug(
    path: String,
    use_uart_rx: bool,
    sd_dma_ticks_per_word: u32,
    sd0_image: Option<&[u8]>,
    sd1_image: Option<&[u8]>,
  ) {
    let image = load_program(&path);
    let labels_by_addr = build_labels_by_addr(&image.labels);
    let mut breakpoints: HashSet<u32> = HashSet::new();
    let mut watchpoints: Vec<Watchpoint> = Vec::new();
    let mut cpu = Emulator::from_instructions(
      image.instructions.clone(),
      use_uart_rx,
      sd_dma_ticks_per_word,
      sd0_image,
      sd1_image,
    );
    cpu.set_watchpoints(&watchpoints);

    println!("Debug mode:");
    println!("  r                 reset and run until break/watchpoint/halt");
    println!("  c                 continue execution");
    println!("  n                 step one instruction");
    println!("  break <label|addr> set breakpoint");
    println!("  breaks            list breakpoints");
    println!("  delete <label|addr> remove breakpoint");
    println!("  watch [r|w|rw] <addr> stop on memory access");
    println!("  watchs            list watchpoints");
    println!("  unwatch <addr>    remove watchpoint");
    println!("  info regs         print all registers");
    println!("  info cregs        print control registers + kmode");
    println!("  info <reg>        print a single register");
    println!("  info tlb          dump TLB maps");
    println!("  info p <addr>     print word at physical address");
    println!("  info v <addr>     print word + resolved physical address");
    println!("  x [v|p] <addr> <len> dump memory range");
    println!("  set reg <reg> <value> write a register");
    println!("  q                 quit");

    loop {
      print!("dbg> ");
      io::stdout().flush().unwrap();

      let mut line = String::new();
      if io::stdin().read_line(&mut line).is_err() {
        break;
      }
      let line = line.trim();
      if line.is_empty() {
        continue;
      }

      let mut parts = line.split_whitespace();
      let cmd = parts.next().unwrap();

      match cmd {
        "q" | "quit" => break,
        "h" | "help" => {
          println!("Commands:");
          println!("  r                 reset and run until break/watchpoint/halt");
          println!("  c                 continue execution");
          println!("  n                 step one instruction");
          println!("  break <label|addr> set breakpoint");
          println!("  breaks            list breakpoints");
          println!("  delete <label|addr> remove breakpoint");
          println!("  watch [r|w|rw] <addr> stop on memory access");
          println!("  watchs            list watchpoints");
          println!("  unwatch <addr>    remove watchpoint");
          println!("  info regs         print all registers");
          println!("  info cregs        print control registers + kmode");
          println!("  info <reg>        print a single register");
          println!("  info tlb          dump TLB maps");
          println!("  info p <addr>     print word at physical address");
          println!("  info v <addr>     print word + resolved physical address");
          println!("  x [v|p] <addr> <len> dump memory range");
          println!("  set reg <reg> <value> write a register");
          println!("  q                 quit");
        }
        "r" => {
          cpu = Emulator::from_instructions(
            image.instructions.clone(),
            use_uart_rx,
            sd_dma_ticks_per_word,
            sd0_image,
            sd1_image,
          );
          cpu.set_watchpoints(&watchpoints);
          match run_until_breakpoint(&mut cpu, &breakpoints) {
            RunOutcome::Breakpoint(addr) => {
              print_breakpoint(addr, &labels_by_addr, &mut cpu);
            }
            RunOutcome::Halted => {
              println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
            }
            RunOutcome::Watchpoint(hit) => {
              print_watchpoint_hit(hit, cpu.pc);
            }
          }
        }
        "c" => {
          match run_until_breakpoint(&mut cpu, &breakpoints) {
            RunOutcome::Breakpoint(addr) => {
              print_breakpoint(addr, &labels_by_addr, &mut cpu);
            }
            RunOutcome::Halted => {
              println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
            }
            RunOutcome::Watchpoint(hit) => {
              print_watchpoint_hit(hit, cpu.pc);
            }
          }
        }
        "n" => {
          if cpu.halted {
            println!("Program already halted.");
            continue;
          }

          match cpu.step_instruction() {
            StepOutcome::Executed { pc, instr } => {
              print_step(pc, instr, &labels_by_addr);
              if let Some(hit) = cpu.take_watchpoint_hit() {
                print_watchpoint_hit(hit, cpu.pc);
              }
              if cpu.halted {
                println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
              }
            }
            StepOutcome::Sleeping => {
              println!("CPU sleeping; waiting for interrupt.");
            }
            StepOutcome::TlbMiss { pc } => {
              println!("TLB miss at {:08X}", pc);
            }
          }
        }
        "break" | "b" => {
          let target = parts.next();
          if target.is_none() {
            println!("Usage: break <label|addr>");
            continue;
          }
          let target = target.unwrap();
          match resolve_label_or_addr(target, &image.labels) {
            Ok(addrs) => {
              if addrs.len() == 1 {
                let addr = addrs[0];
                breakpoints.insert(addr);
                println!("Breakpoint set at {:08X}", addr);
              } else {
                println!("Ambiguous label {} -> {}", target, format_addr_list(&addrs));
              }
            }
            Err(msg) => println!("{}", msg),
          }
        }
        "breaks" => {
          list_breakpoints(&breakpoints, &labels_by_addr);
        }
        "delete" | "del" => {
          let target = parts.next();
          if target.is_none() {
            println!("Usage: delete <label|addr>");
            continue;
          }
          delete_breakpoint(target.unwrap(), &mut breakpoints, &image.labels);
        }
        "watch" => {
          let mut kind = WatchKind::ReadWrite;
          let mut addr_token = parts.next();
          if let Some(token) = addr_token {
            if let Some(parsed) = parse_watch_kind(token) {
              kind = parsed;
              addr_token = parts.next();
            }
          }
          let Some(addr_str) = addr_token else {
            println!("Usage: watch [r|w|rw] <addr>");
            continue;
          };
          let Some(addr) = parse_addr(addr_str) else {
            println!("Invalid address {}", addr_str);
            continue;
          };
          let final_kind = add_watchpoint(&mut watchpoints, addr, kind);
          cpu.set_watchpoints(&watchpoints);
          println!("Watchpoint set at {:08X} ({})", addr, watch_kind_label(final_kind));
        }
        "watchs" | "watchpoints" => {
          list_watchpoints(&watchpoints);
        }
        "unwatch" => {
          let Some(addr_str) = parts.next() else {
            println!("Usage: unwatch <addr>");
            continue;
          };
          let Some(addr) = parse_addr(addr_str) else {
            println!("Invalid address {}", addr_str);
            continue;
          };
          if remove_watchpoint(&mut watchpoints, addr) {
            cpu.set_watchpoints(&watchpoints);
            println!("Watchpoint removed at {:08X}", addr);
          } else {
            println!("No watchpoint set at {:08X}", addr);
          }
        }
        "x" => {
          let mut mode = "v";
          let mut addr_token = parts.next();
          if let Some(token) = addr_token {
            if token == "v" || token == "p" {
              mode = token;
              addr_token = parts.next();
            }
          }
          let Some(addr_str) = addr_token else {
            println!("Usage: x [v|p] <addr> <len>");
            continue;
          };
          let Some(len_str) = parts.next() else {
            println!("Usage: x [v|p] <addr> <len>");
            continue;
          };
          let Some(addr) = parse_addr(addr_str) else {
            println!("Invalid address {}", addr_str);
            continue;
          };
          let Some(len) = parse_addr(len_str) else {
            println!("Invalid length {}", len_str);
            continue;
          };
          if mode == "p" {
            dump_bytes(addr, len, |a| cpu.read_phys8_debug(a));
          } else {
            dump_bytes(addr, len, |a| cpu.read_virt8_debug(a));
          }
        }
        "set" => {
          let sub = parts.next();
          if sub != Some("reg") {
            println!("Usage: set reg <reg> <value>");
            continue;
          }
          let Some(reg_name) = parts.next() else {
            println!("Usage: set reg <reg> <value>");
            continue;
          };
          let Some(value_str) = parts.next() else {
            println!("Usage: set reg <reg> <value>");
            continue;
          };
          let Some(value) = parse_addr(value_str) else {
            println!("Invalid value {}", value_str);
            continue;
          };
          if !cpu.set_reg_value(reg_name, value) {
            println!("Unknown register {}", reg_name);
          }
        }
        "info" => {
          match parts.next() {
            Some("regs") => cpu.print_regs(),
            Some("cregs") => cpu.print_cregs(),
            Some("tlb") => cpu.print_tlb(),
            Some("p") => {
              if let Some(arg) = parts.next() {
                if let Some(addr) = parse_addr(arg) {
                  cpu.print_phys(addr);
                } else {
                  println!("Invalid address {}", arg);
                }
              } else {
                println!("Usage: info p <addr>");
              }
            }
            Some("v") => {
              if let Some(arg) = parts.next() {
                if let Some(addr) = parse_addr(arg) {
                  cpu.print_virt(addr);
                } else {
                  println!("Invalid address {}", arg);
                }
              } else {
                println!("Usage: info v <addr>");
              }
            }
            Some(token) => {
              if !cpu.print_single_reg(token) {
                println!("Unknown info target {}", token);
              }
            }
            None => println!("Usage: info <regs|cregs|tlb|p|v|reg>"),
          }
        }
        _ => println!("Unknown command: {}", cmd),
      }
    }
  }

  pub fn debug_c(
    path: String,
    use_uart_rx: bool,
    sd_dma_ticks_per_word: u32,
    sd0_image: Option<&[u8]>,
    sd1_image: Option<&[u8]>,
  ) {
    let image = load_program(&path);
    let mut lines = image.debug.lines.clone();
    lines.sort_by_key(|line| line.addr);
    let line_index = build_line_index(&lines);
    let labels_by_addr = build_labels_by_addr(&image.labels);
    let function_entries = build_function_entries(&line_index, &labels_by_addr);
    let locals_by_addr = build_locals_by_addr(&image.debug);
    let mut globals = image.debug.globals.clone();
    globals.sort_by(|a, b| a.name.cmp(&b.name).then(a.addr.cmp(&b.addr)));
    globals.dedup_by(|a, b| a.name == b.name && a.addr == b.addr);

    if lines.is_empty() {
      println!("Warning: no C debug line info found; break/next/step will be limited.");
    }
    if image.debug.missing_line_addrs {
      println!("Warning: some #line entries lack addresses; rebuild with the updated assembler.");
    }
    if locals_by_addr.is_empty() {
      println!("Warning: no C local debug info found; info locals will be empty.");
    }
    if image.debug.missing_local_addrs {
      println!("Warning: some #local entries lack addresses; rebuild with the updated assembler.");
    }
    if image.debug.missing_local_sizes {
      println!("Warning: some #local entries lack sizes; defaulting to 4-byte reads.");
    }

    let mut breakpoints: HashSet<u32> = HashSet::new();
    let mut cpu = Emulator::from_instructions(
      image.instructions.clone(),
      use_uart_rx,
      sd_dma_ticks_per_word,
      sd0_image,
      sd1_image,
    );

    println!("C debug mode:");
    println!("  r                   reset and run until break/halt");
    println!("  c                   continue execution");
    println!("  step                step to the next source line");
    println!("  next                step over calls to the next source line");
    println!("  break <line>         set breakpoint on current file line");
    println!("  break <file>:<line>  set breakpoint on file line");
    println!("  break <label>        set breakpoint on label");
    println!("  break *<addr>        set breakpoint on address");
    println!("  breaks              list breakpoints");
    println!("  delete <target>     remove breakpoint");
    println!("  info locals         print locals for current frame");
    println!("  info globals        print global data symbols");
    println!("  q                   quit");

    loop {
      print!("dbg> ");
      io::stdout().flush().unwrap();

      let mut line = String::new();
      if io::stdin().read_line(&mut line).is_err() {
        break;
      }
      let line = line.trim();
      if line.is_empty() {
        continue;
      }

      let mut parts = line.split_whitespace();
      let cmd = parts.next().unwrap();

      match cmd {
        "q" | "quit" => break,
        "h" | "help" => {
          println!("Commands:");
          println!("  r                   reset and run until break/halt");
          println!("  c                   continue execution");
          println!("  step                step to the next source line");
          println!("  next                step over calls to the next source line");
          println!("  break <line>         set breakpoint on current file line");
          println!("  break <file>:<line>  set breakpoint on file line");
          println!("  break <label>        set breakpoint on label");
          println!("  break *<addr>        set breakpoint on address");
          println!("  breaks              list breakpoints");
          println!("  delete <target>     remove breakpoint");
          println!("  info locals         print locals for current frame");
          println!("  info globals        print global data symbols");
          println!("  q                   quit");
        }
        "r" => {
          cpu = Emulator::from_instructions(
            image.instructions.clone(),
            use_uart_rx,
            sd_dma_ticks_per_word,
            sd0_image,
            sd1_image,
          );
          match run_until_breakpoint(&mut cpu, &breakpoints) {
            RunOutcome::Breakpoint(addr) => {
              print_c_location(addr, line_for_pc(&lines, addr));
            }
            RunOutcome::Halted => {
              println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
            }
            RunOutcome::Watchpoint(_) => {
              println!("Watchpoints are not supported in C debug mode.");
            }
          }
        }
        "c" => {
          match run_until_breakpoint(&mut cpu, &breakpoints) {
            RunOutcome::Breakpoint(addr) => {
              print_c_location(addr, line_for_pc(&lines, addr));
            }
            RunOutcome::Halted => {
              println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
            }
            RunOutcome::Watchpoint(_) => {
              println!("Watchpoints are not supported in C debug mode.");
            }
          }
        }
        "step" | "s" => {
          if cpu.halted {
            println!("Program already halted.");
            continue;
          }
          let start_line = line_for_pc(&lines, cpu.pc);
          let mut steps = 0;
          loop {
            if steps >= MAX_STEP_INSTRUCTIONS {
              println!("Warning: step limit reached without leaving the current line.");
              break;
            }
            match cpu.step_instruction() {
              StepOutcome::Executed { .. } => {}
              StepOutcome::Sleeping => {
                println!("CPU sleeping; waiting for interrupt.");
                break;
              }
              StepOutcome::TlbMiss { pc } => {
                println!("TLB miss at {:08X}", pc);
                break;
              }
            }
            steps += 1;
            if breakpoints.contains(&cpu.pc) {
              break;
            }
            let next_line = line_for_pc(&lines, cpu.pc);
            if start_line.is_none() || !same_source_line(start_line, next_line) {
              break;
            }
            if cpu.halted {
              break;
            }
          }
          if cpu.halted {
            println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
          } else {
            print_c_location(cpu.pc, line_for_pc(&lines, cpu.pc));
          }
        }
        "next" | "n" => {
          if cpu.halted {
            println!("Program already halted.");
            continue;
          }
          let start_line = line_for_pc(&lines, cpu.pc);
          let start_bp = cpu.get_reg(BP_REG);
          let mut steps = 0;
          loop {
            if steps >= MAX_STEP_INSTRUCTIONS {
              println!("Warning: step limit reached without leaving the current line.");
              break;
            }
            match cpu.step_instruction() {
              StepOutcome::Executed { .. } => {}
              StepOutcome::Sleeping => {
                println!("CPU sleeping; waiting for interrupt.");
                break;
              }
              StepOutcome::TlbMiss { pc } => {
                println!("TLB miss at {:08X}", pc);
                break;
              }
            }
            steps += 1;
            if breakpoints.contains(&cpu.pc) {
              break;
            }
            if cpu.get_reg(BP_REG) != start_bp {
              continue;
            }
            let next_line = line_for_pc(&lines, cpu.pc);
            if start_line.is_none() || !same_source_line(start_line, next_line) {
              break;
            }
            if cpu.halted {
              break;
            }
          }
          if cpu.halted {
            println!("Program halted. r1 = {:08X}", cpu.regfile[1]);
          } else {
            print_c_location(cpu.pc, line_for_pc(&lines, cpu.pc));
          }
        }
        "break" | "b" => {
          let Some(target) = parts.next() else {
            println!("Usage: break <line|file:line|label|*addr>");
            continue;
          };
          let current_line = line_for_pc(&lines, cpu.pc);
          let default_file = current_line
            .map(|line| line.file.as_str())
            .or_else(|| {
              if line_index.len() == 1 {
                line_index.keys().next().map(|name| name.as_str())
              } else {
                None
              }
            });
          match resolve_break_targets_c(target, &image.labels, &line_index, default_file) {
            Ok(addrs) => {
              let mut added = 0;
              for addr in addrs {
                if breakpoints.insert(addr) {
                  added += 1;
                }
              }
              if added == 0 {
                println!("No new breakpoints set.");
              } else {
                println!("Breakpoints set: {}", added);
              }
            }
            Err(msg) => println!("{}", msg),
          }
        }
        "breaks" => {
          list_breakpoints_c(&breakpoints, &lines);
        }
        "delete" | "del" => {
          let Some(target) = parts.next() else {
            println!("Usage: delete <line|file:line|label|*addr>");
            continue;
          };
          let current_line = line_for_pc(&lines, cpu.pc);
          let default_file = current_line
            .map(|line| line.file.as_str())
            .or_else(|| {
              if line_index.len() == 1 {
                line_index.keys().next().map(|name| name.as_str())
              } else {
                None
              }
            });
          match resolve_break_targets_c(target, &image.labels, &line_index, default_file) {
            Ok(addrs) => {
              let mut removed = 0;
              for addr in addrs {
                if breakpoints.remove(&addr) {
                  removed += 1;
                }
              }
              if removed == 0 {
                println!("No matching breakpoints.");
              } else {
                println!("Breakpoints removed: {}", removed);
              }
            }
            Err(msg) => println!("{}", msg),
          }
        }
        "info" => {
          match parts.next() {
            Some("locals") => {
              let Some(locals) = locals_for_pc(&locals_by_addr, &function_entries, cpu.pc) else {
                if let Some((start, end)) = function_range_for_pc(&function_entries, cpu.pc) {
                  if let Some(first_addr) = first_locals_addr_in_range(&locals_by_addr, start, end)
                  {
                    if cpu.pc < first_addr {
                      println!(
                        "Locals are not available yet; enter the function body (after prologue at {:08X}).",
                        first_addr
                      );
                      continue;
                    }
                  }
                }
                println!("No local variables found for current location.");
                continue;
              };
              let bp = cpu.get_reg(BP_REG) as i64;
              for local in locals {
                let addr = bp + local.offset as i64;
                if addr < 0 || addr > u32::MAX as i64 {
                  println!(
                    "{} @ <invalid> (offset {:+}, size {})",
                    display_local_name(&local.name),
                    local.offset,
                    local.size
                  );
                  continue;
                }
                let addr = addr as u32;
                match read_debug_bytes_virt(&mut cpu, addr, local.size) {
                  Some(bytes) => {
                    let value = format_bytes(&bytes);
                    println!(
                      "{} @ {:08X} (offset {:+}, size {}) = {}",
                      display_local_name(&local.name),
                      addr,
                      local.offset,
                      local.size,
                      value
                    );
                  }
                  None => println!(
                    "{} @ {:08X} (offset {:+}, size {}) = <unmapped>",
                    display_local_name(&local.name),
                    addr,
                    local.offset,
                    local.size
                  ),
                }
              }
            }
            Some("globals") => {
              if globals.is_empty() {
                println!("No global debug symbols found.");
                continue;
              }
              for global in &globals {
                match read_debug32_virt(&mut cpu, global.addr) {
                  Some(value) => println!(
                    "{} @ {:08X} = {:08X}",
                    global.name, global.addr, value
                  ),
                  None => println!("{} @ {:08X} = <unmapped>", global.name, global.addr),
                }
              }
            }
            _ => println!("Usage: info <locals|globals>"),
          }
        }
        _ => println!("Unknown command: {}", cmd),
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parse_addr_accepts_hex_and_dec() {
    assert_eq!(parse_addr("0x10"), Some(0x10));
    assert_eq!(parse_addr("0X20"), Some(0x20));
    assert_eq!(parse_addr("10"), Some(10));
    assert_eq!(parse_addr("FF"), Some(0xFF));
    assert_eq!(parse_addr("not-a-number"), None);
  }

  #[test]
  fn watchpoint_merge_upgrades_kind() {
    let mut list = Vec::new();
    add_watchpoint(&mut list, 0x10, WatchKind::Read);
    let merged = add_watchpoint(&mut list, 0x10, WatchKind::Write);
    assert_eq!(merged, WatchKind::ReadWrite);
    assert_eq!(list.len(), 1);
  }

  #[test]
  fn parse_watch_kind_variants() {
    assert_eq!(parse_watch_kind("r"), Some(WatchKind::Read));
    assert_eq!(parse_watch_kind("w"), Some(WatchKind::Write));
    assert_eq!(parse_watch_kind("rw"), Some(WatchKind::ReadWrite));
    assert_eq!(parse_watch_kind("wr"), Some(WatchKind::ReadWrite));
    assert_eq!(parse_watch_kind("x"), None);
  }
}
