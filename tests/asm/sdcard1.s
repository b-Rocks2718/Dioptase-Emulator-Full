
  .global _start

.define SD_DMA_MEM_ADDR 0x7FE5908
.define SD_DMA_SD_BLOCK 0x7FE590C
.define SD_DMA_LEN 0x7FE5910
.define SD_DMA_CTRL 0x7FE5914
.define SD_DMA_STATUS 0x7FE5918

.define SD_DMA_STATUS_BUSY 0x1

_start:
  movi r11 0
  movi r4 0x2000
  movi r5 0x3000
  movi r6 SD_DMA_MEM_ADDR
  movi r7 SD_DMA_SD_BLOCK
  movi r8 SD_DMA_LEN
  movi r9 SD_DMA_CTRL
  movi r10 SD_DMA_STATUS

  # write pattern to source
  movi r1 0xA1B2C3D4
  swa  r1, [r4]
  movi r1 0x55667788
  swa  r1, [r4, 4]

  # DMA RAM -> SD1 (block 3, length 8)
  movi r1 3
  swa  r1, [r7]
  movi r1 8
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

  # DMA SD1 -> RAM (block 3, length 8)
  movi r1 3
  swa  r1, [r7]
  movi r1 8
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
  movi r2 0xA1B2C3D4
  xor  r1 r1 r2
  or   r11 r11 r1

  lwa  r1 [r5, 4]
  movi r2 0x55667788
  xor  r1 r1 r2
  or   r11 r11 r1

  mov  r1, r11
  mode halt
