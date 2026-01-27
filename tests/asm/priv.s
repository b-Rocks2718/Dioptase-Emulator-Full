  .global _start
  # Interrupt vector table entries used by this test.
  .origin 0x204 # IVT EXC_PRIV (0x81 * 4)
  .fill EXC_PRIV
  .origin 0x208 # IVT TLB_UMISS (0x82 * 4)
  .fill TLB_UMISS
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
  jmp _start
_start:
  # set pid to 1
  movi r4, 1
  mov  pid, r4

  # set up tlb

  # map 0x0000 to 0x1000
  movi r2, 0x100F # PPN=0x1000, user, executable, writable, readable
  tlbw r2, r0

  # enter user mode
  crmv epc, r0
  rfe

TLB_UMISS:
  movi r1, 1
  mode halt

EXC_PRIV:
  movi r1, 21
  mode halt

TLB_KMISS:
  movi r1, 2
  mode halt

# user mode
  .origin 0x1000
userland:
  movi r1, 0x42
  crmv cr1, r2
  mode halt
