# Full Dioptase Emulator

Emulator of both the user mode and kernel mode part of the Dioptase ISA

Emulates all the IO devices other than the SD card.  
I/O emulation was written by [Paul Bailey](https://github.com/PaulBailey-1) and [Jonathan Yang](https://github.com/Jzhyang1)
for the [JPEB project](https://github.com/PaulBailey-1/JPEB) and re-used here.

## Usage

Run the emulator with `cargo run -- <file>.hex`

Use the `-graphics` flag to open a window with the VGA output

Use the `-uart` flag to route keyboard input to the `UART_RX` address instead of the `PS2_STREAM` address

## Testing

Run all tests with `cargo test`

Test assume the file structure is the same as how things are orginized in the [Dioptase repo](https://github.com/b-Rocks2718/Dioptase/tree/main). This allows the tests to access the assembler.
