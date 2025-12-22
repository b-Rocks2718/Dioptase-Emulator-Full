# Full Dioptase Emulator

Emulator of both the user mode and kernel mode part of the Dioptase ISA

Emulates all the IO devices other than the SD card.  
I/O emulation was written by [Paul Bailey](https://github.com/PaulBailey-1) and [Jonathan Yang](https://github.com/Jzhyang1)
for the [JPEB project](https://github.com/PaulBailey-1/JPEB) and re-used here.

## Usage

Run the emulator with `cargo run -- <file>.hex`

Use the `--vga` flag to open a window with the VGA output

Use the `--uart` flag to route keyboard input to the `UART_RX` address instead of the `PS2_STREAM` address

Use the `--debug` flag to start an interactive debugger (label breakpoints require `.debug` files built with assembler `--debug`)

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
