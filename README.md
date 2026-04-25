# Full Dioptase Emulator

Emulator of both the user mode and kernel mode part of the Dioptase ISA

Emulates the IO devices including the SD card DMA engine.  
I/O emulation was written by [Paul Bailey](https://github.com/PaulBailey-1) and [Jonathan Yang](https://github.com/Jzhyang1)
for the [JPEB project](https://github.com/PaulBailey-1/JPEB) and re-used here.

## Usage

Run the emulator with `cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>] [--sd0-out <sd0-out.bin>] [--sd1-out <sd1-out.bin>]`

You can also pass positional files in order: `cargo run -- <ram.hex> [sd0.bin] [sd1.bin]`

Use the `--vga` flag to open a window with the VGA output

Use the `--audio` flag to pipe the emulated mixed `25 kHz` mono `s16le` audio stream to `ffplay` for host playback (requires `ffplay` on `PATH`). The stream includes both the existing PCM ring-buffer device and the register-driven synth audio device.

Use the `--audio-fast` flag to drive the MMIO audio devices from wall-clock time instead of emulated device ticks so host playback remains intelligible when emulation is slow. This is a debugging convenience mode and intentionally changes guest-visible audio timing. If the host audio player falls behind, fast mode may drop host samples rather than stalling MMIO device time.

### MIDI to Synth Audio

Use `scripts/midi_to_dsyn.py` to convert a standard MIDI file into the DSYN v1
command-stream format for the synth audio MMIO device:

```sh
python3 scripts/midi_to_dsyn.py song.mid song.dsyn
```

You can also open a GUI for selecting the MIDI source, assigning existing MIDI
channels to each hardware synth channel, editing per-channel settings, saving
configs, converting to DSYN, and previewing the rendered synth output:

```sh
python3 scripts/midi_to_dsyn.py --gui
```

The preview button renders the same DSYN register writes that the converter
will save, then plays them through `ffplay`, `aplay`, `paplay`, or `afplay` if
one is available on `PATH`. The GUI can also save the rendered preview as a WAV
file.

The default mapping uses MIDI channels 1-4 for `square0`-`square3`, channels
5-6 for `triangle0`-`triangle1`, and channel 10 for `noise0`. To create an
editable JSON config:

```sh
python3 scripts/midi_to_dsyn.py --write-default-config synth_config.json
python3 scripts/midi_to_dsyn.py song.mid song.dsyn --config synth_config.json
```

Quick command-line overrides are also supported:

```sh
python3 scripts/midi_to_dsyn.py song.mid song.dsyn \
  --map square0=1 \
  --set square0.duty=1 \
  --set triangle0.transpose=-12 \
  --set noise0.timer=900
```

Use the `--uart` flag to route keyboard input to the `UART_RX` address instead of the `PS2_STREAM` address

Use the `--debug` flag to start an interactive debugger (label breakpoints require `.debug` files built with assembler `--debug`)

Use `--sched` to change the scheduling of when cores run. Options are `free`, `rr` (round robin), and `random`.

Use the `--sd-dma-ticks <N>` flag to set the number of emulator ticks per 4-byte SD DMA transfer (default 1)

Use the `--sd0 <file>` and `--sd1 <file>` flags to load raw binary SD images into the two SD devices

Use the `--sd0-out <file>` and `--sd1-out <file>` flags to write the final raw SD images back to disk when the emulator exits

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
