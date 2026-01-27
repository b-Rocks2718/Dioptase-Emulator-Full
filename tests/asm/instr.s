
  .global _start

  # Interrupt vector table entries used by this test.
  .origin 0x200 # IVT EXC_INSTR (0x80 * 4)
  .fill EXC_INSTR
  .origin 0x204 # IVT EXC_PRIV (0x81 * 4)
  .fill EXC_PRIV
  .origin 0x208 # IVT TLB_UMISS (0x82 * 4)
  .fill TLB_UMISS
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
_start:
  # set pid to 1
  movi r1, 1
  mov  pid, r1

  # set up tlb

  # map 0x0000 to 0x1000
  movi r2, 0x100F # PPN=0x1000, user, executable, writable, readable
  tlbw r2, r0

  # enter user mode
  mov epc, r0
  rfe

TLB_UMISS:
  movi r1, 1
  mode halt

EXC_INSTR:
  movi r1, 22
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
  .fill 0xEEEEEEEE
  mode halt
