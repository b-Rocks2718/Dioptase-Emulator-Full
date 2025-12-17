  .kernel
_start:

  movi r7, 0xFFFFFFF0
  lwa  r3, [r7] # will fail
  mode halt

TLB_UMISS:
  movi r3, 10
  mode halt

TLB_KMISS:
  movi r3, 2
  mode halt
