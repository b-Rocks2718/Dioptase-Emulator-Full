  .kernel

EXIT:
  mode halt

_start:
  movi r2, 14
  movi r31, 3
  mul  r3, r2, r31
  mov  r1, r3
  sys EXIT