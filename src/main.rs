use std::env;
use std::fs;
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
  let mut sd_dma_ticks_per_word: u32 = 1;
  let mut ram_path: Option<String> = None;
  let mut sd0_path: Option<String> = None;
  let mut sd1_path: Option<String> = None;

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
      "--sd-dma-ticks" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --sd-dma-ticks");
          process::exit(1);
        });
        sd_dma_ticks_per_word = value.parse::<u32>().unwrap_or_else(|_| {
          println!("Invalid SD DMA tick count: {}", value);
          process::exit(1);
        });
      }
      "--ram" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --ram");
          process::exit(1);
        });
        ram_path = Some(value.clone());
      }
      "--sd0" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --sd0");
          process::exit(1);
        });
        sd0_path = Some(value.clone());
      }
      "--sd1" => {
        let value = iter.next().unwrap_or_else(|| {
          println!("Missing value for --sd1");
          process::exit(1);
        });
        sd1_path = Some(value.clone());
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
      _ if arg.starts_with("--ram=") => {
        let value = &arg["--ram=".len()..];
        ram_path = Some(value.to_string());
      }
      _ if arg.starts_with("--sd0=") => {
        let value = &arg["--sd0=".len()..];
        sd0_path = Some(value.to_string());
      }
      _ if arg.starts_with("--sd1=") => {
        let value = &arg["--sd1=".len()..];
        sd1_path = Some(value.to_string());
      }
      _ if arg.starts_with("--sd-dma-ticks=") => {
        let value = &arg["--sd-dma-ticks=".len()..];
        sd_dma_ticks_per_word = value.parse::<u32>().unwrap_or_else(|_| {
          println!("Invalid SD DMA tick count: {}", value);
          process::exit(1);
        });
      }
      _ if arg.starts_with('-') => {
        println!("Unknown flag: {}", arg);
        process::exit(1);
      }
      _ => {
        if ram_path.is_none() {
          ram_path = Some(arg.clone());
        } else if sd0_path.is_none() {
          sd0_path = Some(arg.clone());
        } else if sd1_path.is_none() {
          sd1_path = Some(arg.clone());
        } else {
          println!("Usage: cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>] [--vga] [--uart] [--debug|--debugc] [--trace-ints] [--cores N] [--sched free|rr|random] [--max-cycles N] [--sd-dma-ticks N]");
          process::exit(1);
        }
      }
    }
  }

  let ram_path = if let Some(path) = ram_path {
    path
  } else {
    println!("Usage: cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>] [--vga] [--uart] [--debug|--debugc] [--trace-ints] [--cores N] [--sched free|rr|random] [--max-cycles N] [--sd-dma-ticks N]");
    process::exit(1);
  };

  let sd0_image = sd0_path.as_ref().map(|path| {
    fs::read(path).unwrap_or_else(|err| {
      println!("Failed to read SD0 image {}: {}", path, err);
      process::exit(1);
    })
  });
  let sd1_image = sd1_path.as_ref().map(|path| {
    fs::read(path).unwrap_or_else(|err| {
      println!("Failed to read SD1 image {}: {}", path, err);
      process::exit(1);
    })
  });

  set_trace_interrupts(trace_interrupts);
  if sd_dma_ticks_per_word == 0 {
    println!("--sd-dma-ticks must be >= 1");
    process::exit(1);
  }
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
    Emulator::debug_c(
      ram_path,
      use_uart_rx,
      sd_dma_ticks_per_word,
      sd0_image.as_deref(),
      sd1_image.as_deref(),
    );
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
    Emulator::debug(
      ram_path,
      use_uart_rx,
      sd_dma_ticks_per_word,
      sd0_image.as_deref(),
      sd1_image.as_deref(),
    );
  } else {
    if cores == 0 || cores > 4 {
      println!("--cores must be in 1..=4");
      process::exit(1);
    }
    if cores == 1 {
      let cpu = Emulator::new(
        ram_path,
        use_uart_rx,
        sd_dma_ticks_per_word,
        sd0_image.as_deref(),
        sd1_image.as_deref(),
      );
      let result = cpu.run(max_cycles, with_graphics).expect("did not terminate"); // programs should return a value in r1
      println!("{:08x}", result);
    } else {
      let result = Emulator::run_multicore(
        ram_path,
        cores,
        sched,
        max_cycles,
        with_graphics,
        use_uart_rx,
        sd_dma_ticks_per_word,
        sd0_image.as_deref(),
        sd1_image.as_deref(),
      )
        .expect("did not terminate");
      println!("{:08x}", result);
    }
  }
}
