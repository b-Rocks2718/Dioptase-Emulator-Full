  # Immediate subb computes i - rB - borrow, like immediate sub (i - rB).
  .global _start
  .origin 0x400
  jmp _start
_start:
  add  r4 r0 8
  add  r6 r0 1
  sub  r2 r6 r0  # 1 - 0: carry set (no borrow); must directly precede subb
  subb r1 r4 50
  mode halt # should return 50 - 8 = 42
