
#[cfg(test)]
use std::process::Command;

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use super::*;

#[cfg(test)]
fn run_test(asm_file : &'static str, expected : u32){

  // Build hex file path by replacing asm path prefix/suffix
  let hex_file = {
    let asm_path = Path::new(asm_file);
    let stem = asm_path.file_stem().unwrap(); // e.g., "add"
    PathBuf::from("tests/hex").join(format!("{}.hex", stem.to_string_lossy()))
  };

  // assemble test case
  let status = Command::new("../../Dioptase-Assembler/build/assembler")
    .args([asm_file, "-o", hex_file.to_str().unwrap()])
    .status()
    .expect("failed to run assembler");
  assert!(status.success(), "assembler failed");

  // execute hex file
  let mut cpu = Emulator::new(hex_file.to_string_lossy().to_string());
  let result = cpu.run();

  // check result
  assert_eq!(result, expected);
}
