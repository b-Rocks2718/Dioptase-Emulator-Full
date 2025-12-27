EXIT:
  mode halt

  .global _start
_start:
  movi r1 0x80000000
  add  r0 r1 r1
  bc   label # this should be taken
  movi r1 0xE
  sys  EXIT
label:
  add  r0 r0 r0
  bc   label2 # this branch should not be taken
  movi r1 1
  sys  EXIT
label2:
  movi r1 0xF
  sys  EXIT