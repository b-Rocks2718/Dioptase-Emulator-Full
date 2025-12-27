  .global _start
_start:
  # Setup: store 5 at absolute addr 0x7F0 using base r4 + 0x7E0.
  add  r4 r0 0x10
  add  r2 r0 5
  swa  r2 [r4, 0x7E0]

  # Absolute fadd: r5 <- old (5), mem <- old + 7 (12).
  add  r6 r0 7
  fada r5, r6, [r4, 0x7E0]

  # PC-relative fadd with base register: r8 <- old (9), mem <- old + 4 (13).
  add  r7 r0 4
  fad  r8, r7, [r0, DATA_REL]

  # PC-relative fadd immediate: r10 <- old (32), mem <- old + 6 (38).
  add  r11 r0 6
  fad  r10, r11, [DATA_IMM]

  # Read back updated memory to validate side effects.
  lwa  r9 [r4, 0x7E0]  # expect 12
  lw   r12, [DATA_REL] # expect 13
  lw   r13, [DATA_IMM] # expect 38

  # Sum of all observed values:
  # r5(5) + r9(12) + r8(9) + r12(13) + r10(32) + r13(38) = 109 (0x6D)
  add  r14 r5 r9
  add  r14 r14 r8
  add  r14 r14 r12
  add  r14 r14 r10
  add  r14 r14 r13
  mov  r1, r14
  sys  EXIT     # should return 0x6D

EXIT:
  mode halt

DATA_REL: .fill 9
DATA_IMM: .fill 32
