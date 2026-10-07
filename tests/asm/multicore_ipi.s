  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x3D4 # IVT IPI (0xF5 * 4)
  .fill INT_IPI

  .origin 0x400
  jmp _start
_start:
  # Split execution by core id.
  mov  r1, cid
  cmp  r1 r0
  bz   core0
  br   core1

core0:
  # IPIs carry no payload, so publish 0x42 in memory before interrupting core1.
  add  r2 r0 0x42
  movi r4, 0x1004
  swa  r2 [r4, 0]
  ipi  1

  # Wait for core1 to copy the value to 0x1000.
  movi r4, 0x1000
wait_flag:
  lwa  r5 [r4, 0]
  add  r5 r5 r0
  bz   wait_flag
  mov  r1, r5
  mode halt

core1:
  # Stay asleep until the IPI wakes us.
  mode sleep

INT_IPI:
  # Copy the published value to memory and return from interrupt.
  movi r3, 0x1004
  lwa  r2 [r3, 0]
  movi r3, 0x1000
  swa  r2 [r3, 0]
  eoi 5
  rfe
