// Interrupt controller shared by all cores: per-core pending bits, IPI
// delivery, and round-robin routing of device interrupts.
//
// Concurrency: `pending` is a lock-free per-core atomic. Any core may set bits for any core; only the owning core
// takes (clears) its own pending bits. Routing cursors live behind `routes`.
// All atomics use SeqCst.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::memory::{AUDIO_INTERRUPT_BIT, SD2_INTERRUPT_BIT, SD_INTERRUPT_BIT, VGA_INTERRUPT_BIT};

// Interrupt lines owned by the CPU side (docs/ISA.md "Interrupts").
pub(super) const TIMER_INTERRUPT_BIT: u32 = 1 << 0;
pub(super) const KB_INTERRUPT_BIT: u32 = 1 << 1;
pub(super) const UART_INTERRUPT_BIT: u32 = 1 << 2;
pub(super) const IPI_INTERRUPT_BIT: u32 = 1 << 5;

// Device interrupts delivered to one core at a time, rotating across cores.
const ROUTED_DEVICE_BITS: [u32; 4] = [
    SD_INTERRUPT_BIT,
    SD2_INTERRUPT_BIT,
    VGA_INTERRUPT_BIT,
    AUDIO_INTERRUPT_BIT,
];

// Trace names for each interrupt line, in bit order.
const INTERRUPT_NAMES: [(u32, &str); 8] = [
    (TIMER_INTERRUPT_BIT, "timer"),
    (KB_INTERRUPT_BIT, "keyboard"),
    (UART_INTERRUPT_BIT, "uart"),
    (SD_INTERRUPT_BIT, "sd0"),
    (SD2_INTERRUPT_BIT, "sd1"),
    (VGA_INTERRUPT_BIT, "vga"),
    (AUDIO_INTERRUPT_BIT, "audio"),
    (IPI_INTERRUPT_BIT, "ipi"),
];

// Format pending interrupt bits as stable names for trace output.
pub(super) fn format_interrupts(bits: u32) -> String {
    let names: Vec<&str> = INTERRUPT_NAMES
        .iter()
        .filter(|(bit, _)| bits & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join("|")
    }
}

// One value per cache line, so a core polling its own slot every tick does
// not share a line with the slots other cores write.
#[repr(align(64))]
struct CacheLine<T>(T);

// Round-robin cursors and the in-flight input interrupt owner.
struct RouteState {
    next_device: [usize; ROUTED_DEVICE_BITS.len()],
    next_input: usize,
    // Core that currently has the input interrupt pending; a new input
    // interrupt is not routed until that core acknowledges it.
    input_inflight: Option<usize>,
}

// Per-core pending-interrupt state and inter-processor interrupt delivery.
// IPIs carry no payload and always succeed; one sent while the target's IPI
// bit is already set merges into it (docs/ISA.md "Inter-processor interrupts").
pub(super) struct InterruptController {
    cores: usize,
    // Keyboard or UART line, fixed for the run by `--uart`.
    input_bit: u32,
    pending: Vec<CacheLine<AtomicU32>>,
    routes: Mutex<RouteState>,
}

impl InterruptController {
    // Create a controller with no pending sources.
    pub(super) fn new(cores: usize, use_uart_rx: bool) -> Arc<InterruptController> {
        Arc::new(InterruptController {
            cores,
            input_bit: if use_uart_rx { UART_INTERRUPT_BIT } else { KB_INTERRUPT_BIT },
            pending: (0..cores).map(|_| CacheLine(AtomicU32::new(0))).collect(),
            routes: Mutex::new(RouteState {
                next_device: [0; ROUTED_DEVICE_BITS.len()],
                next_input: 0,
                input_inflight: None,
            }),
        })
    }

    // Publish pending bits for a core without losing concurrent updates.
    fn set_pending_bits(&self, core: usize, bits: u32) {
        self.pending[core].0.fetch_or(bits, Ordering::SeqCst);
    }

    // Pending bits for a core, without consuming them.
    pub(super) fn peek_pending(&self, core: usize) -> u32 {
        self.pending[core].0.load(Ordering::SeqCst)
    }

    // Atomically consume one core's pending bits. Called every tick, so the
    // common empty case is a plain load rather than a read-modify-write.
    pub(super) fn take_pending(&self, core: usize) -> u32 {
        let slot = &self.pending[core].0;
        if slot.load(Ordering::SeqCst) == 0 {
            return 0;
        }
        slot.swap(0, Ordering::SeqCst)
    }

    // Raise the IPI line on one target. A target past the configured core
    // count does not exist, so the IPI is dropped.
    pub(super) fn send_ipi(&self, target: usize) {
        if target < self.cores {
            self.set_pending_bits(target, IPI_INTERRUPT_BIT);
        }
    }

    // Raise the IPI line on every core, including the sender.
    pub(super) fn send_ipi_all(&self) {
        for core in 0..self.cores {
            self.set_pending_bits(core, IPI_INTERRUPT_BIT);
        }
    }

    // Route the input interrupt to the next core while input is queued and no
    // core already owns an unacknowledged input interrupt.
    pub(super) fn dispatch_input(&self, input_queued: bool) {
        // Every core calls this every tick; skip the lock in the common case.
        if !input_queued {
            return;
        }
        let mut routes = self.routes.lock().unwrap();
        if routes.input_inflight.is_none() {
            let core = routes.next_input;
            routes.next_input = (core + 1) % self.cores;
            routes.input_inflight = Some(core);
            self.set_pending_bits(core, self.input_bit);
        }
    }

    // Deliver each raised device interrupt to the next core in rotation.
    pub(super) fn dispatch_device_interrupts(&self, raised: u32) {
        if raised == 0 {
            return;
        }
        let mut routes = self.routes.lock().unwrap();
        for (slot, bit) in ROUTED_DEVICE_BITS.iter().enumerate() {
            if raised & bit != 0 {
                let core = routes.next_device[slot];
                routes.next_device[slot] = (core + 1) % self.cores;
                self.set_pending_bits(core, *bit);
            }
        }
    }

    // Raise the timer interrupt on every core.
    pub(super) fn broadcast_timer(&self) {
        for core in 0..self.cores {
            self.set_pending_bits(core, TIMER_INTERRUPT_BIT);
        }
    }

    // Record that `core` cleared ISR bits, reopening input routing for that
    // core when the input bit was among the cleared ones.
    pub(super) fn acknowledge(&self, core: usize, cleared_bits: u32) {
        if cleared_bits & self.input_bit != 0 {
            let mut routes = self.routes.lock().unwrap();
            if routes.input_inflight == Some(core) {
                routes.input_inflight = None;
            }
        }
    }
}
