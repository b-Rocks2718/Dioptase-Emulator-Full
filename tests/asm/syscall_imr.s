  .define IVT_SYS_EXIT 0x04 # 0x01 * 4 per ISA

  .global _start
  .origin 0x400
  jmp _start
_start:
  # Point the EXIT syscall vector at the test handler.
  lw   r22, [EXIT_PTR]
  movi r23, IVT_SYS_EXIT
  swa  r22, [r23]

  # Enable one interrupt source plus the global enable bit before the syscall.
  lui  r3 0x80000000
  movi r4 1
  add  r3, r3, r4
  mov  imr, r3

  sys  EXIT

  # After rfe returns, IMR[31] should be restored while the handler-observed
  # value in r2 still shows that syscall entry masked only the global bit.
  mov  r1, imr
  add  r1, r1, r2

  mode halt

EXIT:
  # Syscall entry must preserve per-source enables while clearing IMR[31].
  # Save the masked value so user mode can verify that rfe restored IMR[31].
  mov  r2, imr
  rfe

EXIT_PTR:
  .fill EXIT
