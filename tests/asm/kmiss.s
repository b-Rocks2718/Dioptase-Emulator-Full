  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
  jmp _start
_start:

  movi r7, 0xFFFFFFF0
  lwa  r3, [r7] # will fail
  mov  r1, r3
  mode halt

TLB_UMISS:
  movi r1, 10
  mode halt

TLB_KMISS:
  movi r1, 2
  mode halt
