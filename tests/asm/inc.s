  .kernel

EXIT:
  mode halt

_start:
  mov  r1 r0
  ld   r3 [r1, DATA]
  add  r1 r1 2
  ld   r4 [r1, DATA]
  add  r3 r4 r3
  sys  EXIT # should return 0xFFFF

DATA:
  .fill 0xAAAA5555