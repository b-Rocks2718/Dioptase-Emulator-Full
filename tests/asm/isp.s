  .global _start

  # Interrupt vector table entry used by this test.
  .origin 0x3C0 # IVT timer interrupt (0xF0 * 4)
  .fill INT_TIMER

  .origin 0x400
  jmp _start

INT_TIMER:
  # clear timer interrupt
  mov  r6, isr
  movi r7, 0xFFFFFFFE
  and  r6, r7, r6
  mov  isr, r6

  # r31 should alias ISP inside an interrupt
  mov  r1, r31
  movi r2, 0x2100
  mov  r31, r2

  # crmv must bypass the alias
  crmv r2, r31
  crmv r3, cr8
  crmv r4, isp

  add  r1, r1, r2
  add  r1, r1, r3
  add  r1, r1, r4

  rfi

_start:
  movi r5, 0x1000
  crmv cr8, r5
  movi r6, 0x2000
  crmv isp, r6
  movi r7, 0x3000
  crmv r31, r7

  # enable timer interrupt + global
  movi r1, 0x80000001
  mov  imr, r1
  movi r1, 0x1
  mov  isr, r1

  nop
  mode halt
