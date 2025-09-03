  .kernel

EXIT:
  mode halt

_start:
  movi r3 0x5555
  movi r1, 2
  lsr  r3 r3 r1
  lsr  r3 r3 1
  sys  EXIT # should return 0xAAA