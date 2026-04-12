  .define IVT_TRAP 0x04 # shared trap vector per ISA

  .global _start
  .origin 0x400
  jmp _start
_start:
  # Point the shared trap vector at the test handler.
  lw   r22, [TRAP_PTR]
  movi r23, IVT_TRAP
  swa  r22, [r23]

  # Trap ABI:
  # - r1 = trap code
  # - r2 = trap argument word
  movi r1, 0
  movi r2, 3
  trap          # handler returns 3 in r1
  add  r1, r1, 1
  mode halt

TRAP_HANDLER:
  mov  r1, r2
  rfe

TRAP_PTR:
  .fill TRAP_HANDLER
