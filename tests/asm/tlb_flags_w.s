  .global _start
  # Interrupt vector table entries used by this test.
  .origin 0x208 # IVT TLB_UMISS (0x82 * 4)
  .fill TLB_UMISS
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
_start:
  # set pid to 1
  movi r4, 1
  mov  pid, r4

  # set up tlb

  # map 0x0000 to 0x1000
  movi r2, 0x100C # PPN=0x1000, user, executable, not writable, not readable
  tlbw r2, r0

  # map 0x1000 to 0x2000
  movi r2, 0x200A # PPN=0x2000, user, not executable, writable, not readable
  movi r1, 0x1000
  tlbw r2, r1

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
  movi r2, 0x1000 # writable memory but not readable
  swa  r3, [r2] # should succeed
  add  r1, r1, 1
  lwa  r0, [r2] # should cause a tlb umiss
  add  r1, r1, 1 # should not reach here

  mode halt
