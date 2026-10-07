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
pub(super) const MOUSE_INTERRUPT_BIT: u32 = 1 << 8;

// Device interrupts delivered to one core at a time, rotating across cores.
const ROUTED_DEVICE_BITS: [u32; 4] = [
    SD_INTERRUPT_BIT,
    SD2_INTERRUPT_BIT,
    VGA_INTERRUPT_BIT,
    AUDIO_INTERRUPT_BIT,
];

// Trace names for each interrupt line, in bit order.
const INTERRUPT_NAMES: [(u32, &str); 9] = [
    (TIMER_INTERRUPT_BIT, "timer"),
    (KB_INTERRUPT_BIT, "keyboard"),
    (UART_INTERRUPT_BIT, "uart"),
    (SD_INTERRUPT_BIT, "sd0"),
    (SD2_INTERRUPT_BIT, "sd1"),
    (VGA_INTERRUPT_BIT, "vga"),
    (AUDIO_INTERRUPT_BIT, "audio"),
    (IPI_INTERRUPT_BIT, "ipi"),
    (MOUSE_INTERRUPT_BIT, "mouse"),
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

// Queued input sources (keyboard/UART stream, then mouse stream), indexed
// into `RouteState::inputs`.
const KEYBOARD_INPUT: usize = 0;
const MOUSE_INPUT: usize = 1;
const INPUT_SOURCES: usize = 2;

// Routing state for one queue-backed input line. The line stays asserted
// while its queue is non-empty, but is delivered to one core at a time.
#[derive(Clone, Copy)]
struct InputRoute {
    next: usize,
    // Core that currently has this line pending; the line is not routed
    // again until that core clears the ISR bit.
    inflight: Option<usize>,
}

// Round-robin cursors and the in-flight input interrupt owners.
struct RouteState {
    next_device: [usize; ROUTED_DEVICE_BITS.len()],
    inputs: [InputRoute; INPUT_SOURCES],
}

// Per-core pending-interrupt state and inter-processor interrupt delivery.
// IPIs carry no payload and always succeed; one sent while the target's IPI
// bit is already set merges into it (docs/ISA.md "Inter-processor interrupts").
pub(super) struct InterruptController {
    cores: usize,
    // ISR bit for each queued input source. The keyboard slot is the
    // keyboard or UART line, fixed for the run by `--uart`.
    input_bits: [u32; INPUT_SOURCES],
    pending: Vec<CacheLine<AtomicU32>>,
    routes: Mutex<RouteState>,
}

impl InterruptController {
    // Create a controller with no pending sources.
    pub(super) fn new(cores: usize, use_uart_rx: bool) -> Arc<InterruptController> {
        Arc::new(InterruptController {
            cores,
            input_bits: [
                if use_uart_rx { UART_INTERRUPT_BIT } else { KB_INTERRUPT_BIT },
                MOUSE_INTERRUPT_BIT,
            ],
            pending: (0..cores).map(|_| CacheLine(AtomicU32::new(0))).collect(),
            routes: Mutex::new(RouteState {
                next_device: [0; ROUTED_DEVICE_BITS.len()],
                inputs: [InputRoute { next: 0, inflight: None }; INPUT_SOURCES],
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

    // Route each queued input line (keyboard/UART, mouse) to the next core
    // while its queue is non-empty and no core already owns an
    // unacknowledged interrupt for that line.
    pub(super) fn dispatch_input(&self, keyboard_queued: bool, mouse_queued: bool) {
        // Every core calls this every tick; skip the lock in the common case.
        if !keyboard_queued && !mouse_queued {
            return;
        }
        let queued = [keyboard_queued, mouse_queued];
        let mut routes = self.routes.lock().unwrap();
        for source in [KEYBOARD_INPUT, MOUSE_INPUT] {
            let route = &mut routes.inputs[source];
            if queued[source] && route.inflight.is_none() {
                let core = route.next;
                route.next = (core + 1) % self.cores;
                route.inflight = Some(core);
                self.set_pending_bits(core, self.input_bits[source]);
            }
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

    // Record that `core` cleared ISR bits, reopening routing for each input
    // line whose bit was among the cleared ones and that this core owned.
    pub(super) fn acknowledge(&self, core: usize, cleared_bits: u32) {
        if cleared_bits & (self.input_bits[KEYBOARD_INPUT] | self.input_bits[MOUSE_INPUT]) == 0 {
            return;
        }
        let mut routes = self.routes.lock().unwrap();
        for source in [KEYBOARD_INPUT, MOUSE_INPUT] {
            let route = &mut routes.inputs[source];
            if cleared_bits & self.input_bits[source] != 0 && route.inflight == Some(core) {
                route.inflight = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keyboard and mouse are independent queued lines: each is routed to one
    // core at a time and re-routed (round-robin) only after that core clears
    // its own bit.
    #[test]
    fn mouse_and_keyboard_lines_route_independently() {
        let controller = InterruptController::new(2, false);

        controller.dispatch_input(true, true);
        assert_eq!(controller.take_pending(0), KB_INTERRUPT_BIT | MOUSE_INTERRUPT_BIT);
        controller.dispatch_input(true, true);
        assert_eq!(controller.take_pending(1), 0);

        controller.acknowledge(0, MOUSE_INTERRUPT_BIT);
        controller.acknowledge(1, KB_INTERRUPT_BIT);
        controller.dispatch_input(true, true);
        assert_eq!(controller.take_pending(0), 0);
        assert_eq!(controller.take_pending(1), MOUSE_INTERRUPT_BIT);

        controller.acknowledge(0, KB_INTERRUPT_BIT);
        controller.dispatch_input(true, true);
        assert_eq!(controller.take_pending(1), KB_INTERRUPT_BIT);
    }

    // `--uart` moves the keyboard line to UART RX but leaves the mouse alone.
    #[test]
    fn uart_mode_keeps_mouse_line() {
        let controller = InterruptController::new(1, true);
        controller.dispatch_input(true, true);
        assert_eq!(controller.take_pending(0), UART_INTERRUPT_BIT | MOUSE_INTERRUPT_BIT);
    }

    // Line 8 vectors through IVT[0xF8] and needs a trace name.
    #[test]
    fn mouse_line_is_named() {
        assert_eq!(MOUSE_INTERRUPT_BIT.trailing_zeros(), 8);
        assert_eq!(format_interrupts(MOUSE_INTERRUPT_BIT | KB_INTERRUPT_BIT), "keyboard|mouse");
    }
}
