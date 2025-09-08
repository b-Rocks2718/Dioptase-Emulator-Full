  .kernel

.define H 104
.define E 101
.define L 108
.define O 111
.define NEWLINE 10

.define UART_ADDR 0x20002

_start:
  movi r4, UART_ADDR

  movi r3, H
  swa  r3, [r4]
  movi r3, E
  swa  r3, [r4]
  movi r3, L
  swa  r3, [r4]
  swa  r3, [r4]
  movi r3, O
  swa  r3, [r4]
  movi r3, NEWLINE
  swa  r3, [r4]

  mode halt