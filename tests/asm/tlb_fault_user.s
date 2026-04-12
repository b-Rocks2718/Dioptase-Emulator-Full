  .global _start
  .origin 0x208 # IVT TLB_MISS (0x82 * 4)
  .fill TLB_MISS

  .origin 0x400
  jmp _start
_start:
  movi r4, 1
  mov  pid, r4

  # PPN=0x1000, user, executable. Used for the user text page.
  movi r2, 0x100C
  tlbw r2, r0

  # PPN=0x2000, executable, writable, readable, but kernel-only.
  movi r2, 0x2007
  movi r1, 0x10000000
  tlbw r2, r1

  mov  epc, r0
  rfe

TLB_MISS:
  mov  r1, tlbf
  mode halt

  .origin 0x1000
userland:
  movi r2, 0x10000000
  lwa  r0, [r2]
