
  .global _start

.define SD_DMA_MEM_ADDR 0x7FE5810
.define SD_DMA_SD_BLOCK 0x7FE5814
.define SD_DMA_LEN 0x7FE5818
.define SD_DMA_CTRL 0x7FE581C
.define SD_DMA_STATUS 0x7FE5820

.define SD_DMA_STATUS_BUSY 0x1
.define SD_DMA_CTRL_SD_INIT 0x8

  .origin 0x400
  jmp _start
_start:
  movi r11 0
  movi r4 0x2000
  movi r5 0x3000
  movi r6 SD_DMA_MEM_ADDR
  movi r7 SD_DMA_SD_BLOCK
  movi r8 SD_DMA_LEN
  movi r9 SD_DMA_CTRL
  movi r10 SD_DMA_STATUS

  # Initialize SD card 0 before issuing DMA commands.
  movi r1 SD_DMA_CTRL_SD_INIT
  swa  r1, [r9]

wait_init:
  lwa  r1 [r10]
  and  r1 r1 SD_DMA_STATUS_BUSY
  bnz  wait_init

  # write pattern to source
  movi r1 0x11223344
  swa  r1, [r4]
  movi r1 0x55667788
  swa  r1, [r4, 4]

  # DMA RAM -> SD (block 2, length 1 block)
  movi r1 2
  swa  r1, [r7]
  movi r1 1
  swa  r1, [r8]
  movi r1 0x2000
  swa  r1, [r6]
  movi r1 0x3
  swa  r1, [r9]

wait_write:
  lwa  r1 [r10]
  and  r1 r1 SD_DMA_STATUS_BUSY
  bnz  wait_write

  # clear destination
  swa  r0, [r5]
  swa  r0, [r5, 4]

  # DMA SD -> RAM (block 2, length 1 block)
  movi r1 2
  swa  r1, [r7]
  movi r1 1
  swa  r1, [r8]
  movi r1 0x3000
  swa  r1, [r6]
  movi r1 0x1
  swa  r1, [r9]

wait_read:
  lwa  r1 [r10]
  and  r1 r1 SD_DMA_STATUS_BUSY
  bnz  wait_read

  lwa  r1 [r5]
  movi r2 0x11223344
  xor  r1 r1 r2
  or   r11 r11 r1

  lwa  r1 [r5, 4]
  movi r2 0x55667788
  xor  r1 r1 r2
  or   r11 r11 r1

  mov  r1, r11
  mode halt
