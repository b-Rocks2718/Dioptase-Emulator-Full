
  .global _start
EXIT:
  mode halt

_start:
  add  r4 r0 10
  movi r5 0x42424242
  sda  r5 [r4, 90] # store at address 100
  lda  r3 [r0, 100]
  mov  r1, r3
  sys  EXIT     # should return 04242
