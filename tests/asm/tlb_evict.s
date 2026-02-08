  .global _start
  .origin 0x400
  jmp _start
_start:
  # Use a non-zero PID so this test exercises PID-scoped entries.
  movi r4, 1
  mov  pid, r4
  tlbc

  # Insert 17 distinct entries (TLB has size 16)
  movi r10, 0
  movi r11, 17
fill_tlb:
  # value = ((index + 1) << 12) | 0x17
  add  r2, r10, 1
  lsl  r2, r2, 12
  add  r2, r2, 0x17
  # key = index << 12
  lsl  r3, r10, 12
  tlbw r2, r3

  add  r10, r10, 1
  add  r11, r11, -1
  bnz  fill_tlb

  # Sanity check: most recent insert must be readable.
  movi r10, 16
  lsl  r3, r10, 12
  tlbr r7, r3
  add  r13, r7, 0
  bz   FAIL

  # Count misses across the 65 inserted keys.
  # At least one miss is required to prove an eviction happened.
  movi r10, 0
  movi r11, 17
  movi r12, 0
check_loop:
  lsl  r3, r10, 12
  tlbr r6, r3
  add  r13, r6, 0
  bnz  check_next
  add  r12, r12, 1
check_next:
  add  r10, r10, 1
  add  r11, r11, -1
  bnz  check_loop

  add  r13, r12, 0
  bz   FAIL

  movi r1, 1
  mode halt

FAIL:
  movi r1, 0
  mode halt
