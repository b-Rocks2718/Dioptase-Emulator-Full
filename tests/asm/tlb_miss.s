  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x208 # IVT TLB_MISS (0x82 * 4)
  .fill TLB_MISS

  .origin 0x400
  jmp _start
_start:

  movi r7, 0xFFFFFFF0
  lwa  r3, [r7] # will fail
  mov  r1, r3
  mode halt

TLB_MISS:
  movi r1, 2
  mode halt
