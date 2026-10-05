  # asr by 0 must leave a negative value unchanged (it used to return -1).
  .global _start
  .origin 0x400
  jmp _start
_start:
  movi r3 0x80000010
  asr  r1 r3 0
  mode halt # should return 0x80000010
