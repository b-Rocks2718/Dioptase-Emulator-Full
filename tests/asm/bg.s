  .global _start
_start:
  movi r1 0x8FFFFFFF
  movi r2 3
  cmp  r2 r1
  bg   label # this should be taken
  movi r1 0xE
  mode halt
label:
  cmp  r1 r2
  bg   label2 # this branch should not be taken
  cmp  r0 r0
  bg   label3 # this branch should not be taken
  movi r1 1
  mode halt
label2:
  movi r1 0xF
  mode halt
label3:
  movi r1 0xD
  mode halt