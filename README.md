# Full Dioptase Emulator

Emulator of both the user mode and kernel mode part of the Dioptase ISA

Emulates all the IO devices other than the SD card.  
I/O emulation was written by https://github.com/PaulBailey-1 and https://github.com/Jzhyang1

## Usage

Run the emulator with `cargo run -- <file>.hex`

## Testing

Run all tests with `cargo test`

Test assume the file structure is the same as how things are orginized in the [Dioptase repo](https://github.com/b-Rocks2718/Dioptase/tree/main). This allows the tests to access the assembler.
