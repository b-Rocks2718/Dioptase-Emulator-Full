  .kernel

  .define PS2_ADDR 0x20000
  .define UART_TX_ADDR 0x20002

_start:
  movi r4, UART_TX_ADDR
  movi r3, PS2_ADDR
  
  lda  r5, [r3]
  cmp  r5, r0
  bz   _start
  add  r5, r5, 1
  sba  r5, [r4]

  jmp _start