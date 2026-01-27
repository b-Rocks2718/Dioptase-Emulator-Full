
  .global _start
  .define UART_RX_ADDR 0x7FE5803
  .define UART_TX_ADDR 0x7FE5802

  .origin 0x400
  jmp _start
_start:
  movi r4, UART_TX_ADDR
  movi r3, UART_RX_ADDR
loop: 
  lba  r5, [r3]
  cmp  r5, r0
  bz   loop
  add  r5, r5, 1
  sba  r5, [r4]

  jmp loop
