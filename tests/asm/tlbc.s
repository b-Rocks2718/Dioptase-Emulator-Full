  .global _start
_start:
  # set up tlb

  # map 0x0000 to 0x1000
  movi r2, 0x100C # PPN=0x1000, user, executable, not writable, not readable
  tlbw r2, r0

  # map 0x10000000 to 0x2000
  movi r2, 0x2017 # PPN=0x2000
  movi r3, 0x10000000
  tlbw r2, r3

  tlbc
  tlbr r2, r0
  tlbr r3, r3

  add  r1, r2, r3

  mode halt
