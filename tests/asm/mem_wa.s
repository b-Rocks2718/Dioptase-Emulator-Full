
  .global _start
EXIT:
  mode halt

_start:
  add  r4 r0 0x10
  movi r5 0x42424242
  swa  r5 [r4, 0x3FE0] # store at address 0x3FF0
  lwa  r3 [r0, 0x3FF0]
  mov  r1, r3
  sys  EXIT     # should return 0x42424242
