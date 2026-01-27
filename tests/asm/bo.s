  .global _start
_start:
  movi r1 0x7FFFFFFF
  movi r2 3
  add  r0 r1 r2
  bo   label # this should be taken
  movi r1 0xE
  mode halt
label:
  add  r0 r2 r2
  bc   label2 # this branch should not be taken
  add  r0 r1 r1
  bc   label3 # this branch should not be taken
  movi r1 0
  mode halt
label2:
  movi r1 0xF
  mode halt
label3:
  movi r1 0xD
  mode halt