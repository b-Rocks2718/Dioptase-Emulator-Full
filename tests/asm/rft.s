  .global _start
  # Interrupt vector table entry used by this test.
  .origin 0x20C # IVT TLB_KMISS (0x83 * 4)
  .fill TLB_KMISS

  .origin 0x400
  jmp _start

TLB_KMISS:
  # skip the faulting instruction
  mov  r5, epc
  add  r5, r5, 4
  crmv epc, r5

  # r31 should alias ISP inside a kernel TLB miss
  mov  r1, r31
  movi r2, 0x2100
  mov  r31, r2

  # crmv must bypass the alias
  crmv r2, r31
  crmv r3, ksp
  crmv r4, isp

  add  r1, r1, r2
  add  r1, r1, r3
  add  r1, r1, r4

  rft

_start:
  movi r5, 0x1000
  crmv ksp, r5
  movi r6, 0x2000
  crmv isp, r6
  movi r7, 0x3000
  crmv r31, r7

  movi r7, 0xFFFFFFF0
  lwa  r3, [r7] # will cause a kernel TLB miss
  mode halt
