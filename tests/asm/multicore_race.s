
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
  # Shared addresses: counter @0x1000, ready0 @0x1004, ready1 @0x1008.
  movi r4, 0x1000
  swa  r0 [r4, 0]
  add  r5 r4 4
  swa  r0 [r5, 0]
  add  r6 r4 8
  swa  r0 [r6, 0]

  # Wake core1 so both cores race on the counter update.
  ipi  r7, 1

  # Load counter, then signal ready0.
  lwa  r8 [r4, 0]
  add  r9 r0 1
  swa  r9 [r5, 0]
wait_ready1:
  # Wait for core1 to signal ready1.
  lwa  r10 [r6, 0]
  add  r10 r10 r0
  bz   wait_ready1
  # Non-atomic increment; should lose an update and end at 1.
  add  r8 r8 1
  swa  r8 [r4, 0]
  mov  r1, r8
  mode halt

core1:
  # Load counter, then signal ready1.
  movi r4, 0x1000
  add  r5 r4 4
  add  r6 r4 8
  lwa  r8 [r4, 0]
  add  r9 r0 1
  swa  r9 [r6, 0]
wait_ready0:
  # Wait for core0 to signal ready0.
  lwa  r10 [r5, 0]
  add  r10 r10 r0
  bz   wait_ready0
  # Non-atomic increment races with core0.
  add  r8 r8 1
  swa  r8 [r4, 0]
  # Sleep instead of halting the entire system.
  mode sleep

INT_IPI:
  # Clear interrupt and return.
  mov  isr, r0
  rfi
