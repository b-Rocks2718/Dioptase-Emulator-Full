  .global _start
  .origin 0x400
  jmp _start
_start:
  movi r1 0x80000000
  add  r0 r1 r1
  bc   label # this should be taken
  movi r1 0xE
  mode halt
label:
  add  r0 r0 r0
  bc   label2 # this branch should not be taken
  movi r1 1
  mode halt
label2:
  movi r1 0xF
  mode halt
