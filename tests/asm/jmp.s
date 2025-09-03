  .kernel

EXIT:
  mode halt

_start:
  add  r1 r0 10
  add  r2 r0 11
  add  r3 r0 10
  cmp  r1 r2
  jmp  label # this should be taken
  movi r3 0xE
  sys  EXIT
label:
  cmp  r1 r3
  jmp  label2 # this branch should be taken
  movi r3 0xA
  sys  EXIT
label2:
  movi r3 0
  sys  EXIT