  .kernel

EXIT:
  mode halt

_start:
  movi r3 0xFFFFFFFD
  not  r3 r3
  not  r4, 0
  add  r3, r3, r4
  sys  EXIT # should return 1