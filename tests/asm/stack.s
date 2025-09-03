  .kernel

EXIT:
  mode halt

_start:
  mov  sp r0
  movi r2 0x123456
  movi r7 0x111111
  push r2
  push r7
  pop  r0
  pop  r3
  sys  EXIT