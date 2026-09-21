#[cfg(test)]
use std::fs;

#[cfg(test)]
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::process::Command;

#[cfg(test)]
use std::sync::Once;

#[cfg(test)]
use super::*;

#[cfg(test)]
use crate::emulator::{AudioMode, ScheduleMode};

// Select the assembler profile used by the emulator integration tests.
#[cfg(test)]
fn assembler_profile() -> &'static str {
    // Match the assembler build to the test binary profile.
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

// Assemble a test program and return its executable image.
#[cfg(test)]
fn build_assembler() {
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let asm_dir = manifest.join("../../Dioptase-Assembler");
        // Build the assembler once so tests can run in clean environments.
        let status = Command::new("make")
            .arg(assembler_profile())
            .current_dir(asm_dir)
            .status()
            .expect("failed to run make for assembler");
        assert!(status.success(), "assembler build failed");
    });
}

// Locate the assembler executable used to build a test image.
#[cfg(test)]
fn assembler_path() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = manifest
        .join("../../Dioptase-Assembler")
        .join("build")
        .join(assembler_profile())
        .join("basm");
    if path.exists() {
        return path;
    }
    // Build on-demand if the binary isn't present yet.
    build_assembler();
    assert!(path.exists(), "assembler not found at {}", path.display());
    path
}

// Create the generated-image directory used by instruction fixtures.
#[cfg(test)]
fn ensure_hex_dir() {
    let hex_dir = Path::new("tests/hex");
    fs::create_dir_all(hex_dir).expect("failed to create tests/hex dir");
}

// Assemble and execute one instruction-level emulator test case.
#[cfg(test)]
fn run_test(asm_file: &'static str, expected: u32) {
    ensure_hex_dir();

    // Build hex file path by replacing asm path prefix/suffix
    let hex_file = {
        let asm_path = Path::new(asm_file);
        let stem = asm_path.file_stem().unwrap(); // e.g., "add"
        PathBuf::from("tests/hex").join(format!("{}.hex", stem.to_string_lossy()))
    };

    // assemble test case
    let assembler = assembler_path();
    let status = Command::new(&assembler)
        .args([asm_file, "-o", hex_file.to_str().unwrap(), "-kernel"])
        .status()
        .expect("failed to run assembler");
    assert!(status.success(), "assembler failed");

    // execute hex file
    let cpu = Emulator::new(hex_file.to_string_lossy().to_string(), false, 1, None, None);
    let result = cpu.run(10000, false, AudioMode::Disabled);

    // check result
    assert_eq!(result, Some(expected));
}

// Assemble a kernel fixture and run it under deterministic round-robin multicore scheduling.
#[cfg(test)]
fn run_multicore_test(asm_file: &'static str, expected: u32, cores: usize) {
    ensure_hex_dir();

    let hex_file = {
        let asm_path = Path::new(asm_file);
        let stem = asm_path.file_stem().unwrap();
        PathBuf::from("tests/hex").join(format!("{}.hex", stem.to_string_lossy()))
    };

    let assembler = assembler_path();
    let status = Command::new(&assembler)
        .args([asm_file, "-o", hex_file.to_str().unwrap(), "-kernel"])
        .status()
        .expect("failed to run assembler");
    assert!(status.success(), "assembler failed");

    let result = Emulator::run_multicore(
        hex_file.to_string_lossy().to_string(),
        cores,
        ScheduleMode::RoundRobin,
        200000,
        false,
        AudioMode::Disabled,
        false,
        1,
        None,
        None,
    );
    assert_eq!(result, Some(expected));
}

// I/O tests that must be run manually (12):
// cdiv.s (run with --vga)
// colors.s (--vga)
// multicore_colors.s (--vga --cores 4)
// green.s (--vga)
// ps2.s (--vga)
// sleep.s (--vga)
// sprite.s (--vga)
// tile_colors.s (--vga)
// uart.s
// uart_rx.s (--vga --uart)
// pixels.s (--vga)
// vblank.s (--vga)
// tile_colors.s (--vga)

// Check register AND produces the expected bit mask.
#[test]
fn and() {
    run_test("tests/asm/and.s", 2);
}

// Check NAND complements the register AND result.
#[test]
fn nand() {
    run_test("tests/asm/nand.s", 0xFFFFFFFA);
}

// Check register OR combines every set input bit.
#[test]
fn or() {
    run_test("tests/asm/or.s", 0xF000000F);
}

// Check NOR complements the register OR result.
#[test]
fn nor() {
    run_test("tests/asm/nor.s", 6);
}

// Check register XOR preserves bits set in exactly one operand.
#[test]
fn xor() {
    run_test("tests/asm/xor.s", 25);
}

// Check XNOR complements the register XOR result.
#[test]
fn xnor() {
    run_test("tests/asm/xnor.s", 13);
}

// Check NOT complements every bit in its operand.
#[test]
fn not() {
    run_test("tests/asm/not.s", 1);
}

// Check logical left shift inserts zeros at the low end.
#[test]
fn lsl() {
    run_test("tests/asm/lsl.s", 0x55550);
}

// Check logical right shift inserts zeros at the high end.
#[test]
fn lsr() {
    run_test("tests/asm/lsr.s", 0xAAA);
}

// Check arithmetic right shift preserves the sign bit.
#[test]
fn asr() {
    run_test("tests/asm/asr.s", 0xF5555555);
}

// Check left shift through carry consumes and updates the carry bit.
#[test]
fn lslc() {
    run_test("tests/asm/lslc.s", 0x143);
}

// Check right shift through carry consumes and updates the carry bit.
#[test]
fn lsrc() {
    run_test("tests/asm/lsrc.s", 0xC0000028);
}

// Check register addition produces the expected sum.
#[test]
fn add() {
    run_test("tests/asm/add.s", 38);
}

// Check add-with-carry includes the incoming carry bit.
#[test]
fn addc() {
    run_test("tests/asm/addc.s", 0xAAAAAAAD);
}

// Check register subtraction produces the expected difference.
#[test]
fn sub() {
    run_test("tests/asm/sub.s", 8);
}

// Check subtract-with-borrow includes the incoming borrow state.
#[test]
fn subb() {
    run_test("tests/asm/subb.s", 0xFFFFFFFF);
}

// Verify subtraction reports signed overflow through the architectural flag.
#[test]
fn sub_overflow_sets_flag() {
    run_test("tests/asm/sub_overflow.s", 1);
}

// Check byte sign extension preserves the encoded signed value.
#[test]
fn sxtb() {
    run_test("tests/asm/sxtb.s", 0x000000FF);
}

// Check halfword sign extension preserves the encoded signed value.
#[test]
fn sxtd() {
    run_test("tests/asm/sxtd.s", 0x0000FFFF);
}

// Check byte truncation discards only the upper bits.
#[test]
fn tncb() {
    run_test("tests/asm/tncb.s", 0x00000081);
}

// Check halfword truncation discards only the upper bits.
#[test]
fn tncd() {
    run_test("tests/asm/tncd.s", 0x00008001);
}

// Check load-upper-immediate places its payload in the high bits.
#[test]
fn lui() {
    run_test("tests/asm/lui.s", 0xAA000000);
}

// Check the movi pseudo-instruction constructs a full-width constant.
#[test]
fn movi() {
    run_test("tests/asm/movi.s", 0xABABABAB);
}

// Check add-PC forms the expected PC-relative address.
#[test]
fn adpc() {
    run_test("tests/asm/adpc.s", 0);
}

// Verify the swa/lwa register-relative word round trip.
#[test]
fn mem_wa() {
    run_test("tests/asm/mem_wa.s", 0x42424242);
}

// Verify relocated lw/sw accesses preserve neighboring words.
#[test]
fn mem_wr() {
    run_test("tests/asm/mem_wr.s", 0x25);
}

// Verify the sda/lda register-relative halfword round trip.
#[test]
fn mem_da() {
    run_test("tests/asm/mem_da.s", 0x4242);
}

// Verify an interprocessor interrupt wakes a sleeping target core.
#[test]
fn multicore_ipi_wakeup() {
    run_multicore_test("tests/asm/multicore_ipi.s", 0x42, 2);
}

// Demonstrate that an unsynchronized multicore increment loses an update.
#[test]
fn multicore_non_atomic_race() {
    run_multicore_test("tests/asm/multicore_race.s", 1, 2);
}

// Verify atomic addition preserves both concurrent increments.
#[test]
fn multicore_atomic_add() {
    run_multicore_test("tests/asm/multicore_atomic.s", 2, 2);
}

// Verify relocated ld/sd accesses update only the selected halfword.
#[test]
fn mem_dr() {
    run_test("tests/asm/mem_dr.s", 0x11114444);
}

// Verify the sba/lba register-relative byte round trip.
#[test]
fn mem_ba() {
    run_test("tests/asm/mem_ba.s", 0x42);
}

// Verify relocated lb/sb accesses update only the selected byte.
#[test]
fn mem_br() {
    run_test("tests/asm/mem_br.s", 0x11111144);
}

// Check increment updates the operand by exactly one.
#[test]
fn inc() {
    run_test("tests/asm/inc.s", 0xFFFF);
}

// Check stack pseudo-operations preserve pushed values and stack position.
#[test]
fn stack() {
    run_test("tests/asm/stack.s", 0x123456);
}

// Check unsigned-above branches only when carry and zero permit it.
#[test]
fn ba() {
    run_test("tests/asm/ba.s", 1);
}

// Check unsigned-above-or-equal follows the carry condition.
#[test]
fn bae() {
    run_test("tests/asm/bae.s", 1);
}

// Check unsigned-below follows the inverse carry condition.
#[test]
fn bb() {
    run_test("tests/asm/bb.s", 1);
}

// Check unsigned-below-or-equal includes equality.
#[test]
fn bbe() {
    run_test("tests/asm/bbe.s", 1);
}

// Check branch-on-carry observes the carry flag.
#[test]
fn bc() {
    run_test("tests/asm/bc.s", 1);
}

// Check branch-on-zero observes the zero flag.
#[test]
fn bz() {
    run_test("tests/asm/bz.s", 1);
}

// Check signed-greater branches from the sign/overflow/zero flags.
#[test]
fn bg() {
    run_test("tests/asm/bg.s", 1);
}

// Check signed-greater-or-equal includes equality.
#[test]
fn bge() {
    run_test("tests/asm/bge.s", 1);
}

// Check signed-less branches from differing sign and overflow flags.
#[test]
fn bl() {
    run_test("tests/asm/bl.s", 2);
}

// Check signed-less-or-equal includes equality.
#[test]
fn ble() {
    run_test("tests/asm/ble.s", 3);
}

// Check branch-on-sign observes the sign flag.
#[test]
fn bs() {
    run_test("tests/asm/bs.s", 2);
}

// Check branch-on-no-carry rejects a set carry flag.
#[test]
fn bnc() {
    run_test("tests/asm/bnc.s", 0);
}

// Check branch-on-nonzero rejects a set zero flag.
#[test]
fn bnz() {
    run_test("tests/asm/bnz.s", 0);
}

// Check branch-on-overflow observes the overflow flag.
#[test]
fn bo() {
    run_test("tests/asm/bo.s", 0);
}

// Check branch-on-positive-sign rejects a set sign flag.
#[test]
fn bps() {
    run_test("tests/asm/bps.s", 0);
}

// Check an unconditional jump resumes at its target.
#[test]
fn jmp() {
    run_test("tests/asm/jmp.s", 0);
}

// Verify memory loads cannot change the architecturally constant r0.
#[test]
fn r0_load_invariant() {
    run_test("tests/asm/r0_load_invariant.s", 0);
}

// Check call transfers control and preserves a usable return address.
#[test]
fn call() {
    run_test("tests/asm/call.s", 42);
}

// Verify SD0 DMA initialization and block transfer through its MMIO registers.
#[test]
fn sdcard() {
    run_test("tests/asm/sdcard.s", 0);
}

// Verify the independent SD1 DMA register bank performs a block transfer.
#[test]
fn sdcard1() {
    run_test("tests/asm/sdcard1.s", 0);
}

// Check code assembled at a nonzero origin executes with correct addresses.
#[test]
fn origin() {
    run_test("tests/asm/origin.s", 21);
}

// Verify TLBC removes all cached translations.
#[test]
fn tlbc() {
    run_test("tests/asm/tlbc.s", 0);
}

// Verify TLBI invalidates only the requested virtual-page entry.
#[test]
fn tlbi() {
    run_test("tests/asm/tlbi.s", 0x2017);
}

// Verify TLBR returns the mapping stored for a virtual page.
#[test]
fn tlbr() {
    run_test("tests/asm/tlbr.s", 0xA);
}

// Verify TLBW installs translations and enforces privilege/miss behavior.
#[test]
fn tlbw() {
    run_test("tests/asm/tlbw.s", 0x43);
}

// Verify inserting beyond TLB capacity evicts an older PID-scoped entry.
#[test]
fn tlb_evict() {
    run_test("tests/asm/tlb_evict.s", 1);
}

// Verify an unmapped access vectors to the TLB-miss handler.
#[test]
fn tlb_miss() {
    run_test("tests/asm/tlb_miss.s", 2);
}

// Verify user mode cannot perform privileged memory and control operations.
#[test]
fn priv_() {
    run_test("tests/asm/priv.s", 0x15);
}

// Verify invalid instruction and privilege faults reach their vector entries.
#[test]
fn instr() {
    run_test("tests/asm/instr.s", 0x16);
}

// Verify a misaligned program counter raises the instruction fault.
#[test]
fn misaligned_pc() {
    run_test("tests/asm/misaligned_pc.s", 0x0000000D);
}

// Verify RFE resumes after a fault using the handler-adjusted EPC.
#[test]
fn rfe() {
    run_test("tests/asm/rfe.s", 0x80000044);
}

// Verify indexed and all-source EOI forms clear ISR state as specified.
#[test]
fn eoi() {
    run_test("tests/asm/eoi.s", 0);
}

// Verify a TLB miss records the faulting address in TLBA.
#[test]
fn tlb_reg() {
    run_test("tests/asm/tlb_reg.s", 0x000fffff);
}

// Verify an absent translation reports no permission bits.
#[test]
fn tlb_fault_absent() {
    run_test("tests/asm/tlb_fault_absent.s", 0);
}

// Verify a read-protection fault reports the read flag.
#[test]
fn tlb_fault_read() {
    run_test("tests/asm/tlb_fault_read.s", 0x1);
}

// Verify a write-protection fault reports the write flag.
#[test]
fn tlb_fault_write() {
    run_test("tests/asm/tlb_fault_write.s", 0x2);
}

// Verify an execute-protection fault reports the execute flag.
#[test]
fn tlb_fault_exec() {
    run_test("tests/asm/tlb_fault_exec.s", 0x4);
}

// Verify a user-protection fault reports the user flag.
#[test]
fn tlb_fault_user() {
    run_test("tests/asm/tlb_fault_user.s", 0x8);
}

// Verify a software trap vectors to its handler and returns the handler result.
#[test]
fn trap() {
    run_test("tests/asm/trap.s", 4);
}

// Verify trap entry masks global interrupts until the handler returns.
#[test]
fn trap_masks_global_interrupts_until_trap_return() {
    run_test("tests/asm/trap_imr.s", 0x80000002);
}

// Verify an invalid trap increases privilege nesting exactly once.
#[test]
fn invalid_trap_increments_psr_once() {
    run_test("tests/asm/invalid_trap_psr.s", 2);
}

// Verify the reserved alternate RFE encoding faults with one nesting increment.
#[test]
fn invalid_alt_rfe_encoding_increments_psr_once() {
    run_test("tests/asm/invalid_alt_rfe_psr.s", 2);
}

// Verify user and kernel stack-pointer banks switch with processor mode.
#[test]
fn ksp() {
    run_test("tests/asm/ksp.s", 0xA9);
}

// Verify the TLB read permission permits the intended access set.
#[test]
fn tlb_flags_r() {
    run_test("tests/asm/tlb_flags_r.s", 67);
}

// Verify the TLB write permission permits the intended access set.
#[test]
fn tlb_flags_w() {
    run_test("tests/asm/tlb_flags_w.s", 67);
}

// Verify the TLB execute permission permits instruction fetch.
#[test]
fn tlb_flags_x() {
    run_test("tests/asm/tlb_flags_x.s", 68);
}

// Verify the TLB user permission gates user-mode access.
#[test]
fn tlb_flags_u() {
    run_test("tests/asm/tlb_flags_u.s", 66);
}

// Verify a global TLB entry remains visible across PID selection.
#[test]
fn tlb_flags_g() {
    run_test("tests/asm/tlb_flags_g.s", 67);
}

// Verify atomic fetch-add returns the old word and stores the sum.
#[test]
fn atomic_fadd() {
    run_test("tests/asm/atomic_fadd.s", 0x6D);
}

// Verify atomic swap returns the old word and installs the replacement.
#[test]
fn atomic_swap() {
    run_test("tests/asm/atomic_swap.s", 0x164);
}

// Check arithmetic carry propagates through the tested instruction sequence.
#[test]
fn carry() {
    run_test("tests/asm/carry.s", 42);
}
