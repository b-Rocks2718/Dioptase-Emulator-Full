# Profiler fixture: _start calls work 5 times so per-function instruction
# counts are exact and hand-checkable (see profile_counts_calls in tests.rs).
  .global _start
  .origin 0x400
  jmp _start
_start:
  add  r4 r0 5
  add  r1 r0 0
_start.loop:
  call work
  add  r4 r4 -1
  cmp  r4 r0
  bnz  _start.loop
  mode halt # should return 5

work:
  add  r1 r1 1
  ret
