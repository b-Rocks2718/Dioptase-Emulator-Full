
  .global _start
EXIT:
  mode halt

_start:
  add  r4 r0 10
  movi r5 0x42424242
  sba  r5 [r4, 91] # store at address 101
  lba  r3 [r0, 101]
  mov  r1, r3
  sys  EXIT     # should return 0x42
