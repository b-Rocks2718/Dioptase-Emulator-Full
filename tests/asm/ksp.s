  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x204 # IVT EXC_PRIV (0x81 * 4)
  .fill EXIT

  .origin 0x400
  jmp _start
_start:
  # set pid to 1
  movi r4, 1
  mov  pid, r4

  # set up tlb
  # map 0x0000 to 0x1000
  movi r2, 0x100F # PPN=0x1000
  tlbw r2, r0

  movi r31, 0x67 # should actually write to ksp

  # enter user mode
  mov epc, r0
  rfe

EXIT:
  mov r1, r31 # actually ksp
  crmv r2, r31 # real r31
  add r1, r1, r2 # r1 = r31 + ksp = 0xA9
  mode halt

  .origin 0x1000
userland:
  movi r31, 0x42 # should be the real r31
  mode halt
