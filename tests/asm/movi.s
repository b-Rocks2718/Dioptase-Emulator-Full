  .kernel

EXIT:
  mode halt

_start:
  movi r3 0xABABABAB
  mov  r1, r3
  sys  EXIT # should return 0xABABABAB