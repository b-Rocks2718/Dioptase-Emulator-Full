  .kernel

  # a colorful square that the user can move around using the keyboard
  # uses a sprite instead of the framebuffer
  # uses interrupts to accomplish this

  .define SPRITEMAP_0_ADDR 0x26000
  .define SPRITE_0_X_ADDR 0x2FFD0
  .define SPRITE_0_Y_ADDR 0x2FFD2
  
  .define PS2_ADDR 0x20000

  .define KEY_W 119
  .define KEY_A 97
  .define KEY_S 115
  .define KEY_D 100
  .define KEY_Q 113

EXIT:
  mode halt

INT_KEYBOARD:

  # check key
  movi r4, PS2_ADDR
  lda  r3, [r4]

  cmp  r3, KEY_W
  bz   key_w
  cmp  r3, KEY_A
  bz   key_a
  cmp  r3, KEY_S
  bz   key_s
  cmp  r3, KEY_D
  bz   key_d
  cmp  r3, KEY_Q
  bz   key_q
  jmp  end

key_w:
  # move sprite up
  movi r4, SPRITE_0_Y_ADDR
  lda  r3, [r4]
  add  r3, r3, -5
  sda  r3, [r4]
  jmp  end

key_a:
  # move sprite left
  movi r4, SPRITE_0_X_ADDR
  lda  r3, [r4]
  add  r3, r3, -5
  sda  r3, [r4]
  jmp  end

key_s:
  # move sprite down
  movi r4, SPRITE_0_Y_ADDR
  lda  r3, [r4]
  add  r3, r3, 5
  sda  r3, [r4]
  jmp  end

key_d:
  # move sprite right
  movi r4, SPRITE_0_X_ADDR
  lda  r3, [r4]
  add  r3, r3, 5
  sda  r3, [r4]
  jmp  end

key_q:
  sys  EXIT

end:
  # mark interrupt as handled
  mov  r4, isr
  movi r3, 0xFFFFFFFD
  and  r4, r4, r3
  mov  isr, r4

  # return from the interrupt
  mov  r29, efg
  mov  r30, epc
  rfi  r29, r30

_start:
  # initialize stack
  movi r1, 0x1000
  movi r2, 0x1000

  # set imr
  movi r3, 0x7FFFFFFF
  mov  imr, r3

  # load address and color
  movi r8, SPRITEMAP_0_ADDR
  movi r6, 0xF

  movi r10, 1024 # size of a sprite
draw_sprite_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_sprite_loop

  # enable interrupts
  mov  r3, imr
  movi r4, 0x80000000
  or   r3, r4, r3
  mov  imr, r3

  # wait for keypress
  mode sleep
  jmp draw_sprite_loop
