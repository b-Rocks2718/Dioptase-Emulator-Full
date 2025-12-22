use std::env;
use std::process;

pub mod emulator;
pub mod tests;
pub mod graphics;
pub mod memory;
pub mod disassembler;

use emulator::Emulator;

fn main() {
  let args = env::args().collect::<Vec<_>>();

  let mut with_graphics = false;
  let mut use_uart_rx = false;
  let mut debug = false;
  let mut path: Option<String> = None;

  for arg in args.iter().skip(1) {
    match arg.as_str() {
      "--vga" => with_graphics = true,
      "--uart" => use_uart_rx = true,
      "--debug" => debug = true,
      _ if arg.starts_with('-') => {
        println!("Unknown flag: {}", arg);
        process::exit(1);
      }
      _ => {
        if path.is_none() {
          path = Some(arg.clone());
        } else {
          println!("Usage: cargo run -- <file>.hex [--vga] [--uart] [--debug]");
          process::exit(1);
        }
      }
    }
  }

  if let Some(path) = path {
    // file to run is passed as a command line argument
    if debug {
      if with_graphics {
        println!("Warning: --vga is ignored in debug mode");
      }
      Emulator::debug(path, use_uart_rx);
    } else {
      let cpu = Emulator::new(path, use_uart_rx);
      let result = cpu.run(0, with_graphics).expect("did not terminate"); // programs should return a value in r3
      println!("{:08x}", result);
    }
  } else {
    println!("Usage: cargo run -- <file>.hex [--vga] [--uart] [--debug]");
    process::exit(1);
  }
}
