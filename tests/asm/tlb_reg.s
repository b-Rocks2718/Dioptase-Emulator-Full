  # ensure address that causes miss is placed in cr7 (tlb reg)

  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
TLB_KMISS:
  mov  r1, tlb
  mode halt

_start:
  movi r4, 0xFFFFF000
  lwa  r3, [r4] # will fail
