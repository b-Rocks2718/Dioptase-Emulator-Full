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

// Ensure hex dir.
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

// Run multicore test.
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

#[test]
fn and() {
    run_test("tests/asm/and.s", 2);
}

// Verify the nand instruction's result and architectural side effects.
#[test]
fn nand() {
    run_test("tests/asm/nand.s", 0xFFFFFFFA);
}

// Verify the or instruction's result and architectural side effects.
#[test]
fn or() {
    run_test("tests/asm/or.s", 0xF000000F);
}

// Verify the nor instruction's result and architectural side effects.
#[test]
fn nor() {
    run_test("tests/asm/nor.s", 6);
}

// Verify the xor instruction's result and architectural side effects.
#[test]
fn xor() {
    run_test("tests/asm/xor.s", 25);
}

// Verify the xnor instruction's result and architectural side effects.
#[test]
fn xnor() {
    run_test("tests/asm/xnor.s", 13);
}

// Verify the not instruction's result and architectural side effects.
#[test]
fn not() {
    run_test("tests/asm/not.s", 1);
}

// Verify the lsl instruction's result and architectural side effects.
#[test]
fn lsl() {
    run_test("tests/asm/lsl.s", 0x55550);
}

// Verify the lsr instruction's result and architectural side effects.
#[test]
fn lsr() {
    run_test("tests/asm/lsr.s", 0xAAA);
}

// Verify the asr instruction's result and architectural side effects.
#[test]
fn asr() {
    run_test("tests/asm/asr.s", 0xF5555555);
}

// Verify the lslc instruction's result and architectural side effects.
#[test]
fn lslc() {
    run_test("tests/asm/lslc.s", 0x143);
}

// Verify the lsrc instruction's result and architectural side effects.
#[test]
fn lsrc() {
    run_test("tests/asm/lsrc.s", 0xC0000028);
}

// Verify the add instruction's result and architectural side effects.
#[test]
fn add() {
    run_test("tests/asm/add.s", 38);
}

// Verify the addc instruction's result and architectural side effects.
#[test]
fn addc() {
    run_test("tests/asm/addc.s", 0xAAAAAAAD);
}

// Verify the sub instruction's result and architectural side effects.
#[test]
fn sub() {
    run_test("tests/asm/sub.s", 8);
}

// Verify the subb instruction's result and architectural side effects.
#[test]
fn subb() {
    run_test("tests/asm/subb.s", 0xFFFFFFFF);
}

// Test sub overflow sets flag.
#[test]
fn sub_overflow_sets_flag() {
    run_test("tests/asm/sub_overflow.s", 1);
}

// Verify the sxtb instruction's result and architectural side effects.
#[test]
fn sxtb() {
    run_test("tests/asm/sxtb.s", 0x000000FF);
}

// Verify the sxtd instruction's result and architectural side effects.
#[test]
fn sxtd() {
    run_test("tests/asm/sxtd.s", 0x0000FFFF);
}

// Verify the tncb instruction's result and architectural side effects.
#[test]
fn tncb() {
    run_test("tests/asm/tncb.s", 0x00000081);
}

// Verify the tncd instruction's result and architectural side effects.
#[test]
fn tncd() {
    run_test("tests/asm/tncd.s", 0x00008001);
}

// Verify the lui instruction's result and architectural side effects.
#[test]
fn lui() {
    run_test("tests/asm/lui.s", 0xAA000000);
}

// Verify the movi instruction's result and architectural side effects.
#[test]
fn movi() {
    run_test("tests/asm/movi.s", 0xABABABAB);
}

// Verify the adpc instruction's result and architectural side effects.
#[test]
fn adpc() {
    run_test("tests/asm/adpc.s", 0);
}

// Test mem wa.
#[test]
fn mem_wa() {
    run_test("tests/asm/mem_wa.s", 0x42424242);
}

// Test mem wr.
#[test]
fn mem_wr() {
    run_test("tests/asm/mem_wr.s", 0x25);
}

// Test mem da.
#[test]
fn mem_da() {
    run_test("tests/asm/mem_da.s", 0x4242);
}

// Test multicore IPI wakeup.
#[test]
fn multicore_ipi_wakeup() {
    run_multicore_test("tests/asm/multicore_ipi.s", 0x42, 2);
}

// Test multicore non atomic race.
#[test]
fn multicore_non_atomic_race() {
    run_multicore_test("tests/asm/multicore_race.s", 1, 2);
}

// Test multicore atomic add.
#[test]
fn multicore_atomic_add() {
    run_multicore_test("tests/asm/multicore_atomic.s", 2, 2);
}

// Test mem dr.
#[test]
fn mem_dr() {
    run_test("tests/asm/mem_dr.s", 0x11114444);
}

// Test mem ba.
#[test]
fn mem_ba() {
    run_test("tests/asm/mem_ba.s", 0x42);
}

// Test mem br.
#[test]
fn mem_br() {
    run_test("tests/asm/mem_br.s", 0x11111144);
}

// Verify the inc instruction's result and architectural side effects.
#[test]
fn inc() {
    run_test("tests/asm/inc.s", 0xFFFF);
}

// Verify the stack instruction's result and architectural side effects.
#[test]
fn stack() {
    run_test("tests/asm/stack.s", 0x123456);
}

// Verify the ba instruction's result and architectural side effects.
#[test]
fn ba() {
    run_test("tests/asm/ba.s", 1);
}

// Verify the bae instruction's result and architectural side effects.
#[test]
fn bae() {
    run_test("tests/asm/bae.s", 1);
}

// Verify the bb instruction's result and architectural side effects.
#[test]
fn bb() {
    run_test("tests/asm/bb.s", 1);
}

// Verify the bbe instruction's result and architectural side effects.
#[test]
fn bbe() {
    run_test("tests/asm/bbe.s", 1);
}

// Verify the bc instruction's result and architectural side effects.
#[test]
fn bc() {
    run_test("tests/asm/bc.s", 1);
}

// Verify the bz instruction's result and architectural side effects.
#[test]
fn bz() {
    run_test("tests/asm/bz.s", 1);
}

// Verify the bg instruction's result and architectural side effects.
#[test]
fn bg() {
    run_test("tests/asm/bg.s", 1);
}

// Verify the bge instruction's result and architectural side effects.
#[test]
fn bge() {
    run_test("tests/asm/bge.s", 1);
}

// Verify the bl instruction's result and architectural side effects.
#[test]
fn bl() {
    run_test("tests/asm/bl.s", 2);
}

// Verify the ble instruction's result and architectural side effects.
#[test]
fn ble() {
    run_test("tests/asm/ble.s", 3);
}

// Verify the bs instruction's result and architectural side effects.
#[test]
fn bs() {
    run_test("tests/asm/bs.s", 2);
}

// Verify the bnc instruction's result and architectural side effects.
#[test]
fn bnc() {
    run_test("tests/asm/bnc.s", 0);
}

// Verify the bnz instruction's result and architectural side effects.
#[test]
fn bnz() {
    run_test("tests/asm/bnz.s", 0);
}

// Verify the bo instruction's result and architectural side effects.
#[test]
fn bo() {
    run_test("tests/asm/bo.s", 0);
}

// Verify the bps instruction's result and architectural side effects.
#[test]
fn bps() {
    run_test("tests/asm/bps.s", 0);
}

// Verify the jmp instruction's result and architectural side effects.
#[test]
fn jmp() {
    run_test("tests/asm/jmp.s", 0);
}

// Test r0 load invariant.
#[test]
fn r0_load_invariant() {
    run_test("tests/asm/r0_load_invariant.s", 0);
}

// Verify the call instruction's result and architectural side effects.
#[test]
fn call() {
    run_test("tests/asm/call.s", 42);
}

// Verify sdcard behavior, including its architectural state transition.
#[test]
fn sdcard() {
    run_test("tests/asm/sdcard.s", 0);
}

// Verify sdcard1 behavior, including its architectural state transition.
#[test]
fn sdcard1() {
    run_test("tests/asm/sdcard1.s", 0);
}

// Verify the origin instruction's result and architectural side effects.
#[test]
fn origin() {
    run_test("tests/asm/origin.s", 21);
}

// Verify tlbc behavior, including its architectural state transition.
#[test]
fn tlbc() {
    run_test("tests/asm/tlbc.s", 0);
}

// Verify tlbi behavior, including its architectural state transition.
#[test]
fn tlbi() {
    run_test("tests/asm/tlbi.s", 0x2017);
}

// Verify tlbr behavior, including its architectural state transition.
#[test]
fn tlbr() {
    run_test("tests/asm/tlbr.s", 0xA);
}

// Verify tlbw behavior, including its architectural state transition.
#[test]
fn tlbw() {
    run_test("tests/asm/tlbw.s", 0x43);
}

// Verify TLB evict behavior, including its architectural state transition.
#[test]
fn tlb_evict() {
    run_test("tests/asm/tlb_evict.s", 1);
}

// Verify TLB miss behavior, including its architectural state transition.
#[test]
fn tlb_miss() {
    run_test("tests/asm/tlb_miss.s", 2);
}

// Verify priv behavior, including its architectural state transition.
#[test]
fn priv_() {
    run_test("tests/asm/priv.s", 0x15);
}

// Verify instr behavior, including its architectural state transition.
#[test]
fn instr() {
    run_test("tests/asm/instr.s", 0x16);
}

// Test misaligned PC.
#[test]
fn misaligned_pc() {
    run_test("tests/asm/misaligned_pc.s", 0x0000000D);
}

// Verify rfe behavior, including its architectural state transition.
#[test]
fn rfe() {
    run_test("tests/asm/rfe.s", 0x80000044);
}

// Verify EOI behavior, including its architectural state transition.
#[test]
fn eoi() {
    run_test("tests/asm/eoi.s", 0);
}

// Verify TLB reg behavior, including its architectural state transition.
#[test]
fn tlb_reg() {
    run_test("tests/asm/tlb_reg.s", 0x000fffff);
}

// Test TLB fault absent.
#[test]
fn tlb_fault_absent() {
    run_test("tests/asm/tlb_fault_absent.s", 0);
}

// Test TLB fault read.
#[test]
fn tlb_fault_read() {
    run_test("tests/asm/tlb_fault_read.s", 0x1);
}

// Test TLB fault write.
#[test]
fn tlb_fault_write() {
    run_test("tests/asm/tlb_fault_write.s", 0x2);
}

// Test TLB fault exec.
#[test]
fn tlb_fault_exec() {
    run_test("tests/asm/tlb_fault_exec.s", 0x4);
}

// Test TLB fault user.
#[test]
fn tlb_fault_user() {
    run_test("tests/asm/tlb_fault_user.s", 0x8);
}

// Verify trap behavior, including its architectural state transition.
#[test]
fn trap() {
    run_test("tests/asm/trap.s", 4);
}

// Test trap masks global interrupts until trap return.
#[test]
fn trap_masks_global_interrupts_until_trap_return() {
    run_test("tests/asm/trap_imr.s", 0x80000002);
}

// Test invalid trap increments PSR once.
#[test]
fn invalid_trap_increments_psr_once() {
    run_test("tests/asm/invalid_trap_psr.s", 2);
}

// Test invalid alt rfe encoding increments PSR once.
#[test]
fn invalid_alt_rfe_encoding_increments_psr_once() {
    run_test("tests/asm/invalid_alt_rfe_psr.s", 2);
}

// Verify ksp behavior, including its architectural state transition.
#[test]
fn ksp() {
    run_test("tests/asm/ksp.s", 0xA9);
}

// Test TLB flags r.
#[test]
fn tlb_flags_r() {
    run_test("tests/asm/tlb_flags_r.s", 67);
}

// Test TLB flags w.
#[test]
fn tlb_flags_w() {
    run_test("tests/asm/tlb_flags_w.s", 67);
}

// Test TLB flags x.
#[test]
fn tlb_flags_x() {
    run_test("tests/asm/tlb_flags_x.s", 68);
}

// Test TLB flags u.
#[test]
fn tlb_flags_u() {
    run_test("tests/asm/tlb_flags_u.s", 66);
}

// Test TLB flags g.
#[test]
fn tlb_flags_g() {
    run_test("tests/asm/tlb_flags_g.s", 67);
}

// Test atomic fadd.
#[test]
fn atomic_fadd() {
    run_test("tests/asm/atomic_fadd.s", 0x6D);
}

// Test atomic swap.
#[test]
fn atomic_swap() {
    run_test("tests/asm/atomic_swap.s", 0x164);
}

// Verify the carry instruction's result and architectural side effects.
#[test]
fn carry() {
    run_test("tests/asm/carry.s", 42);
}
