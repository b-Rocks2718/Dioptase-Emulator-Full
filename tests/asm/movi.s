  .kernel

EXIT:
  mode halt

_start:
  movi r3 0xABABABAB
  sys  EXIT # should return 0xABABABAB