  # Summary:
  # - Enter user mode and branch to an odd instruction address.
  # - Misaligned-PC exception entry must run before the odd target executes.
  # - EPC must capture the misaligned virtual fetch address.

  .global _start
  .origin 0x210 # IVT EXC_MISALIGNED_PC (0x84 * 4)
  .fill EXC_MISALIGNED_PC

  .origin 0x400
  jmp _start

EXC_MISALIGNED_PC:
  mov  r1, epc
  mode halt

_start:
  movi r4, 1
  mov  pid, r4

  # PPN=0x1000, user, executable, writable, readable.
  movi r2, 0x100F
  tlbw r2, r0

  mov  epc, r0
  rfe

  .origin 0x1000
userland:
  adpc r2, aligned_target
  add  r2, r2, 1
  jmp  r2

aligned_target:
  movi r1, 0
  mode halt
