  .kernel
_start:
  # set pid to 1
  movi r4, 1
  mov  pid, r4

  # set up tlb

  # map 0x1000 to 0x2000
  movi r2, 0x2019 # PPN=0x2000, global, user, not executable, not writable, readable
  movi r1, 0x1000
  tlbw r2, r1

  # map 0x2000 to 0x3000
  movi r2, 0x3009 # PPN=0x3000, not global, user, not executable, not writable, readable
  movi r1, 0x2000
  tlbw r2, r1

  # set pid to 2
  movi r4, 2
  mov pid, r4

  # map 0x0000 to 0x1000
  movi r2, 0x101C # PPN=0x1000, global, user, executable, not writable, not readable
  tlbw r2, r0

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
  mode halt

TLB_KMISS:
  movi r1, 2
  mode halt

# user mode
  .origin 0x1000
userland:
  movi r1, 0x42
  movi r2, 0x1000 # global page
  movi r3, 0x2000 # non-global page
  lwa  r3, [r2] # should succeed
  add  r1, r1, 1
  swa  r0, [r3] # should cause a tlb umiss
  add  r1, r1, 1 # should not reach here

  sys  EXIT