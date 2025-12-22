use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Once;
use std::time::{SystemTime, UNIX_EPOCH};

// Search for the emulator binary in Cargo's env vars and target dirs.
fn locate_emulator_bin() -> (Option<PathBuf>, Vec<PathBuf>) {
  let name = "Dioptase-Emulator-Full";
  let direct = format!("CARGO_BIN_EXE_{}", name);
  if let Ok(val) = std::env::var(&direct) {
    let path = PathBuf::from(val);
    if path.exists() {
      return (Some(path), Vec::new());
    }
  }
  let underscored = format!("CARGO_BIN_EXE_{}", name.replace('-', "_"));
  if let Ok(val) = std::env::var(&underscored) {
    let path = PathBuf::from(val);
    if path.exists() {
      return (Some(path), Vec::new());
    }
  }

  let mut name_candidates = Vec::new();
  name_candidates.push(name.to_string());
  name_candidates.push(name.to_ascii_lowercase());
  name_candidates.push(name.replace('-', "_"));
  name_candidates.push(name.replace('-', "_").to_ascii_lowercase());

  let mut dirs = Vec::new();
  if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
    dirs.push(PathBuf::from(dir));
  }
  if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
    let manifest = PathBuf::from(dir);
    dirs.push(manifest.join("target"));
    if let Some(parent) = Path::new(&manifest).parent() {
      dirs.push(parent.join("target"));
    }
  }

  let mut tried = Vec::new();
  for dir in dirs {
    for name in &name_candidates {
      for suffix in ["", ".exe"] {
        let candidate = dir.join("debug").join(format!("{}{}", name, suffix));
        if candidate.exists() {
          return (Some(candidate), tried);
        }
        tried.push(candidate);
      }
    }
  }

  (None, tried)
}

// Build the emulator binary once if it isn't found.
fn build_emulator_bin() {
  static BUILD: Once = Once::new();
  BUILD.call_once(|| {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
      .expect("CARGO_MANIFEST_DIR not set");
    let status = Command::new("cargo")
      .args(["build", "--bin", "Dioptase-Emulator-Full"])
      .current_dir(manifest_dir)
      .status()
      .expect("failed to run cargo build");
    assert!(status.success(), "failed to build emulator binary");
  });
}

fn find_emulator_bin() -> PathBuf {
  let (found, _) = locate_emulator_bin();
  if let Some(path) = found {
    return path;
  }

  // If the test runs without a built binary, compile it on demand.
  build_emulator_bin();
  let (found, tried) = locate_emulator_bin();
  if let Some(path) = found {
    return path;
  }

  let tried_list = tried
    .iter()
    .map(|p| p.display().to_string())
    .collect::<Vec<_>>()
    .join("\n");
  panic!(
    "Missing emulator binary env var for Dioptase-Emulator-Full\nTried:\n{}",
    tried_list
  );
}

fn write_temp_debug(contents: &str) -> PathBuf {
  let stamp = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .expect("time went backwards")
    .as_nanos();
  let mut path = std::env::temp_dir();
  path.push(format!("dioptase_full_debug_{}_{}.debug", std::process::id(), stamp));
  fs::write(&path, contents).expect("failed to write temp debug file");
  path
}

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
info p 0x400
info v 0x400
x p 0x400 4
x v 0x400 4
set reg r1 0x10
info r1
watch w 0x400
watchs
unwatch 0x400
q
";
  {
    let mut stdin = child.stdin.take().expect("missing stdin");
    stdin.write_all(commands.as_bytes()).expect("failed to write commands");
  }

  let output = child.wait_with_output().expect("failed to wait on emulator");
  let stdout = String::from_utf8_lossy(&output.stdout);
  let stderr = String::from_utf8_lossy(&output.stderr);

  assert!(output.status.success(), "emulator failed: {}", stderr);
  assert!(stdout.contains("Breakpoint set at 00000400"));
  assert!(stdout.contains("TLB private"));
  assert!(stdout.contains("paddr 00000400"));
  assert!(stdout.contains("vaddr 00000400"));
  assert!(stdout.contains("r1 = 00000010"));
  assert!(stdout.contains("Watchpoint set at 00000400"));
  assert!(stdout.contains("Watchpoint removed at 00000400"));

  let _ = fs::remove_file(debug_file);
}
