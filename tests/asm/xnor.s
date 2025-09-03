  .kernel

EXIT:
  mode halt

_start:
  add  r1 r0 15
  add  r2 r0 18
  xnor r3 r2 r1
  xnor r3 r3 16 
  # should return 13
  sys  EXIT