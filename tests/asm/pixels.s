
  .origin 0x400
  jmp _start
  # have the kernel initialize the tlb
  # then enter user mode and draw a green square
  .global _start
  .define PHY_PIXEL_FRAMEBUFFER_ADDR 0x7FC0000
  .define PHY_TILEMAP_ADDR 0x7FE8000

  .define VMEM_FLAGS 0x00F

  .define PS2_IVT 0x3C4

  .define VRT_PIXEL_FRAMEBUFFER_ADDR 0x20000000
  .define USER_PC_START 0x80000000

  .define PS2_ADDR 0x7FE5800

  .define USER_PID 1

INT_KEYBOARD:
  # return the character causing an interrupt
  movi r4, PS2_ADDR
  lda  r1, [r4]
  mode halt

_start:
  # register the keyboard interrupt handler
  movi r4, PS2_IVT
  adpc r5, INT_KEYBOARD
  swa  r5, [r4]

  movi r4, USER_PID
  mov  pid, r4 # set pid to 1

  call init_tlb

  # make tile 0 transparent so the pixel framebuffer is visible
  movi r8, PHY_TILEMAP_ADDR
  movi r6, 0xF000
  movi r10, 64
clear_tile0:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  clear_tile0

  # enable interrupts
  movi r3, 0x8000000F
  mov  imr, r3

  movi r1, USER_PC_START
  mov  epc, r1
  rfe  # jump to userland

init_tlb:
  movi r4, VMEM_FLAGS

  # map VRT_PIXEL_FRAMEBUFFER_ADDR => PHY_PIXEL_FRAMEBUFFER_ADDR
  movi r2, PHY_PIXEL_FRAMEBUFFER_ADDR
  movi r3, VRT_PIXEL_FRAMEBUFFER_ADDR
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

  movi r8, VRT_PIXEL_FRAMEBUFFER_ADDR
  movi r6, 0xF0    # green

  movi r10, 650
draw_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_loop

inf_loop:
  jmp  inf_loop
