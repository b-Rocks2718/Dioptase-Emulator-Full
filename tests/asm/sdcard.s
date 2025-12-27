
  .global _start
EXIT:
  mode halt

.define SD_SEND_REG 0x7FE58F9
.define SD_CMD_BASE 0x7FE58FA
.define SD_BUF_BASE 0x7FE5900

_start:
  movi r7 0
  movi r4 SD_CMD_BASE
  movi r5 SD_SEND_REG
  movi r6 SD_BUF_BASE

  # CMD0
  movi r1 0x40
  sba  r1, [r4]
  sba  r0, [r4, 1]
  sba  r0, [r4, 2]
  sba  r0, [r4, 3]
  sba  r0, [r4, 4]
  movi r1 0x95
  sba  r1, [r4, 5]
  sba  r0, [r5]

  lba  r1 [r4]
  xor  r1 r1 0x01
  or   r7 r7 r1

  # CMD8
  movi r1 0x48
  sba  r1, [r4]
  sba  r0, [r4, 1]
  sba  r0, [r4, 2]
  movi r1 0x01
  sba  r1, [r4, 3]
  movi r1 0xAA
  sba  r1, [r4, 4]
  movi r1 0x87
  sba  r1, [r4, 5]
  sba  r0, [r5]

  lba  r1 [r4]
  xor  r1 r1 0x01
  or   r7 r7 r1
  lba  r1 [r4, 4]
  xor  r1 r1 0xAA
  or   r7 r7 r1

  # CMD55
  movi r1 0x77
  sba  r1, [r4]
  sba  r0, [r4, 1]
  sba  r0, [r4, 2]
  sba  r0, [r4, 3]
  sba  r0, [r4, 4]
  sba  r0, [r4, 5]
  sba  r0, [r5]

  lba  r1 [r4]
  xor  r1 r1 0x01
  or   r7 r7 r1

  # ACMD41
  movi r1 0x69
  sba  r1, [r4]
  movi r1 0x40
  sba  r1, [r4, 1]
  sba  r0, [r4, 2]
  sba  r0, [r4, 3]
  sba  r0, [r4, 4]
  sba  r0, [r4, 5]
  sba  r0, [r5]

  lba  r1 [r4]
  xor  r1 r1 0x00
  or   r7 r7 r1

  # CMD58
  movi r1 0x7A
  sba  r1, [r4]
  sba  r0, [r4, 1]
  sba  r0, [r4, 2]
  sba  r0, [r4, 3]
  sba  r0, [r4, 4]
  sba  r0, [r4, 5]
  sba  r0, [r5]

  lba  r1 [r4]
  xor  r1 r1 0x00
  or   r7 r7 r1
  lba  r1 [r4, 1]
  xor  r1 r1 0x40
  or   r7 r7 r1

  # Write block (CMD24)
  movi r1 0x0BADCAFE
  swa  r1, [r6]

  movi r1 0x58
  sba  r1, [r4]
  sba  r0, [r4, 1]
  sba  r0, [r4, 2]
  sba  r0, [r4, 3]
  sba  r0, [r4, 4]
  sba  r0, [r4, 5]
  sba  r0, [r5]

  swa  r0, [r6]

  # Read block (CMD17)
  movi r1 0x51
  sba  r1, [r4]
  sba  r0, [r4, 1]
  sba  r0, [r4, 2]
  sba  r0, [r4, 3]
  sba  r0, [r4, 4]
  sba  r0, [r4, 5]
  sba  r0, [r5]

  lwa  r1 [r6]
  movi r2 0x0BADCAFE
  xor  r1 r1 r2
  or   r7 r7 r1

  add  r3 r7 r0
  mov  r1, r3
  sys  EXIT
