  .define IVT_SYS_EXIT 0x04 # 0x01 * 4 per ISA

  .global _start
  .origin 0x400
_start:
  # Point the EXIT syscall vector at the test handler.
  lw   r22, [EXIT_PTR]
  movi r23, IVT_SYS_EXIT
  swa  r22, [r23]

  movi r1, 1
  movi r2, 2
  sys  EXIT    # should return 3
  add  r1, r1, 1
  mode halt

# as a test, the exit syscall will actually add
EXIT:
  add r1, r1, r2
  rfe

EXIT_PTR:
  .fill EXIT
