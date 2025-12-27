  .global _start
_start:
  # set pid to 1
  movi r4, 1
  mov  pid, r4

  # set up tlb

  # map 0x0000 to 0x1000
  movi r2, 0x100F # PPN=0x1000, user, executable, writable, readable
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
  add  r1, r1, 1
  mode halt

TLB_KMISS:
  movi r1, 2
  mode halt

# user mode
  .origin 0x1000
userland:
  movi r1, 0x42
  sys  EXIT