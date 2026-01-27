  .global _start
  .origin 0x400
  jmp _start
_start:
  add  r1 r0 10
  add  r2 r0 11
  add  r3 r0 10
  cmp  r1 r2
  jmp  label # this should be taken
  movi r1 0xE
  mode halt
label:
  cmp  r1 r3
  jmp  label2 # this branch should be taken
  movi r1 0xA
  mode halt
label2:
  movi r1 0
  mode halt
