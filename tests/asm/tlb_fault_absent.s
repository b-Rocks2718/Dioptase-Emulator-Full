  .global _start
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
  jmp _start
TLB_KMISS:
  mov  r1, tlbf
  mode halt

_start:
  movi r4, 0xFFFFF000
  lwa  r3, [r4]
