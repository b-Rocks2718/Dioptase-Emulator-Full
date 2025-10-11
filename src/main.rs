use std::env;
use std::process;

pub mod emulator;
pub mod tests;
pub mod graphics;
pub mod memory;

use emulator::Emulator;

fn main() {
  let args = env::args().collect::<Vec<_>>();

  let with_graphics = args.contains(&String::from("--vga"));
  let use_uart_rx = args.contains(&String::from("--uart"));

  if args.len() >= 2 {
    // file to run is passed as a command line argument
    let cpu = Emulator::new(args[1].clone(), use_uart_rx);
    let result = cpu.run(0, with_graphics).expect("did not terminate");
    println!("{:08x}", result);
  } else {
    println!("Usage: cargo run -- file.hex");
    process::exit(64);
  }
}