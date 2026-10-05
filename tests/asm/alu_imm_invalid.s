  # ALU-immediate ops 18 and 19 have no immediate encoding in docs/ISA.md and
  # must raise invalid-instruction (op 18 used to execute sxtb and op 19 used
  # to crash the emulator). The handler counts faults in r1 and skips them.
  .global _start
  .origin 0x200 # IVT EXC_INSTR (0x80 * 4)
  .fill EXC_INSTR

  .origin 0x400
  jmp _start
EXC_INSTR:
  add  r1, r1, 1
  mov  r30, epc
  add  r30, r30, 4
  mov  epc, r30
  rfe

_start:
  mov  r1 r0
  .fill 0x08812000 # ALU-imm op 18 (sxtb) r2, r0, 0
  .fill 0x08813000 # ALU-imm op 19 (sxtd) r2, r0, 0
  mode halt # should return 2
