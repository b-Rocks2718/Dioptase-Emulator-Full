EXIT:
  mode halt

  .global _start
_start:
  movi r2 0x12348000
  tncd r3 r2
  add  r1 r3 1
  sys  EXIT # should return 0x00008001
