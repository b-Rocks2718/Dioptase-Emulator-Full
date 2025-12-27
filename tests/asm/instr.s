
  .global _start
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
  
EXIT:
  mode halt

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
  sys  EXIT