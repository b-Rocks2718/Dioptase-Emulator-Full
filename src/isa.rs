// Instruction-encoding numbers from docs/ISA.md shared by the emulator core,
// its ALU, and the disassembler, so decoding and printing cannot disagree.

// Major opcodes: bits 31..27 of every instruction. The memory and atomic
// opcodes form contiguous blocks that are decoded by offset from the block's
// first opcode, so only each block's endpoints are named.
pub(crate) const OPC_ALU: u32 = 0;
pub(crate) const OPC_ALU_IMM: u32 = 1;
pub(crate) const OPC_LUI: u32 = 2;
// Loads/stores: word, double, then byte, each as absolute, PC-relative, and
// PC-relative immediate.
pub(crate) const OPC_MEM_WORD_ABS: u32 = 3;
pub(crate) const OPC_MEM_BYTE_IMM: u32 = 11;
pub(crate) const OPC_BRANCH_IMM: u32 = 12;
pub(crate) const OPC_BRANCH_ABS_REG: u32 = 13;
pub(crate) const OPC_BRANCH_REL_REG: u32 = 14;
pub(crate) const OPC_TRAP: u32 = 15;
// Atomics in absolute, PC-relative, and PC-relative immediate order.
pub(crate) const OPC_FETCH_ADD_ABS: u32 = 16;
pub(crate) const OPC_FETCH_ADD_IMM: u32 = 18;
pub(crate) const OPC_SWAP_ABS: u32 = 19;
pub(crate) const OPC_SWAP_IMM: u32 = 21;
pub(crate) const OPC_ADPC: u32 = 22;
// All privileged instructions share this opcode.
pub(crate) const OPC_PRIVILEGED: u32 = 31;

// ALU op numbers: the 5-bit op field (bits 9..5 of the register form, bits
// 16..12 of the immediate form) as listed in docs/ISA.md "ALU instructions".
// Both forms share the numbering; only ops up to `OP_SUBB` have an immediate
// form.
pub(crate) const OP_AND: u32 = 0;
pub(crate) const OP_NAND: u32 = 1;
pub(crate) const OP_OR: u32 = 2;
pub(crate) const OP_NOR: u32 = 3;
pub(crate) const OP_XOR: u32 = 4;
pub(crate) const OP_XNOR: u32 = 5;
pub(crate) const OP_NOT: u32 = 6;
pub(crate) const OP_LSL: u32 = 7;
pub(crate) const OP_LSR: u32 = 8;
pub(crate) const OP_ASR: u32 = 9;
pub(crate) const OP_ROTL: u32 = 10;
pub(crate) const OP_ROTR: u32 = 11;
pub(crate) const OP_LSLC: u32 = 12;
pub(crate) const OP_LSRC: u32 = 13;
pub(crate) const OP_ADD: u32 = 14;
pub(crate) const OP_ADDC: u32 = 15;
pub(crate) const OP_SUB: u32 = 16;
pub(crate) const OP_SUBB: u32 = 17;
pub(crate) const OP_SXTB: u32 = 18;
pub(crate) const OP_SXTD: u32 = 19;
pub(crate) const OP_TNCB: u32 = 20;
pub(crate) const OP_TNCD: u32 = 21;
