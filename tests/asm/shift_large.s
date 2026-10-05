  # Register shift amounts past 31: logical shifts produce 0 and rotates
  # use the amount modulo 32 (these used to crash debug builds).
  .global _start
  .origin 0x400
  jmp _start
_start:
  movi r4 1
  movi r5 40
  lsl  r2 r4 r5   # 0
  movi r5 33
  rotl r3 r4 r5   # rotate by 1 -> 2
  add  r1 r2 r3
  mode halt # should return 2
