  .kernel

EXIT:
  mode halt

_start:
  movi r3 0xAAAAAAAA
  movi r20, 2
  asr  r3 r3 r20
  asr  r3 r3 1
  sys  EXIT # should return 0xF5555555