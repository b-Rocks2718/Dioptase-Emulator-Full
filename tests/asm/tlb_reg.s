  # ensure address that causes miss is placed in cr7 (tlb reg)

  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x208 # IVT TLB_MISS (0x82 * 4)
  .fill TLB_MISS

  .origin 0x400
  jmp _start
TLB_MISS:
  mov  r1, tlba
  mode halt

_start:
  movi r4, 0xFFFFF000
  lwa  r3, [r4] # will fail
