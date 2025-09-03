  .kernel

EXIT:
  mode halt

_start:
  add  r1 r0 15
  add  r2 r0 18
  xor  r3 r2 r1
  xor  r3 r3 4 # should return 11001 = 25
  sys  EXIT