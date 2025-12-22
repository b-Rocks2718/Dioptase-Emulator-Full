use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use crate::disassembler::disassemble;
use crate::memory::PHYSMEM_MAX;

use super::{load_program, Emulator, LabelMap, WatchAccess, WatchKind, Watchpoint, WatchpointHit};

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

  pub fn debug(path: String, use_uart_rx: bool) {
    let (program, labels) = load_program(&path);
    let labels_by_addr = build_labels_by_addr(&labels);
    let mut breakpoints: HashSet<u32> = HashSet::new();
    let mut watchpoints: Vec<Watchpoint> = Vec::new();
    let mut cpu = Emulator::from_instructions(program.clone(), use_uart_rx);
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
          cpu = Emulator::from_instructions(program.clone(), use_uart_rx);
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
          match resolve_label_or_addr(target, &labels) {
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
          delete_breakpoint(target.unwrap(), &mut breakpoints, &labels);
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
