use std::env;
use std::process;

pub mod emulator;
pub mod tests;
pub mod graphics;
pub mod memory;

use emulator::Emulator;

fn main() {
  let args = env::args().collect::<Vec<_>>();

  if args.len() == 2 {
    // file to run is passed as a command line argument
    let cpu = Emulator::new(args[1].clone());
    let result = cpu.run(1000, true).expect("did not terminate");
    println!("{:08x}", result);
  } else {
    println!("Usage: cargo run -- file.hex");
    process::exit(64);
  }
}