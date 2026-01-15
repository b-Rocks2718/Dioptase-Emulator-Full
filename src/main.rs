use std::env;
use std::process;

pub mod emulator;
pub mod tests;
pub mod graphics;
pub mod memory;
pub mod disassembler;

use emulator::{Emulator, ScheduleMode, set_trace_interrupts};

fn main() {
  let args = env::args().collect::<Vec<_>>();

  let mut with_graphics = false;
  let mut use_uart_rx = false;
  let mut debug = false;
  let mut debugc = false;
  let mut trace_interrupts = false;
  let mut cores: usize = 1;
  let mut sched = ScheduleMode::Free;
  let mut max_cycles: u32 = 0;
  let mut path: Option<String> = None;

  let mut iter = args.iter().skip(1).peekable();
  while let Some(arg) = iter.next() {
    match arg.as_str() {
      "--vga" => with_graphics = true,
      "--uart" => use_uart_rx = true,
      "--debug" => debug = true,
      "--debugc" => debugc = true,
      "--trace-ints" | "--trace-interrupts" => trace_interrupts = true,
      "--cores" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --cores");
          process::exit(1);
        });
        cores = value.parse::<usize>().unwrap_or_else(|_| {
          println!("Invalid core count: {}", value);
          process::exit(1);
        });
      }
      "--sched" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --sched");
          process::exit(1);
        });
        sched = ScheduleMode::parse(value).unwrap_or_else(|| {
          println!("Unknown scheduler mode: {}", value);
          process::exit(1);
        });
      }
      "--max-cycles" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --max-cycles");
          process::exit(1);
        });
        max_cycles = value.parse::<u32>().unwrap_or_else(|_| {
          println!("Invalid max cycle count: {}", value);
          process::exit(1);
        });
      }
      _ if arg.starts_with("--cores=") => {
        let value = &arg["--cores=".len()..];
        cores = value.parse::<usize>().unwrap_or_else(|_| {
          println!("Invalid core count: {}", value);
          process::exit(1);
        });
      }
      _ if arg.starts_with("--sched=") => {
        let value = &arg["--sched=".len()..];
        sched = ScheduleMode::parse(value).unwrap_or_else(|| {
          println!("Unknown scheduler mode: {}", value);
          process::exit(1);
        });
      }
      _ if arg.starts_with("--max-cycles=") => {
        let value = &arg["--max-cycles=".len()..];
        max_cycles = value.parse::<u32>().unwrap_or_else(|_| {
          println!("Invalid max cycle count: {}", value);
          process::exit(1);
        });
      }
      _ if arg.starts_with('-') => {
        println!("Unknown flag: {}", arg);
        process::exit(1);
      }
      _ => {
        if path.is_none() {
          path = Some(arg.clone());
        } else {
          println!("Usage: cargo run -- <file>.hex [--vga] [--uart] [--debug|--debugc] [--trace-ints] [--cores N] [--sched free|rr|random] [--max-cycles N]");
          process::exit(1);
        }
      }
    }
  }

  if let Some(path) = path {
    set_trace_interrupts(trace_interrupts);
    if debug && debugc {
      println!("Error: --debug and --debugc are mutually exclusive");
      process::exit(1);
    }
    // file to run is passed as a command line argument
    if debugc {
      if with_graphics {
        println!("Warning: --vga is ignored in debugc mode");
      }
      if cores != 1 {
        println!("Warning: --cores is ignored in debugc mode");
      }
      if sched != ScheduleMode::Free {
        println!("Warning: --sched is ignored in debugc mode");
      }
      if max_cycles != 0 {
        println!("Warning: --max-cycles is ignored in debugc mode");
      }
      Emulator::debug_c(path, use_uart_rx);
    } else if debug {
      if with_graphics {
        println!("Warning: --vga is ignored in debug mode");
      }
      if cores != 1 {
        println!("Warning: --cores is ignored in debug mode");
      }
      if sched != ScheduleMode::Free {
        println!("Warning: --sched is ignored in debug mode");
      }
      if max_cycles != 0 {
        println!("Warning: --max-cycles is ignored in debug mode");
      }
      Emulator::debug(path, use_uart_rx);
    } else {
      if cores == 0 || cores > 4 {
        println!("--cores must be in 1..=4");
        process::exit(1);
      }
      if cores == 1 {
        let cpu = Emulator::new(path, use_uart_rx);
        let result = cpu.run(max_cycles, with_graphics).expect("did not terminate"); // programs should return a value in r1
        println!("{:08x}", result);
      } else {
        let result = Emulator::run_multicore(path, cores, sched, max_cycles, with_graphics, use_uart_rx)
          .expect("did not terminate");
        println!("{:08x}", result);
      }
    }
  } else {
    println!("Usage: cargo run -- <file>.hex [--vga] [--uart] [--debug|--debugc] [--trace-ints] [--cores N] [--sched free|rr|random] [--max-cycles N]");
    process::exit(1);
  }
}
