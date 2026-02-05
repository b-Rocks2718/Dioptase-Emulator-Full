
  .global _start
.define H 104
.define E 101
.define L 108
.define O 111
.define NEWLINE 10

.define UART_ADDR 0x7FE5802

.define VGA_STATUS_ADDR 0x7FE5B46
.define VGA_FRAME_ADDR 0x7FE5B48

.define VGA_IVT 0x3D0

  .origin 0x400
  jmp _start
hello:
  push r3
  push r4

  # prints "hello\n" to the uart

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

  pop  r4
  pop  r3

  ret

INT_VGA:
  push r1
  mov  r1, flg
  push r1
  push r3
  push r4

  # mark interrupt as handled
  mov  isr, r0

  movi r4, UART_ADDR
  add  r3, r2, 48
  sba  r3, [r4]

  call hello

  pop  r4
  pop  r3
  pop  r1
  mov  flg, r1
  pop  r1

  rfi

_start:
  # register the VGA interrupt handler
  movi r4, VGA_IVT
  adpc r5, INT_VGA
  swa  r5, [r4]

  # set up stack pointer
  movi r31, 0x10000

  # enable interrupts
  movi r1, 0x80000010
  mov  imr, r1

  movi r1, 3

loop:
  movi r3, VGA_FRAME_ADDR
  lwa  r2, [r3]
  cmp  r2, r1
  bz   end

  movi r4, UART_ADDR
  movi r3, VGA_STATUS_ADDR
  lba  r2, [r3]
  add  r2, r2, 48
  #sba  r2, [r4] # print VGA status
  movi r2, 10
  #sba  r2, [r4]

  jmp  loop

end:
  mov  r1, r2
  mode halt
