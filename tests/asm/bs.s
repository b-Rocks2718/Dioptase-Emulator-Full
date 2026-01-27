  .global _start
_start:
  add  r1 r0 10
  add  r2 r0 11
  add  r3 r0 9
  cmp  r3 r1
  bs   label # this should be taken
  movi r1 0xE
  mode halt
label:
  cmp  r2 r1
  bs   label2 # this branch should not be taken
  cmp  r1 r1
  bs   label3 # this branch should not be taken
  movi r1 2
  mode halt
label2:
  movi r1 0xF
  mode halt
label3:
  movi r1 0xD
  mode halt