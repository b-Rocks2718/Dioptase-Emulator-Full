
  .global _start

_start:
  add  r4 r0 10
  add  r1 r0 12
  or   r3 r1 r4 
  or   r3 r3 1 
  or   r3 r3 0xF0000000 # should be 0xF000000F
  mov  r1, r3
  mode halt