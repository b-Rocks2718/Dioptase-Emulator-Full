  .kernel

EXC_INSTR:
  mov r29, efg
  mov r30, epc
  add r30, r30, 4 # skip the bad instruction and then return
  rfi r29, r30

_start:
  movi r3, 0x42
  # bad instruction
  .fill 0xEEEEEEEE

  # handler should return here
  add  r3, r3, 2

  # ensure interrupts are enabled
  mov  r4, imr
  add  r3, r4, r3

  mode halt