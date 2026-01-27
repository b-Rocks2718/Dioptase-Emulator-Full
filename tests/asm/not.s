
  .global _start

  .origin 0x400
  jmp _start
_start:
  movi r3 0xFFFFFFFD
  not  r3 r3
  not  r4, 0
  add  r3, r3, r4
  mov  r1, r3
  mode halt # should return 1
