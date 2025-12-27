EXIT:
  mode halt

  .global _start
_start:
  movi r1 0x7FFFFFFF
  movi r2 3
  add  r0 r1 r2
  bo   label # this should be taken
  movi r1 0xE
  sys  EXIT
label:
  add  r0 r2 r2
  bc   label2 # this branch should not be taken
  add  r0 r1 r1
  bc   label3 # this branch should not be taken
  movi r1 0
  sys  EXIT
label2:
  movi r1 0xF
  sys  EXIT
label3:
  movi r1 0xD
  sys  EXIT