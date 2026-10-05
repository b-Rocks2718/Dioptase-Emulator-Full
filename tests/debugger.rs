use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

// Cargo builds the binary before integration tests and exposes its path.
fn find_emulator_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_Dioptase-Emulator-Full"))
}

// Write a uniquely named temporary .debug image.
fn write_temp_debug(contents: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!(
        "dioptase_full_debug_{}_{}.debug",
        std::process::id(),
        stamp
    ));
    fs::write(&path, contents).expect("failed to write temp debug file");
    path
}

// Exercise the common inspection commands once.
#[test]
fn debug_repl_smoke() {
    let debug_file = write_temp_debug("@00000100\n00000000\n#label start 00000400\n");
    let bin = find_emulator_bin();

    let mut child = Command::new(bin)
        .arg("--debug")
        .arg(&debug_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start emulator");

    let commands = "\
break start
r
info regs
info cregs
info tlb
info p 0x00
info v 0x00
x p 0x00 4
x v 0x00 4
set reg r1 0x10
info r1
watch w 0x00
watchs
unwatch 0x00
q
";
    {
        let mut stdin = child.stdin.take().expect("missing stdin");
        stdin
            .write_all(commands.as_bytes())
            .expect("failed to write commands");
    }

    let output = child
        .wait_with_output()
        .expect("failed to wait on emulator");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "emulator failed: {}", stderr);
    assert!(stdout.contains("Breakpoint set at 00000400"));
    assert!(stdout.contains("TLB private"));
    assert!(stdout.contains("paddr 00000000"));
    assert!(stdout.contains("vaddr 00000000"));
    assert!(stdout.contains("r1 = 00000010"));
    assert!(stdout.contains("Watchpoint set at 00000000"));
    assert!(stdout.contains("Watchpoint removed at 00000000"));

    let _ = fs::remove_file(debug_file);
}

// Run `--debug` with `commands` on stdin and return stdout.
fn run_debugger(image: &str, commands: &str) -> String {
    let debug_file = write_temp_debug(image);
    let mut child = Command::new(find_emulator_bin())
        .arg("--debug")
        .arg(&debug_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start emulator");
    child
        .stdin
        .take()
        .expect("missing stdin")
        .write_all(commands.as_bytes())
        .expect("failed to write commands");
    let output = child.wait_with_output().expect("failed to wait on emulator");
    let _ = fs::remove_file(debug_file);
    assert!(
        output.status.success(),
        "emulator failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// `c` from a breakpoint must resume past it. It used to re-report the same
// breakpoint forever because the breakpoint check ran before the step.
// Program: 0x400 add r1, r1, 1; 0x404 add r1, r1, 1; 0x408 mode halt.
#[test]
fn continue_resumes_past_current_breakpoint() {
    let stdout = run_debugger(
        "@00000100\n0842E001\n0842E001\nF8002800\n",
        "break 0x404\nr\nc\nq\n",
    );
    assert!(stdout.contains("Program halted. r1 = 00000002"), "{stdout}");
}

// Without a `q`, EOF on stdin must end the session instead of spinning.
#[test]
fn eof_ends_debug_session() {
    let stdout = run_debugger("@00000100\nF8002800\n", "breaks\n");
    assert!(stdout.contains("No breakpoints set."), "{stdout}");
}
