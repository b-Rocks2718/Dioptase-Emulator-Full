  .global _start
  .origin 0x400
  jmp _start
_start:
  movi r1 0x8FFF
  movi r2 3
  cmp  r2 r1
  bbe  label # this should be taken
  movi r1 0xA
  mode halt
label:
  cmp  r1 r2
  bbe  label2 # this branch should not be taken
  cmp  r0 r0
  bbe  label3 # this branch should be taken
  movi r1 0xD
  mode halt
label2:
  movi r1 0xF
  mode halt
label3:
  movi r1 1
  mode halt
