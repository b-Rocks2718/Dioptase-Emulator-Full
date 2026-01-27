  .global _start
_start:
  call far_label
  mode halt # should return 42

  mode halt
  mode halt
  mode halt

far_label:
  add  r3 r0 21
  add  r3 r3 21
  mov  r1, r3
  ret  

  mode halt
  mode halt
  mode halt