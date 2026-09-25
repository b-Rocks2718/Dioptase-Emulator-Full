# Full Dioptase Emulator

[![CI](https://github.com/b-Rocks2718/Dioptase-Emulator-Full/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/b-Rocks2718/Dioptase-Emulator-Full/actions/workflows/ci.yml)

Emulator of both the user mode and kernel mode part of the Dioptase ISA

Emulates the IO devices including the SD card DMA engine.  
I/O emulation was written by [Paul Bailey](https://github.com/PaulBailey-1) and [Jonathan Yang](https://github.com/Jzhyang1)
for the [JPEB project](https://github.com/PaulBailey-1/JPEB) and re-used here.

## Usage

Run the emulator with `cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>] [--sd0-out <sd0-out.bin>] [--sd1-out <sd1-out.bin>]`

You can also pass positional files in order: `cargo run -- <ram.hex> [sd0.bin] [sd1.bin]`

Use the `--vga` flag to open a window with the VGA output

Use the `--audio` flag to pipe the emulated `25 kHz` mono `s16le` PCM audio stream to `ffplay` for host playback (requires `ffplay` on `PATH`).

Use the `--audio-fast` flag to drive the MMIO audio device from wall-clock time instead of emulated device ticks so host playback remains intelligible when emulation is slow. This is a debugging convenience mode and intentionally changes guest-visible audio timing. If the host audio player falls behind, fast mode may drop host samples rather than stalling MMIO device time.

Use the `--uart` flag to route keyboard input to the `UART_RX` address instead of the `PS2_STREAM` address

Use the `--debug` flag to start an interactive debugger (label breakpoints require `.debug` files built with assembler `--debug`)

Use `--sched` to change the scheduling of when cores run. Options are `free`, `rr` (round robin), and `random`.

Use the `--sd-dma-ticks <N>` flag to set the number of emulator ticks per 4-byte SD DMA transfer (default 1)

Use the `--sd0 <file>` and `--sd1 <file>` flags to load raw binary SD images into the two SD devices

Use the `--sd0-out <file>` and `--sd1-out <file>` flags to write the final raw SD images back to disk when the emulator exits

SD images are raw binary byte streams; byte 0 maps to SD block 0 byte 0

### Profiling

Use `--profile <report.txt>` to count every instruction each core executes and write a report when the run ends (also when `--max-cycles` stops it). Counts are exact, not sampled, but they are instruction counts, not hardware cycles: the emulator does not model caches or pipeline stalls.

Use `--symbols <file.hex>` (repeatable) to name kernel functions and source lines. Pass the kernel `.hex` built with `basm -g` (the OS build writes `build/<test>.hex` next to each `build/<test>.bin`). The `--ram` image is always loaded for symbols too.

The report has per-core tick counts (including ticks spent asleep), a table of all functions by instruction count, the hottest kernel source lines, and the hottest instructions with disassembly. User-mode code is reported per PID (the raw PID control register value, in hex) without symbols.

Labels containing `.` are treated as local labels and folded into the enclosing function.

Example: `Dioptase-Emulator-Full build/bios.hex --sd0 build/test.bin --cores 4 --profile prof.txt --symbols build/test.hex`

Use `--profile-start <trigger>` and `--profile-stop <trigger>` to count only part of the run. A trigger is a kernel label from the symbol files, a `0x`-prefixed kernel address, or (for start only) `user`, meaning the first user-mode instruction on any core. The window is system-wide: while it is open, every core is counted, including idle ones. It can open and close more than once. The report covers all open intervals and says how many times the window opened.

To measure while a user program runs, e.g. an OS test whose `/sbin/init` does the work: `--profile-start user --profile-stop stop`. `stop` is the kernel's thread-exit function. Any exiting thread closes the window, so if the report shows it opened more than once, a kernel thread exited while the program was running.

With more than one core, the report also lists each core's top functions, so busy-waiting on an idle core is visible.

Kernel time is split into two parts:

- **Kernel work entered from user mode.** Each kernel instruction is charged to the handler the core entered through when it last left user mode, until the core returns to user mode or sleeps. The report lists each entry point (timer interrupt, TLB miss, syscall, ...) with its total instructions, number of entries, and average cost per entry.
- **Background kernel work.** Everything else: boot, idle loops, and kernel threads.

Each part gets its own function table and a callers breakdown. Calls are detected when `r29` (the link register) equals the previous PC + 4, so call counts are exact. The "est." column splits a callee's self instructions across its callers by call count, which assumes a similar cost per call.

Plain assembly labels that are never called and extend the previous label's name with `_` (e.g. `smul_loop` after `smul`) are folded into that routine in the function tables. The hot instruction table still shows the exact label.

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
