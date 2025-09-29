  .kernel
_start:
  # set up tlb

  # map 0x0000 to 0x1000
  mov  r4, r0
  lsl  r4, r4, 20
  add  r5, r0, 0xA
  tlbw r5, r4
  tlbr r3, r4

  mode halt
