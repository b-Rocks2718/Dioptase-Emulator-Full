  .kernel
_start:
  movi r1, 1
  movi r2, 2
  sys  EXIT    # should return 3
  add  r1, r1, 1
  mode halt

# as a test, the exit syscall will actually add
EXIT:
  add r1, r1, r2
  rfe
