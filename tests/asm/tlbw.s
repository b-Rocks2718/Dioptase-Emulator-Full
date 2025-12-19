  .kernel
_start:
  # set pid to 1
  movi r4, 1
  mov  pid, r4

  # set up tlb

  # map 0x0000 to 0x1000
  lsl  r4, r4, 20
  add  r5, r0, 1
  tlbw r5, r4

  # enter user mode
  mov epc, r0
  rfe
  
EXIT:
  # change process to one with no tlb entries
  # should cause a tlb umiss
  mov  r5, pid
  add  r5, r5, 1
  mov  pid, r5
  mov  epc, r0
  rfe

TLB_UMISS:
  add  r3, r3, 1
  mode halt

TLB_KMISS:
  movi r3, 2
  mode halt

# user mode
  .origin 0x1000
userland:
  movi r3, 0x42
  sys  EXIT