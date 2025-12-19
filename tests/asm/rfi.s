  .kernel

EXC_INSTR:
  mov r1, epc
  add r1, r1, 4 # skip the bad instruction and then return
  crmv epc, r1
  rfi

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