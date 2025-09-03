  .kernel

EXIT:
  mode halt

_start:
  add  r4 r0 10
  add  r1 r0 12
  nor  r3 r1 r4 
  nor  r3 r3 8 # should be 6
  sys  EXIT