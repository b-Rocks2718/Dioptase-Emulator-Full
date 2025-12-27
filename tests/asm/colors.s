  # a colorful square that the user can move around using the keyboard
  # uses interrupts to accomplish this

  .define TILEMAP_ADDR 0x7FE8000
  .define FRAMEBUFFER_ADDR 0x7FC0000
  .define SCALE_REG_ADDR 0x7FE5B44
  .define HSCROLL_ADDR 0x7FE5B40
  .define VSCROLL_ADDR 0x7FE5B42
  .define PIT_ADDR 0x7FE5804

  .define PS2_ADDR 0x7FE5800

  .define KEY_W 119
  .define KEY_A 97
  .define KEY_S 115
  .define KEY_D 100
  .define KEY_I 105
  .define KEY_J 106
  .define KEY_K 107
  .define KEY_L 108
  .define KEY_X 120
  .define KEY_Z 122
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
  cmp  r3, KEY_I
  bz   key_i
  cmp  r3, KEY_J
  bz   key_j
  cmp  r3, KEY_K
  bz   key_k
  cmp  r3, KEY_L
  bz   key_l
  cmp  r3, KEY_X
  bz   key_x
  cmp  r3, KEY_Z
  bz   key_z
  cmp  r3, KEY_Q
  bz   key_q
  jmp  end

key_w:
  # move square up
  lw   r3, [SQUARE_INDEX]
  add  r3, r3, -80
  sw   r3, [SQUARE_INDEX]
  jmp  end

key_a:
  # move square left
  lw   r3, [SQUARE_INDEX]
  add  r3, r3, -1
  sw   r3, [SQUARE_INDEX]
  jmp  end

key_s:
  # move square down
  lw   r3, [SQUARE_INDEX]
  add  r3, r3, 80
  sw   r3, [SQUARE_INDEX]
  jmp  end

key_d:
  # move square right
  lw   r3, [SQUARE_INDEX]
  add  r3, r3, 1
  sw   r3, [SQUARE_INDEX]
  jmp  end

key_i:
  # scroll screen up
  movi r4, VSCROLL_ADDR
  lda  r3, [r4]
  add  r3, r3, -1
  sda  r3, [r4]
  jmp  end

key_j:
  # scroll screen left
  movi r4, HSCROLL_ADDR
  lda  r3, [r4]
  add  r3, r3, -1
  sda  r3, [r4]
  jmp  end

key_k:
  # scroll screen down
  movi r4, VSCROLL_ADDR
  lda  r3, [r4]
  add  r3, r3, 1
  sda  r3, [r4]
  jmp  end

key_l:
  # scroll screen right
  movi r4, HSCROLL_ADDR
  lda  r3, [r4]
  add  r3, r3, 1
  sda  r3, [r4]
  jmp  end

key_x:
  # disable timer interrupt
  movi r4, 0xFFFFFFFE
  mov  r3, imr
  and  r3, r4, r3
  mov  imr, r3
  jmp end

key_z:
  # enable timer interrupt
  movi r4, 1
  mov  r3, imr
  or   r3, r4, r3
  mov  imr, r3
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
  jmp  draw_square

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

SQUARE_INDEX:
  .fill 0

COLOR: 
  .fill 0xFF00FF00

  .global _start
_start:
  # initialize stack
  movi r1, 0x1000
  movi r2, 0x1000

  # set timer
  movi r4, PIT_ADDR
  movi r3, 50000
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
  add  r8, r8, 128 
  lw   r6,  [COLOR]

  movi r10, 64
draw_tile_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_tile_loop

draw_square:
  movi r8, FRAMEBUFFER_ADDR
  lw   r9, [SQUARE_INDEX]
  add  r8, r8, r9
  movi r5, 1
  sba  r5, [r8]

  # enable interrupts
  mov  r3, imr
  movi r4, 0x80000000
  or   r3, r4, r3
  mov  imr, r3

  # wait for keypress
  mode sleep
  mode halt # should never run
