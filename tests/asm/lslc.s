
  .global _start
EXIT:
  mode halt

_start:
  movi r4 0xFAAAAAAA
  movi r3 0x00000050
  movi r15 1
  lsl  r4 r4 r15
  lslc r3 r3 r15
  lsl  r4 r4 1
  lslc r3 r3 1
  mov  r1, r3
  sys  EXIT # should return 0x0143