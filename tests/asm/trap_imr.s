  .define IVT_TRAP 0x04 # shared trap vector per ISA

  .global _start
  .origin 0x400
  jmp _start
_start:
  # Point the shared trap vector at the test handler.
  lw   r22, [TRAP_PTR]
  movi r23, IVT_TRAP
  swa  r22, [r23]

  # Enable one interrupt source plus the global enable bit before the trap.
  lui  r3 0x80000000
  movi r4 1
  add  r3, r3, r4
  mov  imr, r3

  movi r1, 0
  movi r2, 0
  trap

  # After rfe returns, IMR[31] should be restored while the handler-observed
  # value in r2 still shows that trap entry masked only the global bit.
  mov  r1, imr
  add  r1, r1, r2

  mode halt

TRAP_HANDLER:
  # Trap entry must preserve per-source enables while clearing IMR[31].
  # Save the masked value so user mode can verify that rfe restored IMR[31].
  mov  r2, imr
  rfe

TRAP_PTR:
  .fill TRAP_HANDLER
