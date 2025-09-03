  .kernel

EXIT:
  mode halt

_start:
  add  r1 r0 15
  add  r2 r0 35
  and  r3 r2 r1
  and  r3, r3, 14
  # should return 2
  sys  EXIT