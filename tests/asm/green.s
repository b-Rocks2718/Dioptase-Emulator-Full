  .origin 0x400
  # have the kernel initialize the tlb
  # then enter user mode and draw a green square

  .define PHY_TILEMAP_ADDR 0x7FE8000
  .define PHY_TILE_FRAMEBUFFER_ADDR 0x7FBD000

  .define PS2_INT 0x3C4

  .define VMEM_FLAGS 0x00F

  .define VRT_TILEMAP_ADDR 0x10000000
  .define VRT_TILE_FRAMEBUFFER_ADDR 0x20000000
  .define USER_PC_START 0x80000000

  .define PS2_ADDR 0x7FE5800

  .define USER_PID 1

  .global _start
_start:
  # register the keyboard interrupt handler
  movi r1, PS2_INT
  adpc r2, INT_KEYBOARD
  swa  r2, [r1]

  movi r4, USER_PID
  mov  pid, r4 # set pid to 1

  call init_tlb

  # enable interrupts
  movi r3, 0x8000000F
  mov  imr, r3

  movi r1, USER_PC_START
  mov  epc, r1
  rfe  # jump to userland

init_tlb:
  # map VRT_TILEMAP_ADDR => PHY_TILEMAP_ADDR
  movi r2, PHY_TILEMAP_ADDR
  movi r4, VMEM_FLAGS
  or   r2, r2, r4

  movi r3, VRT_TILEMAP_ADDR

  tlbw r2, r3

  # map VRT_TILE_FRAMEBUFFER_ADDR => PHY_TILE_FRAMEBUFFER_ADDR
  movi r2, PHY_TILE_FRAMEBUFFER_ADDR
  movi r3, VRT_TILE_FRAMEBUFFER_ADDR
  or   r2, r2, r4

  tlbw r2, r3

  # map 0x80000000 => 0x0001000
  movi r2, 0x1000
  movi r3, USER_PC_START
  or   r2, r2, r4

  tlbw r2, r3

  ret

  .origin 0x1000
userland:

  movi r8, VRT_TILEMAP_ADDR
  add  r8, r8, 128
  movi r6, 0xF0    # green

  movi r10, 64
draw_square_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_square_loop

  movi r8, VRT_TILE_FRAMEBUFFER_ADDR
  movi r5, 1
  sda  r5, [r8]

inf_loop:
  jmp  inf_loop


INT_KEYBOARD:
  # return the character causing an interrupt
  movi r4, PS2_ADDR
  lda  r1, [r4]
  mode halt
