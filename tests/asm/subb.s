  .kernel

EXIT:
  mode halt

_start:
  mov  r4 r0
  mov  r5 r0 # 64 bit integer 0x0000000000000000 stored in r4 + r5
  add  r6 r0 1
  mov  r7 r0 # 64 bit integer 0x0000000000000001 stored in r6 + r7
  # add and store result in r2 + r3
  sub  r2 r4 r6
  subb r3 r5 r7
  # result should be 0xFFFFFFFFFFFFFFFF
  # r3 should have 0xFFFFFFFF
  sys  EXIT