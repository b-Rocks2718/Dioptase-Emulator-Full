  .kernel

  # changes the color of the tilemap based on timer interrupts
  # changes the speed of the color change based on keyboard input
  # X to speed up, Z to slow down, Q to quit

  .define TILEMAP_ADDR 0x7FE8000
  .define FRAMEBUFFER_ADDR 0x7FC0000
  .define SCALE_REG_ADDR 0x7FE5B44
  .define HSCROLL_ADDR 0x7FE5B40
  .define VSCROLL_ADDR 0x7FE5B42
  .define PIT_ADDR 0x7FE5804
  .define CLK_DIV_ADDR 0x7FE5B4C

  .define PS2_ADDR 0x7FE5800
  .define KEY_X 120
  .define KEY_Z 122
  .define KEY_Q 113

EXIT:
  mode halt

INT_KEYBOARD:

  # check key
  movi r4, PS2_ADDR
  lda  r3, [r4]

  cmp  r3, KEY_X
  bz   key_x
  cmp  r3, KEY_Z
  bz   key_z
  cmp  r3, KEY_Q
  bz   key_q
  jmp  end

key_x:
  # make it faster
  movi r4, CLK_DIV_ADDR
  lwa  r3, [r4]
  lsr  r3, r3, 1
  swa  r3  [r4]
  jmp end

key_z:
  # make it slower
  movi r4, CLK_DIV_ADDR
  lwa  r3, [r4]
  lsl  r3, r3, 1
  swa  r3, [r4]
  jmp end

key_q:
  sys  EXIT

end:
  # mark interrupt as handled
  mov  r4, isr
  movi r3, 0xFFFFFFFD
  and  r4, r4, r3
  mov  isr, r4

  # return from the interrupt
  jmp  wait

INT_TIMER:
  # update color
  lw   r6, [COLOR]
  rotr r6, r6, 1
  sw   r6, [COLOR]

  # mark interrupt as handled
  mov  r4, isr
  movi r3, 0xFFFFFFFE
  and  r4, r4, r3
  mov  isr, r4

  # return from the interrupt
  jmp  set_color

COLOR: 
  .fill 0xFF00FF00

_start:
  # initialize stack
  movi r1, 0x1000
  movi r2, 0x1000

  # set timer
  movi r4, PIT_ADDR
  movi r3, 5000
  swa  r3, [r4]

  # set clock divider
  movi r3, 0x1000
  movi r4, CLK_DIV_ADDR
  swa  r3, [r4]

  # set imr
  movi r3, 0x0000000F
  mov  imr, r3

  # set square reg
  movi r3, SCALE_REG_ADDR
  movi r4, 1
  sba  r4, [r3]

set_color:
  movi r8, TILEMAP_ADDR
  lw   r6,  [COLOR]

  movi r10, 64
draw_tile_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_tile_loop
wait:
  # enable interrupts
  mov  r3, imr
  movi r4, 0x80000000
  or   r3, r4, r3
  mov  imr, r3

  # wait for keypress
  mode sleep
