  .kernel

EXIT:
  mode halt

_start:
  movi r1 0x80000000
  movi r2 1
  sub  r4 r1 r2       # 0x80000000 - 1 = 0x7FFFFFFF with overflow set
  bo   overflow       # should be taken
  movi r3 0xBAD       # failure path if overflow was not set
  sys  EXIT
overflow:
  movi r3 1
  sys  EXIT
