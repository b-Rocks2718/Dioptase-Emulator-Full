  # ensure address that causes miss is placed in cr7 (tlb reg)

  .global _start
TLB_KMISS:
  mov  r1, tlb
  mode halt

_start:
  movi r4, 0xFFFFF000
  lwa  r3, [r4] # will fail