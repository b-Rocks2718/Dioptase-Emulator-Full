
  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x3D4 # IVT IPI (0xF5 * 4)
  .fill INT_IPI

  .origin 0x400
  jmp _start
_start:
  # Split execution by core id.
  mov  r1, cid
  add  r2 r1 r0
  bz   core0
  br   core1

core0:
  # Shared addresses: counter @0x1000, done flag @0x1004.
  movi r4, 0x1000
  swa  r0 [r4, 0]
  add  r5 r4 4
  swa  r0 [r5, 0]

  # Wake core1 so both cores perform atomic adds.
  ipi  r6, 1

  # Atomic increment of the shared counter.
  add  r7 r0 1
  fada r8, r7, [r4, 0]

wait_done:
  # Wait for core1 to mark done.
  lwa  r9 [r5, 0]
  add  r9 r9 r0
  bz   wait_done

  # Expect the counter to be 2 with atomic ops.
  lwa  r10 [r4, 0]
  mov  r1, r10
  mode halt

core1:
  # Atomic increment, then signal done.
  movi r4, 0x1000
  add  r5 r4 4
  add  r7 r0 1
  fada r8, r7, [r4, 0]
  add  r9 r0 1
  swa  r9 [r5, 0]
  # Sleep instead of halting the entire system.
  mode sleep

INT_IPI:
  # Clear interrupt and return.
  mov  isr, r0
  rfi
