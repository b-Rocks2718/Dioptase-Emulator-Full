
  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x200 # IVT EXC_INSTR (0x80 * 4)
  .fill EXC_INSTR

  .origin 0x400
  jmp _start
EXC_INSTR:
  mov r30, epc
  add r30, r30, 4 # skip the bad instruction and then return
  mov epc, r30
  rfe

_start:
  movi r3, 0x42
  # bad instruction
  .fill 0xEEEEEEEE

  # handler should return here
  add  r3, r3, 2
  mov  r1, r3

  mode halt
