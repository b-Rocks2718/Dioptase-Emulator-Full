use std::env;
use std::fs;
use std::process;
use std::sync::Arc;

pub mod audio;
pub mod disassembler;
pub mod emulator;
pub mod graphics;
pub mod memory;
pub mod tests;

use emulator::profiler::{CoreProfile, ProfileWindow, Symbols, WindowStart, write_report};
use emulator::{AudioMode, Emulator, ScheduleMode, set_trace_interrupts};
use memory::SdSlot;

const USAGE: &str = "Usage: cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>] [--sd0-out <sd0-out.bin>] [--sd1-out <sd1-out.bin>] [--vga] [--audio|--audio-fast] [--uart] [--debug|--debugc] [--trace-ints] [--cores N] [--sched free|rr|random] [--max-cycles N] [--sd-dma-ticks N] [--profile <report.txt>] [--symbols <file.hex>]... [--profile-start user|<label>|0xADDR] [--profile-stop <label>|0xADDR]";

// Print usage and exit.
fn print_usage_and_exit() -> ! {
    println!("{}", USAGE);
    process::exit(1);
}

fn write_sd_export<F>(path: Option<&str>, slot: SdSlot, dump_image: F)
where
    F: FnOnce() -> Vec<u8>,
{
    if let Some(path) = path {
        let image = dump_image();
        fs::write(path, image).unwrap_or_else(|err| {
            let slot_name = match slot {
                SdSlot::Sd0 => "SD0",
                SdSlot::Sd1 => "SD1",
            };
            println!("Failed to write {} image {}: {}", slot_name, path, err);
            process::exit(1);
        });
    }
}

// Write the profile report if `--profile` was given. Runs before the
// "did not terminate" check so runs stopped by --max-cycles still get a report.
fn write_profile(path: Option<&str>, profiles: &[CoreProfile], symbols: Option<&Symbols>) {
    let (Some(path), Some(symbols)) = (path, symbols) else {
        return;
    };
    // stderr keeps stdout identical to unprofiled runs (OS tests compare it).
    match write_report(path, profiles, symbols) {
        Ok(()) => eprintln!("Profile written to {}", path),
        Err(err) => {
            eprintln!("{}", err);
            process::exit(1);
        }
    }
}

// Build the shared measurement window from --profile-start/--profile-stop.
// `user` starts at the first user-mode instruction; anything else is a kernel
// label or 0x address resolved against the loaded symbols.
fn build_profile_window(
    symbols: &Symbols,
    start: Option<&str>,
    stop: Option<&str>,
) -> Result<Arc<ProfileWindow>, String> {
    if start.is_none() && stop.is_none() {
        return Ok(ProfileWindow::whole_run());
    }
    let (start_kind, start_desc) = match start {
        None => (WindowStart::RunStart, "run start".to_string()),
        Some("user") => (WindowStart::UserMode, "first user-mode instruction".to_string()),
        Some(token) => (
            WindowStart::KernelPcs(symbols.resolve_trigger("--profile-start", token)?),
            token.to_string(),
        ),
    };
    let (stop_pcs, stop_desc) = match stop {
        None => (Vec::new(), "run end".to_string()),
        Some(token) => (symbols.resolve_trigger("--profile-stop", token)?, token.to_string()),
    };
    let description = format!("start at {}, stop at {}", start_desc, stop_desc);
    Ok(ProfileWindow::new(start_kind, stop_pcs, description))
}

// Configure devices and scheduling from command-line options, then run the emulator.
fn main() {
    let args = env::args().collect::<Vec<_>>();

    let mut with_graphics = false;
    let mut audio_mode = AudioMode::Disabled;
    let mut use_uart_rx = false;
    let mut debug = false;
    let mut debugc = false;
    let mut trace_interrupts = false;
    let mut cores: usize = 1;
    let mut sched = ScheduleMode::Free;
    let mut max_cycles: u32 = 0;
    let mut sd_dma_ticks_per_word: u32 = 1;
    let mut ram_path: Option<String> = None;
    let mut sd0_path: Option<String> = None;
    let mut sd1_path: Option<String> = None;
    let mut sd0_out_path: Option<String> = None;
    let mut sd1_out_path: Option<String> = None;
    let mut profile_path: Option<String> = None;
    let mut symbol_paths: Vec<String> = Vec::new();
    let mut profile_start: Option<String> = None;
    let mut profile_stop: Option<String> = None;

    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--vga" => with_graphics = true,
            "--audio" => {
                if audio_mode == AudioMode::Fast {
                    println!("Error: --audio and --audio-fast are mutually exclusive");
                    process::exit(1);
                }
                audio_mode = AudioMode::Emulated;
            }
            "--audio-fast" => {
                if audio_mode == AudioMode::Emulated {
                    println!("Error: --audio and --audio-fast are mutually exclusive");
                    process::exit(1);
                }
                audio_mode = AudioMode::Fast;
            }
            "--uart" => use_uart_rx = true,
            "--debug" => debug = true,
            "--debugc" => debugc = true,
            "--trace-ints" | "--trace-interrupts" => trace_interrupts = true,
            "--cores" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --cores");
                    process::exit(1);
                });
                cores = value.parse::<usize>().unwrap_or_else(|_| {
                    println!("Invalid core count: {}", value);
                    process::exit(1);
                });
            }
            "--sched" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --sched");
                    process::exit(1);
                });
                sched = ScheduleMode::parse(value).unwrap_or_else(|| {
                    println!("Unknown scheduler mode: {}", value);
                    process::exit(1);
                });
            }
            "--max-cycles" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --max-cycles");
                    process::exit(1);
                });
                max_cycles = value.parse::<u32>().unwrap_or_else(|_| {
                    println!("Invalid max cycle count: {}", value);
                    process::exit(1);
                });
            }
            "--sd-dma-ticks" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --sd-dma-ticks");
                    process::exit(1);
                });
                sd_dma_ticks_per_word = value.parse::<u32>().unwrap_or_else(|_| {
                    println!("Invalid SD DMA tick count: {}", value);
                    process::exit(1);
                });
            }
            "--ram" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --ram");
                    process::exit(1);
                });
                ram_path = Some(value.clone());
            }
            "--sd0" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --sd0");
                    process::exit(1);
                });
                sd0_path = Some(value.clone());
            }
            "--sd1" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --sd1");
                    process::exit(1);
                });
                sd1_path = Some(value.clone());
            }
            "--sd0-out" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --sd0-out");
                    process::exit(1);
                });
                sd0_out_path = Some(value.clone());
            }
            "--sd1-out" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --sd1-out");
                    process::exit(1);
                });
                sd1_out_path = Some(value.clone());
            }
            "--profile" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --profile");
                    process::exit(1);
                });
                profile_path = Some(value.clone());
            }
            "--symbols" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --symbols");
                    process::exit(1);
                });
                symbol_paths.push(value.clone());
            }
            "--profile-start" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --profile-start");
                    process::exit(1);
                });
                profile_start = Some(value.clone());
            }
            "--profile-stop" => {
                let value = iter.next().unwrap_or_else(|| {
                    println!("Missing value for --profile-stop");
                    process::exit(1);
                });
                profile_stop = Some(value.clone());
            }
            _ if arg.starts_with("--profile-start=") => {
                profile_start = Some(arg["--profile-start=".len()..].to_string());
            }
            _ if arg.starts_with("--profile-stop=") => {
                profile_stop = Some(arg["--profile-stop=".len()..].to_string());
            }
            _ if arg.starts_with("--profile=") => {
                profile_path = Some(arg["--profile=".len()..].to_string());
            }
            _ if arg.starts_with("--symbols=") => {
                symbol_paths.push(arg["--symbols=".len()..].to_string());
            }
            _ if arg.starts_with("--cores=") => {
                let value = &arg["--cores=".len()..];
                cores = value.parse::<usize>().unwrap_or_else(|_| {
                    println!("Invalid core count: {}", value);
                    process::exit(1);
                });
            }
            _ if arg.starts_with("--sched=") => {
                let value = &arg["--sched=".len()..];
                sched = ScheduleMode::parse(value).unwrap_or_else(|| {
                    println!("Unknown scheduler mode: {}", value);
                    process::exit(1);
                });
            }
            _ if arg.starts_with("--max-cycles=") => {
                let value = &arg["--max-cycles=".len()..];
                max_cycles = value.parse::<u32>().unwrap_or_else(|_| {
                    println!("Invalid max cycle count: {}", value);
                    process::exit(1);
                });
            }
            _ if arg.starts_with("--ram=") => {
                let value = &arg["--ram=".len()..];
                ram_path = Some(value.to_string());
            }
            _ if arg.starts_with("--sd0=") => {
                let value = &arg["--sd0=".len()..];
                sd0_path = Some(value.to_string());
            }
            _ if arg.starts_with("--sd1=") => {
                let value = &arg["--sd1=".len()..];
                sd1_path = Some(value.to_string());
            }
            _ if arg.starts_with("--sd0-out=") => {
                let value = &arg["--sd0-out=".len()..];
                sd0_out_path = Some(value.to_string());
            }
            _ if arg.starts_with("--sd1-out=") => {
                let value = &arg["--sd1-out=".len()..];
                sd1_out_path = Some(value.to_string());
            }
            _ if arg.starts_with("--sd-dma-ticks=") => {
                let value = &arg["--sd-dma-ticks=".len()..];
                sd_dma_ticks_per_word = value.parse::<u32>().unwrap_or_else(|_| {
                    println!("Invalid SD DMA tick count: {}", value);
                    process::exit(1);
                });
            }
            _ if arg.starts_with('-') => {
                println!("Unknown flag: {}", arg);
                process::exit(1);
            }
            _ => {
                if ram_path.is_none() {
                    ram_path = Some(arg.clone());
                } else if sd0_path.is_none() {
                    sd0_path = Some(arg.clone());
                } else if sd1_path.is_none() {
                    sd1_path = Some(arg.clone());
                } else {
                    print_usage_and_exit();
                }
            }
        }
    }

    let ram_path = if let Some(path) = ram_path {
        path
    } else {
        print_usage_and_exit();
    };

    let sd0_image = sd0_path.as_ref().map(|path| {
        fs::read(path).unwrap_or_else(|err| {
            println!("Failed to read SD0 image {}: {}", path, err);
            process::exit(1);
        })
    });
    let sd1_image = sd1_path.as_ref().map(|path| {
        fs::read(path).unwrap_or_else(|err| {
            println!("Failed to read SD1 image {}: {}", path, err);
            process::exit(1);
        })
    });

    set_trace_interrupts(trace_interrupts);
    if sd_dma_ticks_per_word == 0 {
        println!("--sd-dma-ticks must be >= 1");
        process::exit(1);
    }
    if debug && debugc {
        println!("Error: --debug and --debugc are mutually exclusive");
        process::exit(1);
    }
    if profile_path.is_none()
        && (!symbol_paths.is_empty() || profile_start.is_some() || profile_stop.is_some())
    {
        println!("Warning: --symbols, --profile-start, and --profile-stop are ignored without --profile");
    }
    if profile_path.is_some() && (debug || debugc) {
        println!("Warning: --profile is ignored in debug modes");
        profile_path = None;
    }
    // Load symbols before running so a bad path fails fast, not after a long
    // run. The RAM image is always included since it may carry -g labels too.
    let symbols = profile_path.as_ref().map(|_| {
        let mut paths = vec![ram_path.clone()];
        paths.extend(symbol_paths.iter().cloned());
        Symbols::load(&paths).unwrap_or_else(|err| {
            println!("{}", err);
            process::exit(1);
        })
    });
    let window = symbols.as_ref().map(|symbols| {
        build_profile_window(symbols, profile_start.as_deref(), profile_stop.as_deref())
            .unwrap_or_else(|err| {
                println!("{}", err);
                process::exit(1);
            })
    });
    // file to run is passed as a command line argument
    if debugc {
        if with_graphics {
            println!("Warning: --vga is ignored in debugc mode");
        }
        if audio_mode != AudioMode::Disabled {
            println!("Warning: host audio flags are ignored in debugc mode");
        }
        if cores != 1 {
            println!("Warning: --cores is ignored in debugc mode");
        }
        if sched != ScheduleMode::Free {
            println!("Warning: --sched is ignored in debugc mode");
        }
        if max_cycles != 0 {
            println!("Warning: --max-cycles is ignored in debugc mode");
        }
        let cpu = Emulator::debug_c(
            ram_path,
            use_uart_rx,
            sd_dma_ticks_per_word,
            sd0_image.as_deref(),
            sd1_image.as_deref(),
        );
        write_sd_export(sd0_out_path.as_deref(), SdSlot::Sd0, || {
            cpu.dump_sd_image(SdSlot::Sd0)
        });
        write_sd_export(sd1_out_path.as_deref(), SdSlot::Sd1, || {
            cpu.dump_sd_image(SdSlot::Sd1)
        });
    } else if debug {
        if with_graphics {
            println!("Warning: --vga is ignored in debug mode");
        }
        if audio_mode != AudioMode::Disabled {
            println!("Warning: host audio flags are ignored in debug mode");
        }
        if cores != 1 {
            println!("Warning: --cores is ignored in debug mode");
        }
        if sched != ScheduleMode::Free {
            println!("Warning: --sched is ignored in debug mode");
        }
        if max_cycles != 0 {
            println!("Warning: --max-cycles is ignored in debug mode");
        }
        let cpu = Emulator::debug(
            ram_path,
            use_uart_rx,
            sd_dma_ticks_per_word,
            sd0_image.as_deref(),
            sd1_image.as_deref(),
        );
        write_sd_export(sd0_out_path.as_deref(), SdSlot::Sd0, || {
            cpu.dump_sd_image(SdSlot::Sd0)
        });
        write_sd_export(sd1_out_path.as_deref(), SdSlot::Sd1, || {
            cpu.dump_sd_image(SdSlot::Sd1)
        });
    } else {
        if cores == 0 || cores > 4 {
            println!("--cores must be in 1..=4");
            process::exit(1);
        }
        if cores == 1 {
            let mut cpu = Emulator::new(
                ram_path,
                use_uart_rx,
                sd_dma_ticks_per_word,
                sd0_image.as_deref(),
                sd1_image.as_deref(),
            );
            if let Some(window) = &window {
                cpu.enable_profiling(window.clone());
            }
            let memory = cpu.shared_memory();
            let (result, profile) = cpu.run_with_profile(max_cycles, with_graphics, audio_mode);
            let profiles: Vec<CoreProfile> = profile.into_iter().collect();
            write_profile(profile_path.as_deref(), &profiles, symbols.as_ref());
            let result = result.expect("did not terminate"); // programs should return a value in r1
            write_sd_export(sd0_out_path.as_deref(), SdSlot::Sd0, || {
                memory.dump_sd_image(SdSlot::Sd0)
            });
            write_sd_export(sd1_out_path.as_deref(), SdSlot::Sd1, || {
                memory.dump_sd_image(SdSlot::Sd1)
            });
            println!("{:08x}", result);
        } else {
            let (result, memory, profiles) = Emulator::run_multicore_with_memory(
                ram_path,
                cores,
                sched,
                max_cycles,
                with_graphics,
                audio_mode,
                use_uart_rx,
                sd_dma_ticks_per_word,
                sd0_image.as_deref(),
                sd1_image.as_deref(),
                window.clone(),
            );
            write_profile(profile_path.as_deref(), &profiles, symbols.as_ref());
            let result = result.expect("did not terminate");
            write_sd_export(sd0_out_path.as_deref(), SdSlot::Sd0, || {
                memory.dump_sd_image(SdSlot::Sd0)
            });
            write_sd_export(sd1_out_path.as_deref(), SdSlot::Sd1, || {
                memory.dump_sd_image(SdSlot::Sd1)
            });
            println!("{:08x}", result);
        }
    }
}
