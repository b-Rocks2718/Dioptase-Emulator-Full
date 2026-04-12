  .global _start
  .origin 0x208 # IVT TLB_MISS (0x82 * 4)
  .fill TLB_MISS

  .origin 0x400
  jmp _start
TLB_MISS:
  mov  r1, tlbf
  mode halt

_start:
  movi r4, 1
  mov  pid, r4

  # PPN=0x2000, user, writable, readable, not executable.
  movi r2, 0x200B
  movi r1, 0x10000000
  tlbw r2, r1

  jmp  r1
