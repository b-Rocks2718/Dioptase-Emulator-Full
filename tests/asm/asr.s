EXIT:
  mode halt

  .global _start
_start:
  movi r3 0xAAAAAAAA
  movi r20, 2
  asr  r3 r3 r20
  asr  r3 r3 1
  mov  r1, r3
  sys  EXIT # should return 0xF5555555