  .global _start
  .origin 0x400
_start:
  movi r1 1
  movi r2 0
  cmp  r1 r2
  bae  ok   # must be taken
  add  r1 r0 1  # should not be executed
  mode halt
ok:
  add  r1 r0 42
  mode halt   # should return 42
