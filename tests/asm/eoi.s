# Test summary:
# - verifies the assembler accepts `eoi n` and `eoi all`
# - verifies `crmv` writes to `isr` are ignored by the emulator
# - returns the final ISR value in r1

  .global _start

  .origin 0x400
  jmp _start

_start:
  movi r2, 0xFFFFFFFF
  crmv isr, r2
  eoi 0
  eoi all
  crmv r1, isr
  mode halt
