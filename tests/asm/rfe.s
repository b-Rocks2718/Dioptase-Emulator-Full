
  .global _start
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