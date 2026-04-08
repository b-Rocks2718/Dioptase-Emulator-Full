  .global _start
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
  jmp _start
TLB_KMISS:
  mov  r1, tlbf
  mode halt

_start:
  movi r4, 1
  mov  pid, r4

  # PPN=0x2000, user, executable, writable, not readable.
  movi r2, 0x200E
  movi r1, 0x10000000
  tlbw r2, r1

  lwa  r3, [r1]
