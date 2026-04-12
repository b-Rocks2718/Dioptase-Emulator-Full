  # Summary:
  # - Execute a reserved non-zero trap encoding.
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
  # Raw `trap` with a non-zero reserved payload bit set.
  .fill 0x78000001

  mode halt
