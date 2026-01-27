
  .global _start

  .origin 0x400
  jmp _start
DATA_1: .fill 0x22222222

_start:
  ld   r13, [DATA_1]
  sd   r13, [DATA_2]
  lw   r3,  [DATA_2]
  add  r3, r13, r3
  mov  r1, r3
  mode halt     # should return 0x11114444

.space 1108
DATA_2: .fill 0x11111111
