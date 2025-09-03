  .kernel

EXIT:
  mode halt

_start:
  add  r5 r0 10
  add  r7 r0 11
  add  r3 r5 r7
  add  r3 r3 r3
  add  r3 r3 -4
  sys  EXIT     # should return 38
