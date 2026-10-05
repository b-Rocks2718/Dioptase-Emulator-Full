// Run setup and per-core run loops for 1..=4 cores sharing one memory.
//
// Each core runs on its own host thread; the main thread runs the optional
// VGA window. Run termination:
// - any core executing `mode halt` stops every core, and the run result is
//   core 0's r1 (Dioptase-OS tests rely on this even when another core halts);
// - reaching the --max-cycles budget on any core stops every core with no
//   result ("did not terminate").
//
// Scheduling (`--sched`):
// - free: cores run concurrently with no coordination (real host races);
// - rr / random: exactly one core ticks at a time, handing the turn to the
//   next core in order or to a random core after every tick.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use super::interrupts::InterruptController;
use super::profiler::{CoreProfile, ProfileWindow};
use super::{Emulator, build_memory, load_program};
use crate::audio::{AudioMode, AudioPlayback};
use crate::graphics::Graphics;
use crate::memory::Memory;

// Largest supported core count (CID is 2 bits wide in `ipi` targets).
pub const MAX_CORES: usize = 4;

// Scheduler policy for multicore execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleMode {
    Free,
    RoundRobin,
    Random,
}

impl ScheduleMode {
    // Parse a scheduler-mode command-line token.
    pub fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "free" => Some(ScheduleMode::Free),
            "rr" | "round-robin" | "roundrobin" => Some(ScheduleMode::RoundRobin),
            "rand" | "random" => Some(ScheduleMode::Random),
            _ => None,
        }
    }
}

// Everything that configures one emulator run.
#[derive(Clone)]
pub struct RunConfig {
    pub cores: usize,
    pub sched: ScheduleMode,
    // Per-core tick budget; 0 means unlimited.
    pub max_cycles: u32,
    pub with_graphics: bool,
    pub audio_mode: AudioMode,
    pub use_uart_rx: bool,
    pub sd_dma_ticks_per_word: u32,
    // Shared measurement window when `--profile` is enabled.
    pub profile: Option<Arc<ProfileWindow>>,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            cores: 1,
            sched: ScheduleMode::Free,
            max_cycles: 0,
            with_graphics: false,
            audio_mode: AudioMode::Disabled,
            use_uart_rx: false,
            sd_dma_ticks_per_word: 1,
            profile: None,
        }
    }
}

// Outcome of a run. `memory` stays alive so SD images can be exported.
pub struct RunResult {
    // Core 0's r1 if a core halted, None if the cycle budget ran out.
    pub result: Option<u32>,
    pub memory: Arc<Memory>,
    // One profile per core in core order when profiling, else empty.
    pub profiles: Vec<CoreProfile>,
}

// Turn-taking state for the rr/random schedulers.
struct SchedulerState {
    next_core: usize,
    done: bool,
    seed: u64,
}

// Hands a single execution turn between cores (not used in free mode).
// Each core waits on its own condition variable so passing the turn wakes
// only the next core instead of every waiting core.
struct Scheduler {
    mode: ScheduleMode,
    cores: usize,
    state: Mutex<SchedulerState>,
    turn: Vec<Condvar>,
}

// Advance a 64-bit LCG and return its high 32 bits.
fn next_rand_u32(seed: &mut u64) -> u32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (*seed >> 32) as u32
}

impl Scheduler {
    // Create a scheduler, or None for free-running cores.
    fn new(mode: ScheduleMode, cores: usize) -> Option<Arc<Scheduler>> {
        if mode == ScheduleMode::Free {
            return None;
        }
        let mut seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let next_core = if mode == ScheduleMode::Random {
            next_rand_u32(&mut seed) as usize % cores
        } else {
            0
        };
        Some(Arc::new(Scheduler {
            mode,
            cores,
            state: Mutex::new(SchedulerState {
                next_core,
                done: false,
                seed,
            }),
            turn: (0..cores).map(|_| Condvar::new()).collect(),
        }))
    }

    // Block until it is `core`'s turn; false once the run is stopping.
    fn wait_turn(&self, core: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        loop {
            if state.done {
                return false;
            }
            if state.next_core == core {
                return true;
            }
            state = self.turn[core].wait(state).unwrap();
        }
    }

    // Pass the turn on after `core` finished its tick.
    fn finish_turn(&self, core: usize) {
        let mut state = self.state.lock().unwrap();
        if state.done {
            return;
        }
        state.next_core = match self.mode {
            ScheduleMode::Random => next_rand_u32(&mut state.seed) as usize % self.cores,
            _ => (core + 1) % self.cores,
        };
        self.turn[state.next_core].notify_one();
    }

    // Stop scheduling and wake every waiting core.
    fn stop(&self) {
        self.state.lock().unwrap().done = true;
        for turn in &self.turn {
            turn.notify_all();
        }
    }
}

// Stop and halt flags shared by all core threads and the graphics window.
struct RunShared {
    // Set when any core halts or exhausts its budget; every loop exits.
    stop: Arc<AtomicBool>,
    // Set when a core executed `mode halt` (vs. running out of cycles).
    halted: AtomicBool,
}

// What one core thread reports when it exits.
struct CoreExit {
    r1: u32,
    profile: Option<CoreProfile>,
}

// Tick one core until a halt, the cycle budget, or another core's stop.
fn run_core(
    mut cpu: Emulator,
    max_cycles: u32,
    scheduler: Option<Arc<Scheduler>>,
    shared: Arc<RunShared>,
) -> CoreExit {
    let core = cpu.core_id as usize;
    cpu.count = 0;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(sched) = &scheduler {
            // Re-check stop after waiting: the run may have ended meanwhile.
            if !sched.wait_turn(core) || shared.stop.load(Ordering::SeqCst) {
                break;
            }
        }
        cpu.tick();
        if cpu.halted {
            shared.halted.store(true, Ordering::SeqCst);
            shared.stop.store(true, Ordering::SeqCst);
            break;
        }
        if max_cycles != 0 && cpu.count > max_cycles {
            shared.stop.store(true, Ordering::SeqCst);
            break;
        }
        if let Some(sched) = &scheduler {
            sched.finish_turn(core);
        }
    }
    if let Some(sched) = &scheduler {
        sched.stop();
    }
    CoreExit {
        r1: cpu.regfile[1],
        profile: cpu.profile.take(),
    }
}

// Load `path`, run it to completion on `config.cores` cores, and return the
// result with the final memory state.
pub fn run_program(
    path: &str,
    config: &RunConfig,
    sd0_image: Option<&[u8]>,
    sd1_image: Option<&[u8]>,
) -> Result<RunResult, String> {
    if !(1..=MAX_CORES).contains(&config.cores) {
        return Err(format!(
            "Run: --cores must be in 1..={}, got {}",
            MAX_CORES, config.cores
        ));
    }
    let image = load_program(path)?;
    let memory = build_memory(
        image.bytes,
        config.use_uart_rx,
        config.sd_dma_ticks_per_word,
        sd0_image,
        sd1_image,
    );
    let interrupts = InterruptController::new(config.cores, config.use_uart_rx);
    let shared = Arc::new(RunShared {
        stop: Arc::new(AtomicBool::new(false)),
        halted: AtomicBool::new(false),
    });
    let scheduler = Scheduler::new(config.sched, config.cores);

    // The window must be created on the main thread before cores start.
    let graphics = config
        .with_graphics
        .then(|| Graphics::new(Arc::clone(&memory)));
    let (audio_mode, audio_output) = AudioPlayback::start(config.audio_mode, Arc::clone(&memory));

    let mut handles = Vec::with_capacity(config.cores);
    for core_id in 0..config.cores {
        let mut cpu = Emulator::from_shared(
            Arc::clone(&memory),
            Arc::clone(&interrupts),
            core_id as u32,
        );
        if core_id == 0 {
            // Wall-clock audio mode drives the device from its own thread.
            cpu.ticks_audio = audio_mode != AudioMode::Fast;
            cpu.audio_sink = audio_output.as_ref().and_then(|out| out.emulated_sink());
        }
        if let Some(window) = &config.profile {
            cpu.profile = Some(CoreProfile::new(core_id as u32, Arc::clone(window)));
        }
        let shared = Arc::clone(&shared);
        let scheduler = scheduler.clone();
        let max_cycles = config.max_cycles;
        handles.push(thread::spawn(move || {
            run_core(cpu, max_cycles, scheduler, shared)
        }));
    }

    if let Some(mut graphics) = graphics {
        graphics.start(Arc::clone(&shared.stop));
    }

    let exits: Vec<CoreExit> = handles
        .into_iter()
        .map(|handle| handle.join().expect("Run: a core thread panicked"))
        .collect();
    drop(audio_output);

    let result = shared
        .halted
        .load(Ordering::SeqCst)
        .then(|| exits[0].r1);
    let profiles = exits.into_iter().filter_map(|exit| exit.profile).collect();
    Ok(RunResult {
        result,
        memory,
        profiles,
    })
}
