  .global _start
  .origin 0x400
  jmp _start
_start:
  # adpc should resolve the absolute address of target.
  adpc r1, target
  lw   r2, [target_addr]
  sub  r1, r1, r2
  mode halt

target_addr:
  .fill target

target:
  nop
