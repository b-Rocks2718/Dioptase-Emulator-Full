
  .global _start
  .define PS2_ADDR 0x7FE5800
  .define UART_TX_ADDR 0x7FE5802

  .origin 0x400
  jmp _start
_start:
  movi r4, UART_TX_ADDR
  movi r3, PS2_ADDR
  
  lda  r5, [r3]
  cmp  r5, r0
  bz   _start
  and  r6, r5, 0xFF00 # check if this is keyup or keydown
  bnz  _start # ignore keyup
  add  r5, r5, 1
  sba  r5, [r4]

  jmp _start
