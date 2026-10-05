// Pure ALU semantics for the register (opcode 00000) and immediate (opcode
// 00001) instruction forms in docs/ISA.md "ALU instructions".
//
// Flag conventions used here:
// - Carry for add/addc is the carry out of bit 31.
// - Carry for sub/subb is the carry out of `lhs + !rhs + carry_in`, so carry
//   set means "no borrow" (sub uses carry_in = 1, subb uses the current carry).
// - Carry for shifts and rotates is set when any bit shifted out of the
//   register is 1, and clear when nothing is shifted out (amount 0).
// - Logical shifts by more than 32 produce 0. A shift by exactly 32 also
//   shifts every source bit out; for lslc/lsrc the incoming carry lands in
//   bit 31/bit 0 respectively. Rotates use the amount modulo 32.
// - asr by 32 or more fills the result with the sign bit (0 or 0xFFFFFFFF).
//   docs/ISA.md does not state this case; it is the continuation of asr by 31.
// Zero/sign/overflow are derived by the caller from the result (see
// `overflow`), so they are not part of `AluOutput`.

// Highest register-form ALU op (tncd); docs/ISA.md lists ops 0..=21.
const MAX_REG_OP: u32 = 21;
// Highest immediate-form ALU op (subb); docs/ISA.md lists ops 0..=17.
const MAX_IMM_OP: u32 = 17;

pub(super) const OP_SUB: u32 = 16;
pub(super) const OP_SUBB: u32 = 17;

// Value written back plus the carry flag the operation produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AluOutput {
    pub value: u32,
    pub carry: bool,
}

// Sign-extend the low `bits` bits of `value` to 32 bits.
pub(super) fn sign_extend(value: u32, bits: u32) -> u32 {
    let shift = 32 - bits;
    (((value << shift) as i32) >> shift) as u32
}

// Decode the 12-bit immediate field of an ALU-immediate instruction for `op`.
// Bitwise ops use the `yy` byte-lane encoding, shifts use a 5-bit amount, and
// arithmetic ops sign-extend 12 bits. Returns None for ops with no immediate
// form, which must raise an invalid-instruction exception.
pub(super) fn decode_imm(op: u32, imm: u32) -> Option<u32> {
    match op {
        0..=6 => Some((imm & 0xFF) << (8 * ((imm >> 8) & 3))),
        7..=13 => Some(imm & 0x1F),
        14..=MAX_IMM_OP => Some(sign_extend(imm & 0xFFF, 12)),
        _ => None,
    }
}

// Logical left shift that yields 0 once every bit has been shifted out.
fn shl(x: u32, amount: u32) -> u32 {
    if amount >= 32 { 0 } else { x << amount }
}

// Logical right shift that yields 0 once every bit has been shifted out.
fn shr(x: u32, amount: u32) -> u32 {
    if amount >= 32 { 0 } else { x >> amount }
}

// Whether a left shift by `amount` discards any set bit from the top of `x`.
fn lost_left(x: u32, amount: u32) -> bool {
    amount != 0 && shr(x, 32 - amount.min(32)) != 0
}

// Whether a right shift by `amount` discards any set bit from the bottom of `x`.
fn lost_right(x: u32, amount: u32) -> bool {
    let mask = if amount >= 32 { u32::MAX } else { (1u32 << amount) - 1 };
    x & mask != 0
}

// `lhs + !rhs + carry_in` with the carry out of bit 31; implements sub/subb.
fn subtract(lhs: u32, rhs: u32, carry_in: bool) -> AluOutput {
    let wide = u64::from(lhs) + u64::from(!rhs) + u64::from(carry_in);
    AluOutput {
        value: wide as u32,
        carry: wide >> 32 != 0,
    }
}

// Evaluate one ALU op. `lhs` is rB and `rhs` is rC or the decoded immediate.
// Immediate sub/subb compute `rhs - lhs` (docs/ISA.md: `sub rA, rB, i` is
// `i - rB`). Returns None for undefined ops so the caller raises an
// invalid-instruction exception without touching any state.
pub(super) fn evaluate(op: u32, lhs: u32, rhs: u32, carry_in: bool, imm_form: bool) -> Option<AluOutput> {
    let max_op = if imm_form { MAX_IMM_OP } else { MAX_REG_OP };
    if op > max_op {
        return None;
    }
    let c = u32::from(carry_in);
    let plain = |value: u32| AluOutput { value, carry: false };
    let out = match op {
        0 => plain(lhs & rhs),
        1 => plain(!(lhs & rhs)),
        2 => plain(lhs | rhs),
        3 => plain(!(lhs | rhs)),
        4 => plain(lhs ^ rhs),
        5 => plain(!(lhs ^ rhs)),
        6 => plain(!rhs),
        7 => AluOutput {
            value: shl(lhs, rhs),
            carry: lost_left(lhs, rhs),
        },
        8 => AluOutput {
            value: shr(lhs, rhs),
            carry: lost_right(lhs, rhs),
        },
        9 => AluOutput {
            value: ((lhs as i32) >> rhs.min(31)) as u32,
            carry: lost_right(lhs, rhs),
        },
        10 => AluOutput {
            value: lhs.rotate_left(rhs % 32),
            carry: lost_left(lhs, rhs % 32),
        },
        11 => AluOutput {
            value: lhs.rotate_right(rhs % 32),
            carry: lost_right(lhs, rhs % 32),
        },
        12 => {
            let carry_bit = if (1..=32).contains(&rhs) { c << (rhs - 1) } else { 0 };
            AluOutput {
                value: shl(lhs, rhs) | carry_bit,
                carry: lost_left(lhs, rhs),
            }
        }
        13 => {
            let carry_bit = if (1..=32).contains(&rhs) { c << (32 - rhs) } else { 0 };
            AluOutput {
                value: shr(lhs, rhs) | carry_bit,
                carry: lost_right(lhs, rhs),
            }
        }
        14 | 15 => {
            let carry_add = if op == 15 { u64::from(c) } else { 0 };
            let wide = u64::from(lhs) + u64::from(rhs) + carry_add;
            AluOutput {
                value: wide as u32,
                carry: wide >> 32 != 0,
            }
        }
        OP_SUB | OP_SUBB => {
            let borrow_in = if op == OP_SUB { true } else { carry_in };
            if imm_form {
                subtract(rhs, lhs, borrow_in)
            } else {
                subtract(lhs, rhs, borrow_in)
            }
        }
        18 => plain(sign_extend(rhs & 0xFF, 8)),
        19 => plain(sign_extend(rhs & 0xFFFF, 16)),
        20 => plain(rhs & 0xFF),
        _ => plain(rhs & 0xFFFF),
    };
    Some(out)
}

// Overflow flag for a result whose operation was `a op b` in operand order.
// Subtractions overflow when the operands' signs differ and the result's sign
// differs from `a`; every other op uses the addition rule. Non-arithmetic ops
// also get the addition rule applied to their operands; docs/ISA.md does not
// define V for them and this matches the emulator's historical behavior.
pub(super) fn overflow(op: u32, a: u32, b: u32, result: u32) -> bool {
    let (a, b, r) = (a >> 31, b >> 31, result >> 31);
    if op == OP_SUB || op == OP_SUBB {
        r != a && a != b
    } else {
        r != a && a == b
    }
}

#[cfg(test)]
/*
Summary:
- Pins the shift/rotate carry and large-amount rules from the header comment,
  including the amount-0 and amount-32 boundaries that the original emulator
  got wrong (asr by 0 of a negative value returned 0xFFFFFFFF, lslc/lsrc by 0
  ORed in the carry, and amounts >= 32 panicked in debug builds).
- Pins immediate subb using its immediate (it used to substitute 1) and the
  `i - rB` operand order shared with immediate sub.
*/
mod tests {
    use super::*;

    // Evaluate a register-form op with a clear carry.
    fn reg(op: u32, lhs: u32, rhs: u32) -> AluOutput {
        evaluate(op, lhs, rhs, false, false).unwrap()
    }

    #[test]
    fn shift_by_zero_is_identity_with_clear_carry() {
        for op in 7..=13 {
            for carry_in in [false, true] {
                let out = evaluate(op, 0x8000_0010, 0, carry_in, false).unwrap();
                assert_eq!(out.value, 0x8000_0010, "op {op} by 0 must not change the value");
                assert!(!out.carry, "op {op} by 0 shifts nothing out, so carry must be clear");
            }
        }
    }

    #[test]
    fn carry_reflects_any_shifted_out_bit() {
        assert!(reg(7, 0x4000_0000, 2).carry);
        assert!(!reg(7, 0x2000_0000, 2).carry);
        assert!(reg(8, 0b10, 2).carry);
        assert!(!reg(8, 0b100, 2).carry);
        assert!(reg(9, 0x8000_0001, 1).carry, "asr carry comes from shifted-out low bits");
        assert!(!reg(9, 0x8000_0000, 1).carry, "asr carry must not just copy bit 0 of a wider shift");
        assert!(reg(10, 0x8000_0000, 1).carry);
        assert!(reg(11, 1, 1).carry);
    }

    #[test]
    fn logical_shifts_of_32_or_more_produce_zero() {
        for amount in [32, 33, 40, u32::MAX] {
            assert_eq!(reg(7, 0xFFFF_FFFF, amount), AluOutput { value: 0, carry: true });
            assert_eq!(reg(8, 0xFFFF_FFFF, amount), AluOutput { value: 0, carry: true });
            assert_eq!(reg(7, 0, amount), AluOutput { value: 0, carry: false });
        }
        // The incoming carry still lands in range for an amount of exactly 32.
        assert_eq!(evaluate(12, 0, 32, true, false).unwrap().value, 0x8000_0000);
        assert_eq!(evaluate(13, 0, 32, true, false).unwrap().value, 1);
        assert_eq!(evaluate(12, 0, 33, true, false).unwrap().value, 0);
        assert_eq!(evaluate(13, 0, 33, true, false).unwrap().value, 0);
    }

    #[test]
    fn asr_saturates_to_sign_fill() {
        assert_eq!(reg(9, 0x8000_0000, 31).value, 0xFFFF_FFFF);
        assert_eq!(reg(9, 0x8000_0000, 40).value, 0xFFFF_FFFF);
        assert_eq!(reg(9, 0x7FFF_FFFF, 40).value, 0);
    }

    #[test]
    fn rotates_use_amount_mod_32() {
        assert_eq!(reg(10, 0x8000_0001, 33), reg(10, 0x8000_0001, 1));
        assert_eq!(reg(11, 0x8000_0001, 64).value, 0x8000_0001);
        assert!(!reg(11, 0x8000_0001, 64).carry, "a rotate by a multiple of 32 moves nothing");
    }

    #[test]
    fn shift_through_carry_inserts_carry_in() {
        assert_eq!(evaluate(12, 0x1, 1, true, false).unwrap().value, 0x3);
        assert_eq!(evaluate(13, 0x2, 1, true, false).unwrap().value, 0x8000_0001);
    }

    #[test]
    fn immediate_subtracts_use_imm_minus_register() {
        // sub rA, rB, i => i - rB with carry meaning "no borrow".
        assert_eq!(
            evaluate(OP_SUB, 10, 50, false, true).unwrap(),
            AluOutput { value: 40, carry: true }
        );
        // subb rA, rB, i => i - rB - !carry.
        assert_eq!(evaluate(OP_SUBB, 10, 50, true, true).unwrap().value, 40);
        assert_eq!(evaluate(OP_SUBB, 10, 50, false, true).unwrap().value, 39);
        assert_eq!(evaluate(OP_SUBB, 0, 0, false, true).unwrap(), AluOutput {
            value: u32::MAX,
            carry: false,
        });
    }

    #[test]
    fn subb_borrow_chain_matches_64_bit_subtraction() {
        let (a, b): (u64, u64) = (0x0000_0001_0000_0000, 0x0000_0000_FFFF_FFFF);
        let lo = reg(OP_SUB, a as u32, b as u32);
        let hi = evaluate(OP_SUBB, (a >> 32) as u32, (b >> 32) as u32, lo.carry, false).unwrap();
        assert_eq!((u64::from(hi.value) << 32) | u64::from(lo.value), a - b);
        // Borrowing through rC = 0xFFFFFFFF must still report a borrow.
        assert!(!evaluate(OP_SUBB, 5, u32::MAX, false, false).unwrap().carry);
    }

    #[test]
    fn immediate_form_rejects_ops_without_an_immediate_encoding() {
        for op in 18..32 {
            assert_eq!(evaluate(op, 0, 0, false, true), None, "imm op {op} is not in ISA.md");
            assert_eq!(decode_imm(op, 0), None);
        }
        assert_eq!(evaluate(22, 0, 0, false, false), None);
        assert!(evaluate(21, 0, 0, false, false).is_some());
    }

    #[test]
    fn overflow_uses_operand_order() {
        // 0 - 0x80000000 overflows; the reversed order would not.
        assert!(overflow(OP_SUB, 0, 0x8000_0000, 0x8000_0000));
        assert!(!overflow(OP_SUB, 0x8000_0000, 0, 0x8000_0000));
        assert!(overflow(14, 0x7FFF_FFFF, 1, 0x8000_0000));
    }
}
