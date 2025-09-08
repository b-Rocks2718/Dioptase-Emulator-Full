  .kernel

  # have the kernel initialize the tlb
  # then enter user mode and draw a green square

  .define PHY_TILEMAP_ADDR 0x2A000
  .define PHY_FRAMEBUFFER_ADDR 0x2E000

  .define VRT_TILEMAP_ADDR 0x10000000
  .define VRT_FRAMEBUFFER_ADDR 0x20000000

  .define PS2_ADDR 0x20000

  .define USER_PID 1

EXIT:
  mode halt

INT_KEYBOARD:
  # return the character causing an interrupt
  movi r4, PS2_ADDR
  lda  r3, [r4]
  mode halt

INT_TIMER:
  mode halt

_start:
  call init_tlb
  movi r4, 1
  mov  pid, r4 # set pid to 1

  # enable interrupts
  movi r3, 0xFFFFFFFF
  mov  cr3, r3

  rfe  r0, r0 # jump to userland

init_tlb:
  movi r3, USER_PID
  lsl  r3, r3, 20

  movi r5, PHY_TILEMAP_ADDR
  movi r8, VRT_TILEMAP_ADDR
  lsr  r5, r5, 12
  lsr  r8, r8, 12
  or   r8, r8, r3
  tlbw r5, r8

  movi r5, PHY_FRAMEBUFFER_ADDR
  movi r8, VRT_FRAMEBUFFER_ADDR
  lsr  r5, r5, 12
  lsr  r8, r8, 12
  or   r8, r8, r3
  tlbw r5, r8

  movi r5, 0x1000
  movi r8, 0
  lsr  r5, r5, 12
  lsr  r8, r8, 12
  or   r8, r8, r3
  tlbw r5, r8

  ret

  .origin 0x1000
userland:

  movi r8, VRT_TILEMAP_ADDR
  add  r8, r8, 128
  movi r6, 0xF0    # green

  movi r10, 64
draw_square_loop:
  swa  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_square_loop

  movi r8, VRT_FRAMEBUFFER_ADDR
  movi r5, 1
  swa  r5, [r8]

inf_loop:
  jmp  inf_loop
