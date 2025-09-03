  .kernel

EXIT:
  mode halt

_start:
  movi r3 0xAAAA
  movi r1, 2
  lsl  r3 r3 r1
  lsl  r3 r3 1
  sys  EXIT # should return 0x55550