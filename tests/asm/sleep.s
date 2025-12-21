  .kernel

  .define PS2_ADDR 0x7FE5800

  .define UART_ADDR 0x7FE5802

  .define KEY_A 97
  .define KEY_Q 113

INT_KEYBOARD:

  # save caller registers used below
  push r3
  push r4
  push r5

  # save flags
  mov  r3, flg
  push r3

  # check key
  movi r4, PS2_ADDR
  lda  r3, [r4]

  mov  r5, r3
  and  r5, r5, 0xFF00 # ignore keyup events
  bnz  end
  and  r3, r3, 0x00FF

  cmp  r3, KEY_A
  bz   key_a
  cmp  r3, KEY_Q
  bz   key_q
  jmp  end

key_q:
  movi r4, UART_ADDR
  movi r3, 10
  sba  r3, [r4]
  mode halt

key_a:
  # increment counter
  lw   r3, [COUNTER]
  add  r3, r3, 1
  sw   r3, [COUNTER]

end:
  # mark interrupt as handled
  mov  r4, isr
  movi r3, 0xFFFFFFFD
  and  r4, r4, r3
  mov  isr, r4

  # restore flags
  pop  r3
  mov  flg, r3

  # restore caller registers
  pop  r5
  pop  r4
  pop  r3

  # return from the interrupt
  rfi

COUNTER:
  .fill 0

_start:
  # initialize stack
  movi r31, 0x1000

  # enable keyboard interrupts (bit 1) and global enable
  movi r3, 0x80000002
  mov  imr, r3

  # wait for keypress
loop:
  movi r4, UART_ADDR

  movi r3, 48
  lw   r5, [COUNTER]
  add  r3, r3, r5
  sba  r3, [r4]

  mode sleep
  jmp loop
