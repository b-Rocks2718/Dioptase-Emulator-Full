  .kernel

EXIT:
  mode halt

_start:
  movi r4 0xFFFFFFFF
  movi r5 0xAAAAAAAA # 64 bit integer 0xAAAAFFFF stored in r4 + r5
  movi r6 0xFFFFFFFF
  movi r7 0x00000001 # 64 bit integer 0x0001FFFF stored in r6 + r7
  # add and store result in r2 + r3
  add  r2 r4 r6
  addc r3 r5 r7
  addc r3 r3 1
  # result should be 0xAAAAAAACFFFFFFFE
  # r1 should have 0xAAAAAAAD
  mov  r1, r3
  sys  EXIT