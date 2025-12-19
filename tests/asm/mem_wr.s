  .kernel

EXIT:
  mode halt

.fill 0x44444444
.fill 0x55555555
.fill 0x66666666
DATA_1: .fill 0xFFFFFFFF
.fill 0x11111111

_start:
  lw   r13, [DATA_1]
  lw   r10, [DATA_2]
  add  r13, r10, r13
  sw   r13, [DATA_2]
  lw   r3,  [r0, DATA_2]
  mov  r1, r3
  sys  EXIT     # should return 0x25 = 37

.space 128
.fill 0x22222222
DATA_2: .fill 38
.fill 0x33333333