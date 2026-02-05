  .origin 0x400
  jmp _start
  # demonstrates tile color override (0xCXXX pixels) with the same tile
  # shown in multiple locations using different per-entry colors

  .define TILEMAP_ADDR 0x7FE8000
  .define TILE_FRAMEBUFFER_ADDR 0x7FBD000
  .define TILE_SCALE_ADDR 0x7FE5B44

  .global _start
_start:
  # scale tile layer by 8 for visibility
  movi r4, TILE_SCALE_ADDR
  movi r5, 3
  sba  r5, [r4]

  # initialize tile 1 to 0xC000 pixels (use tile entry color)
  movi r8, TILEMAP_ADDR
  add  r8, r8, 128 # skip first tile
  movi r6, 0xC000
  movi r10, 32
tile0_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  tile0_loop

  # add some normal pixels
  movi r6, 0x0888
  movi r10, 32
tile1_loop:
  sda  r6, [r8], 2
  add  r10, r10, -1
  bnz  tile1_loop

  # place tile 0 in a 2x2 block with different colors
  movi r8, TILE_FRAMEBUFFER_ADDR

  # top-left: red (RGB332 0b11100000)
  movi r6, 0xE001
  sda  r6, [r8]

  # top-right: green (RGB332 0b00011100)
  movi r6, 0x1C01
  sda  r6, [r8, 2]

  # bottom-left: blue (RGB332 0b00000011)
  movi r6, 0x0301
  sda  r6, [r8, 160] # 80 tiles * 2 bytes

  # bottom-right: white (RGB332 0b11111111)
  movi r6, 0xFF01
  sda  r6, [r8, 162]

halt:
  jmp halt
