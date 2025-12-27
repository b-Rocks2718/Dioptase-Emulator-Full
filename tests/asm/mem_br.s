
  .global _start
EXIT:
  mode halt

DATA_1: .fill 0x22222222

_start:
  lb   r13, [DATA_1]
  sb   r13, [DATA_2]
  lw   r3,  [DATA_2]
  add  r3, r13, r3
  mov  r1, r3
  sys  EXIT     # should return 0x11111144

.space 277
DATA_2: .fill 0x11111111