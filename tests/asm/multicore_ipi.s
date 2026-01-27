
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
  # Send payload 0x42 to core1 via IPI.
  add  r2 r0 0x42
  mov  mbo, r2
  ipi  r3, 1

  # Wait for core1 to publish the payload at 0x1000.
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
  # Copy IPI payload to memory and return from interrupt.
  mov  r2, mbi
  movi r3, 0x1000
  swa  r2 [r3, 0]
  mov  isr, r0
  rfi
