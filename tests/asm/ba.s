EXIT:
  mode halt

  .global _start
_start:
  movi r1 0x8FFF
  movi r2 3
  cmp  r1 r2
  ba   label # this should be taken
  movi r1 0xA
  sys  EXIT
label:
  cmp  r2 r1
  ba   label2 # this branch should not be taken
  cmp  r0 r0
  ba   label3 # this branch should not be taken
  movi r1 1
  sys  EXIT
label2:
  movi r1 0xF
  sys  EXIT
label3:
  movi r1 0xD
  sys  EXIT