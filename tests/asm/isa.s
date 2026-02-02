  .origin 0x400
_start:
  movi r10, 0x8000
  mov  isa, r10
  movi r1, 42
  sisa r1, [4]
  lisa r2, [4]
  lwa  r3, [r10, 4]
  add  r2, r2, r3
  add  r1, r1, r2
  mode halt
  