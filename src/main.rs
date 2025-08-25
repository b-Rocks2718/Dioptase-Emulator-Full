use std::env;
use std::process;

pub mod emulator;
pub mod tests;

use emulator::Emulator;

fn main() {
  let args = env::args().collect::<Vec<_>>();

  if args.len() == 2 {
    // file to run is passed as a command line argument
    let mut cpu = Emulator::new(args[1].clone());
    let result = cpu.run();
    println!("{:08x}", result);
  } else {
    println!("Usage: cargo run -- file.hex");
    process::exit(64);
  }
}