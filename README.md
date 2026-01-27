# Full Dioptase Emulator

Emulator of both the user mode and kernel mode part of the Dioptase ISA

Emulates the IO devices including the SD card DMA engine.  
I/O emulation was written by [Paul Bailey](https://github.com/PaulBailey-1) and [Jonathan Yang](https://github.com/Jzhyang1)
for the [JPEB project](https://github.com/PaulBailey-1/JPEB) and re-used here.

## Usage

Run the emulator with `cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>]`

You can also pass positional files in order: `cargo run -- <ram.hex> [sd0.bin] [sd1.bin]`

Use the `--vga` flag to open a window with the VGA output

Use the `--uart` flag to route keyboard input to the `UART_RX` address instead of the `PS2_STREAM` address

Use the `--debug` flag to start an interactive debugger (label breakpoints require `.debug` files built with assembler `--debug`)

Use the `--sd-dma-ticks <N>` flag to set the number of emulator ticks per 4-byte SD DMA transfer (default 1)

Use the `--sd0 <file>` and `--sd1 <file>` flags to load raw binary SD images into the two SD devices

SD images are raw binary byte streams; byte 0 maps to SD block 0 byte 0

### Debug Commands

- `r` reset and run until break/watchpoint/halt
- `c` continue execution
- `n` step one instruction
- `break <label|addr>` set breakpoint
- `breaks` list breakpoints
- `delete <label|addr>` remove breakpoint
- `watch [r|w|rw] <addr>` stop on memory access
- `watchs` list watchpoints
- `unwatch <addr>` remove watchpoint
- `info regs` print all registers
- `info cregs` print control registers + kmode
- `info <reg>` print a single register
- `info tlb` dump TLB maps
- `info p <addr>` print word at physical address
- `info v <addr>` print word + resolved physical address
- `x [v|p] <addr> <len>` dump memory range
- `set reg <reg> <value>` write a register
- `q` quit

## Testing

Run all tests with `cargo test`

Test assume the file structure is the same as how things are orginized in the [Dioptase repo](https://github.com/b-Rocks2718/Dioptase/tree/main). This allows the tests to access the assembler.
