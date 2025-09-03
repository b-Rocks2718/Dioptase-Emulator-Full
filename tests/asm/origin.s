  .kernel

EXIT:
  mode halt

  .origin 0x500
_start:
  movi r3, 21
  sys EXIT