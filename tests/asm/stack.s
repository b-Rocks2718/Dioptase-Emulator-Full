  .kernel

EXIT:
  mode halt

_start:
  movi sp 0x20000 # keep stack in physical memory for this test
  movi r2 0x123456
  movi r7 0x111111
  push r2
  push r7
  pop  r0
  pop  r1
  sys  EXIT