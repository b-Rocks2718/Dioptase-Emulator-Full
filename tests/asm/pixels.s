
  # have the kernel initialize the tlb
  # then enter user mode and draw a green square
  .global _start
  .define PHY_FRAMEBUFFER_ADDR 0x7FC0000

  .define VMEM_FLAGS 0x00F

  .define VRT_FRAMEBUFFER_ADDR 0x20000000
  .define USER_PC_START 0x80000000

  .define PS2_ADDR 0x7FE5800
  .define VGA_MODE_ADDR 0x7FE5B45

  .define USER_PID 1

EXIT:
  mode halt

INT_KEYBOARD:
  # return the character causing an interrupt
  movi r4, PS2_ADDR
  lda  r1, [r4]
  mode halt

INT_TIMER:
  mode halt

_start:
  movi r4, USER_PID
  mov  pid, r4 # set pid to 1

  call init_tlb

  # set VGA to pixel mode
  movi r2, VGA_MODE_ADDR
  movi r3, 0x1
  sba  r3, [r2]

  # enable interrupts
  movi r3, 0x8000000F
  mov  imr, r3

  movi r1, USER_PC_START
  mov  epc, r1
  rfe  # jump to userland

init_tlb:
  movi r4, VMEM_FLAGS

  # map VRT_FRAMEBUFFER_ADDR => PHY_FRAMEBUFFER_ADDR
  movi r2, PHY_FRAMEBUFFER_ADDR
  movi r3, VRT_FRAMEBUFFER_ADDR
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

  movi r8, VRT_FRAMEBUFFER_ADDR
  movi r6, 0xF0    # green

  movi r10, 650
draw_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  draw_loop

inf_loop:
  jmp  inf_loop
