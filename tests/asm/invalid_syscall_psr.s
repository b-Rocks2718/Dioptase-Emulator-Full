  # Summary:
  # - Execute an unsupported syscall encoding.
  # - The invalid-instruction handler must observe exactly one PSR increment.

  .global _start
  .origin 0x200 # IVT EXC_INSTR (0x80 * 4)
  .fill EXC_INSTR

  .origin 0x400
  jmp _start
EXC_INSTR:
  mov  r1, psr
  mov  r30, epc
  add  r30, r30, 4
  mov  epc, r30
  rfe

_start:
  # Raw `sys` with immediate 0; the emulator only recognizes immediate 1.
  .fill 0x78000000

  mode halt
