  .kernel

EXIT:
  mode halt

_start:
  lui  r6 0xAA000000
  mov  r6 r6
  mov  r3 r6
  sys  EXIT # should return 0xAA000000
  