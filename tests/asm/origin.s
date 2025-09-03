  .kernel

EXIT:
  mode halt

  .origin 0xF0
_start:
  movi r3, 21
  sys EXIT