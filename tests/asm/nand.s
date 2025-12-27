
  .global _start
EXIT:
  mode halt

_start:
  add  r1 r0 10 # 0b1010
  add  r2 r0 6  # 0b0110
  nand r3 r1 r2 # 0b1111 1111 1111 1101
  nand r3, r3, 5
  mov  r1, r3
  sys  EXIT # should return -6