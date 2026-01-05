EXIT:
  mode halt

  .global _start
_start:
  movi r2 0xFFFFFF7F
  movi r3 0x00000080
  sxtb r4 r2
  sxtb r5 r3
  sub  r1 r4 r5
  sys  EXIT # should return 0x000000FF
