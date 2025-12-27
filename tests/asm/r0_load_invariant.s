
  .global _start
EXIT:
  mode halt

_start:
  movi r1 0x100
  movi r2 0x12345678
  swa  r2 [r1, 0]   # store a word
  lwa  r0 [r1, 0]   # attempt to load into r0; should be ignored
  add  r3 r0 r0     # r3 should still be zero
  mov  r1, r3
  sys  EXIT
