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
  sba  r3, [r4]
  movi r3, E
  sba  r3, [r4]
  movi r3, L
  sba  r3, [r4]
  sba  r3, [r4]
  movi r3, O
  sba  r3, [r4]
  movi r3, NEWLINE
  sba  r3, [r4]

  mode halt
