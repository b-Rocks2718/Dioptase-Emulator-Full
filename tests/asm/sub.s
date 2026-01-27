
  .global _start

  .origin 0x400
  jmp _start
_start:
  add  r5 r0 12
  add  r7 r0 19
  sub  r3 r5 r7
  sub  r3 r3 1
  mov  r1, r3
  mode halt # should return 8
