// Guest-visible PS/2 mouse event queue (docs/mem_map.md "PS/2 mouse input
// stream"). The host window pushes button state and motion here; guest MMIO
// reads peek and pop the oldest encoded event.
//
// Not thread-safe on its own: `Memory` wraps it in a mutex.

use std::collections::VecDeque;

// Event word layout (docs/mem_map.md). Bits [3:0] mirror byte 0 of a PS/2
// mouse packet; bit 3 is always set so an event never reads as 0 (empty).
pub const MOUSE_BUTTON_LEFT: u8 = 1 << 0;
pub const MOUSE_BUTTON_RIGHT: u8 = 1 << 1;
pub const MOUSE_BUTTON_MIDDLE: u8 = 1 << 2;
const MOUSE_BUTTON_MASK: u8 = MOUSE_BUTTON_LEFT | MOUSE_BUTTON_RIGHT | MOUSE_BUTTON_MIDDLE;
const MOUSE_EVENT_VALID: u32 = 1 << 3;
const MOUSE_DX_SHIFT: u32 = 8;
const MOUSE_DY_SHIFT: u32 = 16;
const MOUSE_WHEEL_SHIFT: u32 = 24;

// Implementation-defined queue depth. Merging keeps continuous motion from
// filling it, so this mainly bounds a guest that never drains the stream
// (e.g. a kernel without a mouse driver).
pub const MOUSE_QUEUE_CAPACITY: usize = 64;

// Pack one event into the guest word.
pub fn encode_mouse_event(buttons: u8, dx: i8, dy: i8, wheel: i8) -> u32 {
    (buttons & MOUSE_BUTTON_MASK) as u32
        | MOUSE_EVENT_VALID
        | (dx as u8 as u32) << MOUSE_DX_SHIFT
        | (dy as u8 as u32) << MOUSE_DY_SHIFT
        | (wheel as u8 as u32) << MOUSE_WHEEL_SHIFT
}

// One queued event before encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MouseEvent {
    buttons: u8,
    dx: i8,
    dy: i8,
    wheel: i8,
    // True when this event changed the button state relative to the event
    // queued before it. Such events are never merged into, so motion that
    // happened after a press is never reported as happening before it.
    button_change: bool,
}

impl MouseEvent {
    // Guest word for this event.
    fn encode(&self) -> u32 {
        encode_mouse_event(self.buttons, self.dx, self.dy, self.wheel)
    }
}

// Clamp one axis' remaining motion to the signed-byte range of a single event.
fn take_chunk(remaining: &mut i32) -> i8 {
    let chunk = (*remaining).clamp(i8::MIN as i32, i8::MAX as i32);
    *remaining -= chunk;
    chunk as i8
}

// Bounded FIFO of mouse events with the merge rules from docs/mem_map.md.
//
// Merge rule: a motion-only event (same buttons as the previously accepted
// event) may be added into the newest queued event when that event is also
// motion-only, is not the oldest event, and the sums fit in a signed byte.
// The oldest event is excluded because the guest may be partway through a
// 32-bit read of it (byte 0 peeks, byte 3 pops); changing it between those
// byte reads would tear the event. Only the guest pops, so the oldest
// event's bytes are stable until byte 3 is read.
#[derive(Default)]
pub struct MouseQueue {
    events: VecDeque<MouseEvent>,
    // Buttons of the most recently accepted event (initially all released).
    // Not updated by discarded events, so the next accepted event after an
    // overflow is classified against what the guest will actually see.
    last_buttons: u8,
}

impl MouseQueue {
    // Empty queue with all buttons released (device reset state).
    pub fn new() -> MouseQueue {
        MouseQueue { events: VecDeque::new(), last_buttons: 0 }
    }

    // Whether no event is waiting for the guest.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    // Queue the current button state plus accumulated motion. Motion beyond
    // a signed byte is split across several events (all carrying `buttons`).
    // A call with unchanged buttons and no motion queues nothing.
    pub fn push(&mut self, buttons: u8, mut dx: i32, mut dy: i32, mut wheel: i32) {
        let buttons = buttons & MOUSE_BUTTON_MASK;
        loop {
            let button_change = buttons != self.last_buttons;
            if !button_change && dx == 0 && dy == 0 && wheel == 0 {
                return;
            }
            let event = MouseEvent {
                buttons,
                dx: take_chunk(&mut dx),
                dy: take_chunk(&mut dy),
                wheel: take_chunk(&mut wheel),
                button_change,
            };
            if !button_change && self.try_merge(&event) {
                continue;
            }
            if self.events.len() >= MOUSE_QUEUE_CAPACITY {
                // Discard everything that does not fit; see `last_buttons`.
                return;
            }
            self.events.push_back(event);
            self.last_buttons = buttons;
        }
    }

    // Add a motion-only event into the newest queued event if the merge rule
    // allows it.
    fn try_merge(&mut self, event: &MouseEvent) -> bool {
        if self.events.len() < 2 {
            return false;
        }
        let tail = self.events.back_mut().unwrap();
        if tail.button_change || tail.buttons != event.buttons {
            return false;
        }
        let (Some(dx), Some(dy), Some(wheel)) = (
            tail.dx.checked_add(event.dx),
            tail.dy.checked_add(event.dy),
            tail.wheel.checked_add(event.wheel),
        ) else {
            return false;
        };
        tail.dx = dx;
        tail.dy = dy;
        tail.wheel = wheel;
        true
    }

    // Byte `offset` (0..=3) of the oldest event's word, or 0 when empty.
    pub fn peek_byte(&self, offset: u32) -> u8 {
        self.events.front().map_or(0, |event| (event.encode() >> (8 * offset)) as u8)
    }

    // Remove and return the oldest event's word (0 when empty).
    pub fn pop(&mut self) -> u32 {
        self.events.pop_front().map_or(0, |event| event.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pop every queued event word, oldest first.
    fn drain(queue: &mut MouseQueue) -> Vec<u32> {
        std::iter::from_fn(|| Some(queue.pop()).filter(|&word| word != 0)).collect()
    }

    // Field positions and sign encoding follow docs/mem_map.md.
    #[test]
    fn encoding_matches_mem_map_layout() {
        assert_eq!(encode_mouse_event(0, 0, 0, 0), 0x0000_0008);
        assert_eq!(encode_mouse_event(MOUSE_BUTTON_LEFT | MOUSE_BUTTON_MIDDLE, 1, -1, -2), 0xFEFF_010D);
        assert_eq!(encode_mouse_event(MOUSE_BUTTON_RIGHT, -128, 127, 3), 0x037F_800A);
    }

    // A release with no motion must still be visible as a nonzero event.
    #[test]
    fn release_without_motion_is_nonzero() {
        let mut queue = MouseQueue::new();
        queue.push(MOUSE_BUTTON_LEFT, 0, 0, 0);
        queue.push(0, 0, 0, 0);
        assert_eq!(drain(&mut queue), vec![0x0000_0009, 0x0000_0008]);
    }

    // Pushing the unchanged state with no motion is not an event.
    #[test]
    fn no_change_queues_nothing() {
        let mut queue = MouseQueue::new();
        queue.push(0, 0, 0, 0);
        assert!(queue.is_empty());
    }

    // Motion beyond a signed byte is split, never clamped away.
    #[test]
    fn large_motion_splits_into_byte_sized_events() {
        let mut queue = MouseQueue::new();
        queue.push(0, 300, -200, 0);
        assert_eq!(
            drain(&mut queue),
            vec![
                encode_mouse_event(0, 127, -128, 0),
                encode_mouse_event(0, 127, -72, 0),
                encode_mouse_event(0, 46, 0, 0),
            ]
        );
    }

    // The oldest event may be mid-read by the guest, so motion merges only
    // into the second and later events.
    #[test]
    fn motion_merges_into_tail_but_not_front() {
        let mut queue = MouseQueue::new();
        queue.push(0, 1, 0, 0);
        queue.push(0, 2, 0, 0);
        queue.push(0, 3, 4, 1);
        assert_eq!(drain(&mut queue), vec![encode_mouse_event(0, 1, 0, 0), encode_mouse_event(0, 5, 4, 1)]);
    }

    // Motion after a press must not be folded into the press event, and a
    // button change must never be folded into earlier motion.
    #[test]
    fn button_changes_are_never_merged() {
        let mut queue = MouseQueue::new();
        queue.push(0, 1, 0, 0);
        queue.push(0, 1, 0, 0);
        queue.push(MOUSE_BUTTON_LEFT, 0, 0, 0);
        queue.push(MOUSE_BUTTON_LEFT, 2, 0, 0);
        queue.push(MOUSE_BUTTON_LEFT, 3, 0, 0);
        queue.push(0, 0, 0, 0);
        assert_eq!(
            drain(&mut queue),
            vec![
                encode_mouse_event(0, 1, 0, 0),
                encode_mouse_event(0, 1, 0, 0),
                encode_mouse_event(MOUSE_BUTTON_LEFT, 0, 0, 0),
                encode_mouse_event(MOUSE_BUTTON_LEFT, 5, 0, 0),
                encode_mouse_event(0, 0, 0, 0),
            ]
        );
    }

    // Merging must not wrap a signed byte.
    #[test]
    fn merge_that_would_overflow_queues_new_event() {
        let mut queue = MouseQueue::new();
        queue.push(0, 1, 0, 0);
        queue.push(0, 100, 0, 0);
        queue.push(0, 100, 0, 0);
        assert_eq!(
            drain(&mut queue),
            vec![encode_mouse_event(0, 1, 0, 0), encode_mouse_event(0, 100, 0, 0), encode_mouse_event(0, 100, 0, 0)]
        );
    }

    // Overflow drops new events; the next accepted event carries the true
    // button state, so a dropped release is recovered.
    #[test]
    fn full_queue_drops_new_events_and_resynchronizes_buttons() {
        let mut queue = MouseQueue::new();
        for i in 0..MOUSE_QUEUE_CAPACITY {
            queue.push(if i % 2 == 0 { MOUSE_BUTTON_LEFT } else { 0 }, 0, 0, 0);
        }
        queue.push(MOUSE_BUTTON_RIGHT, 0, 0, 0);
        queue.pop();
        queue.push(MOUSE_BUTTON_RIGHT, 0, 0, 0);

        let events = drain(&mut queue);
        assert_eq!(events.len(), MOUSE_QUEUE_CAPACITY);
        assert_eq!(*events.last().unwrap(), encode_mouse_event(MOUSE_BUTTON_RIGHT, 0, 0, 0));
    }

    // Bytes 0..3 peek; only pop consumes.
    #[test]
    fn peek_reads_oldest_without_consuming() {
        let mut queue = MouseQueue::new();
        assert_eq!(queue.peek_byte(0), 0);
        queue.push(MOUSE_BUTTON_LEFT, -1, 2, 3);
        let word = encode_mouse_event(MOUSE_BUTTON_LEFT, -1, 2, 3);
        for offset in 0..4 {
            assert_eq!(queue.peek_byte(offset), (word >> (8 * offset)) as u8);
        }
        assert_eq!(queue.pop(), word);
        assert!(queue.is_empty());
    }
}
