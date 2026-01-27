  .global _start
_start:
  movi r1 0x80000000
  add  r0 r0 r0
  bnc  label # this should be taken
  movi r1 0xE
  mode halt
label:
  add  r0 r1 r1
  bnc  label2 # this branch should not be taken
  movi r1 0
  mode halt
label2:
  movi r1 0xF
  mode halt