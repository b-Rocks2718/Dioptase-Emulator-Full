// Disassembler written by Codex

fn sign_extend(value: u32, bits: u8) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

fn reg_name(reg: u32) -> String {
    format!("r{}", reg)
}

fn creg_name(reg: u32) -> String {
    format!("cr{}", reg)
}

fn fmt_imm_hex(value: u32) -> String {
    format!("0x{:08X}", value)
}

fn fmt_imm_signed(value: i32) -> String {
    format!("{}", value)
}

fn alu_op_name(op: u32) -> Option<&'static str> {
    const OPS: [&str; 19] = [
        "and", "nand", "or", "nor", "xor", "xnor", "not", "lsl", "lsr", "asr",
        "rotl", "rotr", "lslc", "lsrc", "add", "addc", "sub", "subb", "mul",
    ];
    OPS.get(op as usize).copied()
}

fn branch_name(op: u32) -> Option<&'static str> {
    const OPS: [&str; 19] = [
        "br", "bz", "bnz", "bs", "bns", "bc", "bnc", "bo", "bno",
        "bps", "bnps", "bg", "bge", "bl", "ble", "ba", "bae", "bb", "bbe",
    ];
    OPS.get(op as usize).copied()
}

fn branch_abs_name(op: u32) -> Option<&'static str> {
    const OPS: [&str; 19] = [
        "bra", "bza", "bnza", "bsa", "bnsa", "bca", "bnca", "boa", "bnoa",
        "bpa", "bnpa", "bga", "bgea", "bla", "blea", "baa", "baea", "bba", "bbea",
    ];
    OPS.get(op as usize).copied()
}

fn decode_alu_imm(op: u32, imm: u32) -> (String, bool) {
    if op <= 6 {
        let shift = 8 * ((imm >> 8) & 3);
        let value = (imm & 0xFF) << shift;
        return (fmt_imm_hex(value), true);
    }
    if op <= 13 {
        return (format!("{}", imm & 0x1F), false);
    }
    let value = sign_extend(imm & 0xFFF, 12);
    (fmt_imm_signed(value), false)
}

fn disassemble_alu_reg(instr: u32) -> String {
    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let r_c = instr & 0x1F;
    let op = (instr >> 5) & 0x1F;

    let Some(name) = alu_op_name(op) else {
        return format!("data {}", fmt_imm_hex(instr));
    };

    if op == 6 {
        return format!("{} {}, {}", name, reg_name(r_a), reg_name(r_c));
    }

    if op == 16 && r_a == 0 {
        return format!("cmp {}, {}", reg_name(r_b), reg_name(r_c));
    }

    format!(
        "{} {}, {}, {}",
        name,
        reg_name(r_a),
        reg_name(r_b),
        reg_name(r_c)
    )
}

fn disassemble_alu_imm(instr: u32) -> String {
    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let op = (instr >> 12) & 0x1F;
    let imm = instr & 0xFFF;

    let Some(name) = alu_op_name(op) else {
        return format!("data {}", fmt_imm_hex(instr));
    };

    let (imm_str, _is_hex) = decode_alu_imm(op, imm);

    if op == 6 {
        return format!("{} {}, {}", name, reg_name(r_a), imm_str);
    }

    if op == 16 && r_a == 0 {
        return format!("cmp {}, {}", reg_name(r_b), imm_str);
    }

    format!("{} {}, {}, {}", name, reg_name(r_a), reg_name(r_b), imm_str)
}

fn disassemble_lui(instr: u32) -> String {
    let r_a = (instr >> 22) & 0x1F;
    let imm = (instr & 0x3FFFFF) << 10;
    format!("lui {}, {}", reg_name(r_a), fmt_imm_hex(imm))
}

fn disassemble_mem(opcode: u32, instr: u32) -> String {
    let group = opcode - 3;
    let width_type = group / 3;
    let addr_type = group % 3;

    let (store_base, load_base) = match width_type {
        0 => ("sw", "lw"),
        1 => ("sd", "ld"),
        _ => ("sb", "lb"),
    };

    let r_a = (instr >> 22) & 0x1F;
    let is_load = if addr_type == 2 {
        ((instr >> 21) & 1) != 0
    } else {
        ((instr >> 16) & 1) != 0
    };

    let mut mnemonic = if is_load { load_base } else { store_base }.to_string();
    if addr_type == 0 {
        mnemonic.push('a');
    }

    match addr_type {
        0 => {
            let r_b = (instr >> 17) & 0x1F;
            let y = (instr >> 14) & 3;
            let z = (instr >> 12) & 3;
            let imm = sign_extend(instr & 0xFFF, 12) << z;
            let imm_str = fmt_imm_signed(imm);

            if y == 1 {
                format!(
                    "{} {}, [{}, {}]!",
                    mnemonic,
                    reg_name(r_a),
                    reg_name(r_b),
                    imm_str
                )
            } else if y == 2 {
                format!(
                    "{} {}, [{}], {}",
                    mnemonic,
                    reg_name(r_a),
                    reg_name(r_b),
                    imm_str
                )
            } else {
                format!(
                    "{} {}, [{}, {}]",
                    mnemonic,
                    reg_name(r_a),
                    reg_name(r_b),
                    imm_str
                )
            }
        }
        1 => {
            let r_b = (instr >> 17) & 0x1F;
            let imm = sign_extend(instr & 0xFFFF, 16);
            format!(
                "{} {}, [{}, {}]",
                mnemonic,
                reg_name(r_a),
                reg_name(r_b),
                fmt_imm_signed(imm)
            )
        }
        _ => {
            let imm = sign_extend(instr & 0x1FFFFF, 21);
            format!(
                "{} {}, [{}]",
                mnemonic,
                reg_name(r_a),
                fmt_imm_signed(imm)
            )
        }
    }
}

fn disassemble_branch_imm(instr: u32) -> String {
    let op = (instr >> 22) & 0x1F;
    let imm = sign_extend((instr & 0x3FFFFF) << 2, 22);
    let Some(name) = branch_name(op) else {
        return format!("data {}", fmt_imm_hex(instr));
    };
    format!("{} {}", name, fmt_imm_signed(imm))
}

fn disassemble_branch_abs(instr: u32) -> String {
    let op = (instr >> 22) & 0x1F;
    let r_a = (instr >> 5) & 0x1F;
    let r_b = instr & 0x1F;
    let Some(name) = branch_abs_name(op) else {
        return format!("data {}", fmt_imm_hex(instr));
    };
    format!("{} {}, {}", name, reg_name(r_a), reg_name(r_b))
}

fn disassemble_branch_rel(instr: u32) -> String {
    let op = (instr >> 22) & 0x1F;
    let r_a = (instr >> 5) & 0x1F;
    let r_b = instr & 0x1F;
    let Some(name) = branch_name(op) else {
        return format!("data {}", fmt_imm_hex(instr));
    };
    format!("{} {}, {}", name, reg_name(r_a), reg_name(r_b))
}

fn disassemble_sys(instr: u32) -> String {
    let imm = (instr & 0xFF) as u32;
    if imm == 1 {
        "sys EXIT".to_string()
    } else {
        format!("sys {}", imm)
    }
}

fn disassemble_atomic(opcode: u32, instr: u32) -> String {
    let is_fadd = opcode <= 18;
    let is_absolute = opcode == 16 || opcode == 19;
    let is_imm = opcode == 18 || opcode == 21;

    let mnemonic = if is_fadd {
        if is_absolute { "fada" } else { "fad" }
    } else if is_absolute {
        "swpa"
    } else {
        "swp"
    };

    let r_a = (instr >> 22) & 0x1F;
    let r_c = (instr >> 17) & 0x1F;

    if is_imm {
        let imm = sign_extend(instr & 0x1FFFF, 17);
        return format!(
            "{} {}, {}, [{}]",
            mnemonic,
            reg_name(r_a),
            reg_name(r_c),
            fmt_imm_signed(imm)
        );
    }

    let r_b = (instr >> 12) & 0x1F;
    let imm = sign_extend(instr & 0xFFF, 12);
    format!(
        "{} {}, {}, [{}, {}]",
        mnemonic,
        reg_name(r_a),
        reg_name(r_c),
        reg_name(r_b),
        fmt_imm_signed(imm)
    )
}

fn disassemble_adpc(instr: u32) -> String {
    let r_a = (instr >> 22) & 0x1F;
    let imm = sign_extend(instr & 0x3FFFFF, 22);
    format!("adpc {}, {}", reg_name(r_a), fmt_imm_signed(imm))
}

fn disassemble_kernel(instr: u32) -> String {
    let major = (instr >> 12) & 0x1F;
    match major {
        0 => {
            let op = (instr >> 10) & 3;
            let r_a = (instr >> 22) & 0x1F;
            let r_b = (instr >> 17) & 0x1F;
            match op {
                0 => format!("tlbr {}, {}", reg_name(r_a), reg_name(r_b)),
                1 => format!("tlbw {}, {}", reg_name(r_a), reg_name(r_b)),
                2 => format!("tlbi {}", reg_name(r_b)),
                _ => "tlbc".to_string(),
            }
        }
        1 => {
            let op = (instr >> 10) & 3;
            let r_a = (instr >> 22) & 0x1F;
            let r_b = (instr >> 17) & 0x1F;
            match op {
                0 => format!("crmv {}, {}", creg_name(r_a), reg_name(r_b)),
                1 => format!("crmv {}, {}", reg_name(r_a), creg_name(r_b)),
                2 => format!("crmv {}, {}", creg_name(r_a), creg_name(r_b)),
                _ => format!("crmv {}, {}", reg_name(r_a), reg_name(r_b)),
            }
        }
        2 => {
            let op = (instr >> 10) & 3;
            match op {
                0 => "mode run".to_string(),
                1 => "mode sleep".to_string(),
                _ => "mode halt".to_string(),
            }
        }
        3 => {
            let is_rfi = ((instr >> 11) & 1) != 0;
            if is_rfi { "rfi".to_string() } else { "rfe".to_string() }
        }
        4 => {
            let r_a = (instr >> 22) & 0x1F;
            let all = ((instr >> 11) & 1) != 0;
            if all {
                format!("ipi {}, all", reg_name(r_a))
            } else {
                format!("ipi {}, {}", reg_name(r_a), instr & 0x3)
            }
        }
        _ => format!("kernel {}", fmt_imm_hex(instr)),
    }
}

pub fn disassemble(instr: u32) -> String {
    let opcode = instr >> 27;
    match opcode {
        0 => disassemble_alu_reg(instr),
        1 => disassemble_alu_imm(instr),
        2 => disassemble_lui(instr),
        3..=11 => disassemble_mem(opcode, instr),
        12 => disassemble_branch_imm(instr),
        13 => disassemble_branch_abs(instr),
        14 => disassemble_branch_rel(instr),
        15 => disassemble_sys(instr),
        22 => disassemble_adpc(instr),
        16..=21 => disassemble_atomic(opcode, instr),
        31 => disassemble_kernel(instr),
        _ => format!("data {}", fmt_imm_hex(instr)),
    }
}
