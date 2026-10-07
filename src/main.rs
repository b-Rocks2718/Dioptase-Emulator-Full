use std::env;
use std::fs;
use std::process;
use std::sync::Arc;

pub mod audio;
pub mod disassembler;
pub mod emulator;
pub mod graphics;
pub mod memory;
pub mod mouse;
mod isa;
#[cfg(test)]
mod tests;

use audio::AudioMode;
use emulator::profiler::{CoreProfile, ProfileWindow, Symbols, WindowStart, write_report};
use emulator::{Emulator, RunConfig, ScheduleMode, run_program, set_trace_interrupts};
use memory::{Memory, SdSlot};

const USAGE: &str = "Usage: cargo run -- --ram <file>.hex [--sd0 <sd0.bin>] [--sd1 <sd1.bin>] [--sd0-out <sd0-out.bin>] [--sd1-out <sd1-out.bin>] [--vga] [--audio|--audio-fast] [--uart] [--debug|--debugc] [--trace-ints] [--cores N] [--sched free|rr|random] [--max-cycles N] [--sd-dma-ticks N] [--profile <report.txt>] [--symbols <file.hex>]... [--profile-start user|<label>|0xADDR] [--profile-stop <label>|0xADDR]";

// Print an error and exit with status 1.
fn fail(message: impl std::fmt::Display) -> ! {
    println!("{}", message);
    process::exit(1);
}

// Parse a numeric flag value or exit naming the flag.
fn parse_number<T: std::str::FromStr>(flag: &str, value: &str) -> T {
    value
        .parse()
        .unwrap_or_else(|_| fail(format!("Invalid value for {}: {}", flag, value)))
}

// Command-line options after parsing.
struct Options {
    config: RunConfig,
    debug: bool,
    debugc: bool,
    trace_interrupts: bool,
    ram_path: String,
    sd0_path: Option<String>,
    sd1_path: Option<String>,
    sd0_out_path: Option<String>,
    sd1_out_path: Option<String>,
    profile_path: Option<String>,
    symbol_paths: Vec<String>,
    profile_start: Option<String>,
    profile_stop: Option<String>,
}

// Parse argv. Value flags accept `--flag value` and `--flag=value`;
// positional arguments fill the RAM image, then SD0, then SD1.
fn parse_args(args: &[String]) -> Options {
    let mut config = RunConfig::default();
    let (mut debug, mut debugc, mut trace_interrupts) = (false, false, false);
    let mut audio_flag: Option<&str> = None;
    let mut ram_path = None;
    let (mut sd0_path, mut sd1_path, mut sd0_out_path, mut sd1_out_path) = (None, None, None, None);
    let (mut profile_path, mut profile_start, mut profile_stop) = (None, None, None);
    let mut symbol_paths = Vec::new();

    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) if arg.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline_value
                .clone()
                .or_else(|| iter.next().cloned())
                .unwrap_or_else(|| fail(format!("Missing value for {}", flag)))
        };
        match flag {
            "--vga" => config.with_graphics = true,
            "--audio" | "--audio-fast" => {
                if audio_flag.is_some_and(|prev| prev != flag) {
                    fail("Error: --audio and --audio-fast are mutually exclusive");
                }
                audio_flag = Some(if flag == "--audio" { "--audio" } else { "--audio-fast" });
                config.audio_mode = if flag == "--audio" { AudioMode::Emulated } else { AudioMode::Fast };
            }
            "--uart" => config.use_uart_rx = true,
            "--debug" => debug = true,
            "--debugc" => debugc = true,
            "--trace-ints" | "--trace-interrupts" => trace_interrupts = true,
            "--cores" => config.cores = parse_number(flag, &value()),
            "--sched" => {
                let token = value();
                config.sched = ScheduleMode::parse(&token)
                    .unwrap_or_else(|| fail(format!("Unknown scheduler mode: {}", token)));
            }
            "--max-cycles" => config.max_cycles = parse_number(flag, &value()),
            "--sd-dma-ticks" => config.sd_dma_ticks_per_word = parse_number(flag, &value()),
            "--ram" => ram_path = Some(value()),
            "--sd0" => sd0_path = Some(value()),
            "--sd1" => sd1_path = Some(value()),
            "--sd0-out" => sd0_out_path = Some(value()),
            "--sd1-out" => sd1_out_path = Some(value()),
            "--profile" => profile_path = Some(value()),
            "--symbols" => symbol_paths.push(value()),
            "--profile-start" => profile_start = Some(value()),
            "--profile-stop" => profile_stop = Some(value()),
            _ if flag.starts_with('-') => fail(format!("Unknown flag: {}", arg)),
            _ => {
                let slot = [&mut ram_path, &mut sd0_path, &mut sd1_path]
                    .into_iter()
                    .find(|slot| slot.is_none())
                    .unwrap_or_else(|| fail(USAGE));
                *slot = Some(arg.clone());
            }
        }
    }

    Options {
        config,
        debug,
        debugc,
        trace_interrupts,
        ram_path: ram_path.unwrap_or_else(|| fail(USAGE)),
        sd0_path,
        sd1_path,
        sd0_out_path,
        sd1_out_path,
        profile_path,
        symbol_paths,
        profile_start,
        profile_stop,
    }
}

// Read an SD image named by --sd0/--sd1.
fn read_sd_image(path: &Option<String>, name: &str) -> Option<Vec<u8>> {
    path.as_ref().map(|path| {
        fs::read(path).unwrap_or_else(|err| fail(format!("Failed to read {} image {}: {}", name, path, err)))
    })
}

// Write the final SD images requested by --sd0-out/--sd1-out.
fn write_sd_exports(options: &Options, dump: impl Fn(SdSlot) -> Vec<u8>) {
    for (path, slot, name) in [
        (&options.sd0_out_path, SdSlot::Sd0, "SD0"),
        (&options.sd1_out_path, SdSlot::Sd1, "SD1"),
    ] {
        if let Some(path) = path {
            fs::write(path, dump(slot))
                .unwrap_or_else(|err| fail(format!("Failed to write {} image {}: {}", name, path, err)));
        }
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

// Configure devices and scheduling from command-line options, then run the
// emulator (or one of the interactive debuggers).
fn main() {
    let args = env::args().collect::<Vec<_>>();
    let mut options = parse_args(&args);
    let sd0_image = read_sd_image(&options.sd0_path, "SD0");
    let sd1_image = read_sd_image(&options.sd1_path, "SD1");

    set_trace_interrupts(options.trace_interrupts);
    if options.config.sd_dma_ticks_per_word == 0 {
        fail("--sd-dma-ticks must be >= 1");
    }
    if options.debug && options.debugc {
        fail("Error: --debug and --debugc are mutually exclusive");
    }
    let debug_mode = if options.debugc { Some("debugc") } else if options.debug { Some("debug") } else { None };
    if options.profile_path.is_none()
        && (!options.symbol_paths.is_empty() || options.profile_start.is_some() || options.profile_stop.is_some())
    {
        println!("Warning: --symbols, --profile-start, and --profile-stop are ignored without --profile");
    }
    if options.profile_path.is_some() && debug_mode.is_some() {
        println!("Warning: --profile is ignored in debug modes");
        options.profile_path = None;
    }

    if let Some(mode) = debug_mode {
        let config = &options.config;
        let ignored = [
            (config.with_graphics, "--vga"),
            (config.audio_mode != AudioMode::Disabled, "host audio flags"),
            (config.cores != 1, "--cores"),
            (config.sched != ScheduleMode::Free, "--sched"),
            (config.max_cycles != 0, "--max-cycles"),
        ];
        for (set, flag) in ignored {
            if set {
                let verb = if flag == "host audio flags" { "are" } else { "is" };
                println!("Warning: {} {} ignored in {} mode", flag, verb, mode);
            }
        }
        let debugger = if mode == "debugc" { Emulator::debug_c } else { Emulator::debug };
        let cpu = debugger(
            options.ram_path.clone(),
            config.use_uart_rx,
            config.sd_dma_ticks_per_word,
            sd0_image.as_deref(),
            sd1_image.as_deref(),
        )
        .unwrap_or_else(|err| fail(err));
        write_sd_exports(&options, |slot| cpu.dump_sd_image(slot));
        return;
    }

    // Load symbols before running so a bad path fails fast, not after a long
    // run. The RAM image is always included since it may carry -g labels too.
    let symbols = options.profile_path.as_ref().map(|_| {
        let mut paths = vec![options.ram_path.clone()];
        paths.extend(options.symbol_paths.iter().cloned());
        Symbols::load(&paths).unwrap_or_else(|err| fail(err))
    });
    if let Some(symbols) = &symbols {
        let window = build_profile_window(
            symbols,
            options.profile_start.as_deref(),
            options.profile_stop.as_deref(),
        )
        .unwrap_or_else(|err| fail(err));
        options.config.profile = Some(window);
    }

    let run = run_program(
        &options.ram_path,
        &options.config,
        sd0_image.as_deref(),
        sd1_image.as_deref(),
    )
    .unwrap_or_else(|err| fail(err));
    write_profile(options.profile_path.as_deref(), &run.profiles, symbols.as_ref());
    // Programs return a value in r1; a missing result means the cycle budget ran out.
    let result = run.result.expect("did not terminate");
    let memory: &Memory = &run.memory;
    write_sd_exports(&options, |slot| memory.dump_sd_image(slot));
    println!("{:08x}", result);
}
