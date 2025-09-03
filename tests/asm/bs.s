  .kernel

EXIT:
  mode halt

_start:
  add  r1 r0 10
  add  r2 r0 11
  add  r3 r0 9
  cmp  r3 r1
  bs   label # this should be taken
  movi r3 0xE
  sys  EXIT
label:
  cmp  r2 r1
  bs   label2 # this branch should not be taken
  cmp  r1 r1
  bs   label3 # this branch should not be taken
  movi r3 2
  sys  EXIT
label2:
  movi r3 0xF
  sys  EXIT
label3:
  movi r3 0xD
  sys  EXIT