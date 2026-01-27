  .global _start
  .origin 0x400
  jmp _start
_start:
  movi r2 0x12348000
  tncd r3 r2
  add  r1 r3 1
  mode halt # should return 0x00008001
