
  .global _start

  .origin 0x400
  jmp _start
_start:
  movi r3 0xABABABAB
  mov  r1, r3
  mode halt # should return 0xABABABAB
