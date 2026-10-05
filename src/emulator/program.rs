// Loader for assembler `.hex` images and the debug metadata embedded in them.
//
// Image format (Dioptase-Assembler output): one little-endian 32-bit word per
// line in hex, `@<word address>` lines to move the load cursor, `;` or `//`
// comment lines, and `#`-prefixed metadata lines emitted with `-g`/`--debug`:
//   #label <name> <addr>
//   #line  <file> <line> <addr>
//   #local <name> <bp offset> [<size>] <addr>
//   #data  <name> <addr>

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;

// Label -> address list (labels can appear multiple times across sections).
pub(super) type LabelMap = HashMap<String, Vec<u32>>;

// Source line marker emitted by the assembler debug pipeline.
#[derive(Clone, Debug)]
pub(super) struct DebugLine {
    pub file: String,
    pub line: u32,
    pub addr: u32,
}

// Stack local debug metadata anchored to a code address.
#[derive(Clone, Debug)]
pub(super) struct DebugLocal {
    pub name: String,
    pub offset: i32,
    pub size: u32,
}

// Global data symbol debug metadata.
#[derive(Clone, Debug)]
pub(super) struct DebugGlobal {
    pub name: String,
    pub addr: u32,
}

// Aggregated C debug info parsed from an image. The `missing_*` flags record
// metadata from older assemblers that omitted fields, so the C debugger can
// warn instead of silently showing partial information.
#[derive(Clone, Debug, Default)]
pub(super) struct DebugInfo {
    pub lines: Vec<DebugLine>,
    pub locals_by_addr: HashMap<u32, Vec<DebugLocal>>,
    pub globals: Vec<DebugGlobal>,
    pub missing_line_addrs: bool,
    pub missing_local_addrs: bool,
    pub missing_local_sizes: bool,
}

// Loader output: sparse initial RAM bytes, labels, and C debug metadata.
#[derive(Clone)]
pub(super) struct ProgramImage {
    pub bytes: HashMap<u32, u8>,
    pub labels: LabelMap,
    pub debug: DebugInfo,
}

// Open a text file for line-by-line reading.
pub(super) fn read_lines<P: AsRef<Path>>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>> {
    Ok(io::BufReader::new(File::open(filename)?).lines())
}

// Parse a hexadecimal 32-bit word, with or without a 0x prefix.
fn parse_hex_u32(token: &str) -> Option<u32> {
    let s = token.trim();
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
    if s.is_empty() {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

// Parse a "#label <name> <addr>" line into `labels`; returns whether it was one.
pub(super) fn parse_label_line(line: &str, labels: &mut LabelMap) -> bool {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("#label") {
        return false;
    }
    let (Some(name), Some(addr)) = (parts.next(), parts.next().and_then(parse_hex_u32)) else {
        return false;
    };
    let entry = labels.entry(name.to_string()).or_default();
    if !entry.contains(&addr) {
        entry.push(addr);
    }
    true
}

// Parse a `#line`, `#local`, or `#data` line into `debug`; returns whether the
// line used one of those tags (malformed entries are recorded as missing data).
pub(super) fn parse_debug_line(line: &str, debug: &mut DebugInfo) -> bool {
    const DEFAULT_LOCAL_SIZE_BYTES: u32 = 4;
    let mut parts = line.split_whitespace();
    match parts.next() {
        Some("#line") => {
            let (Some(file), Some(line_str)) = (parts.next(), parts.next()) else {
                return true;
            };
            match (line_str.parse::<u32>(), parts.next().and_then(parse_hex_u32)) {
                (Ok(line), Some(addr)) => debug.lines.push(DebugLine {
                    file: file.to_string(),
                    line,
                    addr,
                }),
                _ => debug.missing_line_addrs = true,
            }
            true
        }
        Some("#local") => {
            let (Some(name), Some(offset_str)) = (parts.next(), parts.next()) else {
                return true;
            };
            let Ok(offset) = offset_str.parse::<i32>() else {
                debug.missing_local_addrs = true;
                return true;
            };
            // Older assemblers emitted "#local <name> <offset> <addr>" with no size.
            let remaining: Vec<&str> = parts.collect();
            let (size, addr_str) = match remaining.as_slice() {
                [] => {
                    debug.missing_local_sizes = true;
                    debug.missing_local_addrs = true;
                    return true;
                }
                [addr_only] => {
                    debug.missing_local_sizes = true;
                    (DEFAULT_LOCAL_SIZE_BYTES, *addr_only)
                }
                [size_str, addr_str, ..] => match size_str.parse::<u32>() {
                    Ok(size) if size > 0 => (size, *addr_str),
                    _ => {
                        debug.missing_local_sizes = true;
                        (DEFAULT_LOCAL_SIZE_BYTES, *addr_str)
                    }
                },
            };
            let Some(addr) = parse_hex_u32(addr_str) else {
                debug.missing_local_addrs = true;
                return true;
            };
            debug.locals_by_addr.entry(addr).or_default().push(DebugLocal {
                name: name.to_string(),
                offset,
                size,
            });
            true
        }
        Some("#data") => {
            let (Some(name), Some(addr)) = (parts.next(), parts.next().and_then(parse_hex_u32)) else {
                return true;
            };
            if !debug.globals.iter().any(|g| g.name == name && g.addr == addr) {
                debug.globals.push(DebugGlobal {
                    name: name.to_string(),
                    addr,
                });
            }
            true
        }
        _ => false,
    }
}

// Load a hex image and collect any embedded labels and debug metadata.
pub(super) fn load_program(path: &str) -> Result<ProgramImage, String> {
    let lines = read_lines(path)
        .map_err(|err| format!("Loader: failed to open program image {}: {}", path, err))?;
    let mut image = ProgramImage {
        bytes: HashMap::new(),
        labels: LabelMap::new(),
        debug: DebugInfo::default(),
    };
    let mut addr: u32 = 0;
    for (index, line) in lines.enumerate() {
        let line_no = index + 1;
        let line = line
            .map_err(|err| format!("Loader: failed to read {} line {}: {}", path, line_no, err))?;
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with("//") {
            continue;
        }
        if line.starts_with('#') {
            parse_label_line(line, &mut image.labels);
            parse_debug_line(line, &mut image.debug);
            continue;
        }
        if let Some(rest) = line.strip_prefix('@') {
            let word_addr = u32::from_str_radix(rest.trim(), 16).map_err(|_| {
                format!(
                    "Loader: expected a hex word address after '@' at {}:{}, found '{}'",
                    path, line_no, line
                )
            })?;
            addr = word_addr.wrapping_mul(4);
            continue;
        }
        let word = u32::from_str_radix(line, 16).map_err(|_| {
            format!(
                "Loader: expected a 32-bit hex word at {}:{}, found '{}'",
                path, line_no, line
            )
        })?;
        for (offset, byte) in word.to_le_bytes().into_iter().enumerate() {
            image.bytes.insert(addr.wrapping_add(offset as u32), byte);
        }
        addr = addr.wrapping_add(4);
    }
    Ok(image)
}
