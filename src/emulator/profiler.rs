// Instruction-level profiler for the full emulator (`--profile`).
//
// Every instruction a core dispatches to `execute` is counted exactly (there is
// no sampling), keyed by the PC it was fetched from and the address space it
// ran in. Kernel-mode PCs share one address space. User-mode PCs are keyed by
// the PID control register, because each process has its own virtual layout.
//
// What the counts mean:
// - They are dispatched instructions, not hardware cycles. The emulator does
//   not model caches or pipeline stalls, so every instruction costs the same.
// - An instruction that raises a synchronous exception (e.g. a TLB miss on a
//   load) is counted when it is dispatched and again when it is retried after
//   the handler returns with `rfe`.
// - Ticks where the core is asleep (`mode sleep`, or a secondary core waiting
//   for its first IPI) are counted separately so idle time is visible.
//
// Symbolization uses the `#label` / `#line` / `#data` metadata that the
// assembler emits with `-g`. Symbols only apply to kernel-mode PCs; user-mode
// PCs are reported per PID without symbols because user programs are loaded
// from the filesystem and their symbol files are not known to the emulator.
// PIDs are printed in hex: the value is whatever the OS writes to the PID
// control register, which Dioptase-OS sets to an address-like value.
//
// Measurement window (`--profile-start` / `--profile-stop`): counting can be
// limited to an interval of the run. The window is system-wide: one shared
// flag gates every core, so while it is open idle cores are counted too. It
// opens when any core dispatches a start trigger and closes when any core
// dispatches a stop trigger, and may open and close repeatedly; the report
// covers the union of all open intervals and says how many times it opened.
//
// Kernel entry attribution: each core remembers the kernel PC it entered
// through on its most recent user->kernel transition (the trap, interrupt, or
// exception handler entry). Kernel instructions are attributed to that entry
// until the core returns to user mode or goes to sleep; kernel work with no
// such entry (boot, idle loops, kernel threads) is reported separately. This
// separates "kernel work done because user code entered the kernel" from
// background kernel work, and gives a per-entry average cost.
//
// Call edges: a kernel-mode call is detected when the dispatched instruction's
// link register (r29, per docs/abi.md) equals the previous instruction's PC + 4
// and execution did not simply fall through. Call counts are exact. The report
// estimates how much of a callee's self time each caller is responsible for by
// splitting it in proportion to call counts (the gprof approximation), which
// is accurate for helpers whose cost per call is roughly constant.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{DebugInfo, KERNEL_STACK_END, LabelMap, parse_debug_line, parse_label_line, read_lines};
use crate::disassembler::disassemble;

// Kernel-mode PCs below this bound are counted in a dense array indexed by
// word. Per Dioptase-OS/docs/kernel_mem_map.md, everything below 0x100000
// (the top of the core 0 kernel stack) is the IVT, BIOS, and kernel image, so
// all BIOS and kernel code runs below it. Kernel-mode PCs at or above it (and
// all user-mode PCs) fall back to a hash map, so the bound affects speed only.
// Cost: (4 MiB / 4) words * 12 bytes = 3 MiB per profiled core.
const DENSE_PC_LIMIT: u32 = KERNEL_STACK_END;
const DENSE_SLOTS: usize = (DENSE_PC_LIMIT / 4) as usize;

// How many rows the "hot source lines" and "hot instructions" sections print.
// The function table is never truncated so it can be grepped in full.
const REPORT_TOP_LINES: usize = 40;
const REPORT_TOP_INSTRUCTIONS: usize = 40;
// Rows per core in the per-core breakdown; enough to see what a core is doing.
const REPORT_TOP_FUNCTIONS_PER_CORE: usize = 10;
// Rows in each context's function table, and how many of those functions get
// a callers breakdown (with at most REPORT_CALLERS_PER_FUNCTION callers each).
const REPORT_TOP_FUNCTIONS_PER_CONTEXT: usize = 30;
const REPORT_FUNCTIONS_WITH_CALLERS: usize = 15;
const REPORT_CALLERS_PER_FUNCTION: usize = 6;
// Link register written by `call` (docs/abi.md: r29 holds the return address).
pub const LINK_REGISTER: usize = 29;
// Kernel work contexts, used to index per-context tables.
const CONTEXT_BACKGROUND: usize = 0;
const CONTEXT_FROM_USER: usize = 1;
const CONTEXT_COUNT: usize = 2;

// Instructions and entry count attributed to one kernel entry point.
#[derive(Clone, Copy, Debug, Default)]
struct EntryStats {
    entries: u64,
    instructions: u64,
}

// Address space an instruction was fetched from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum AddrSpace {
    Kernel,
    User(u32),
}

// Execution count for one PC plus the last instruction word seen there. The
// word is captured at dispatch time because code memory can be overwritten
// later (e.g. the kernel reusing the BIOS region), so reading memory at report
// time could disassemble the wrong instruction.
#[derive(Clone, Copy, Debug, Default)]
struct PcCount {
    count: u64,
    instr: u32,
}

// What opens the measurement window.
pub enum WindowStart {
    // Open from the first tick; only a stop trigger can close it.
    RunStart,
    // Open when any core dispatches a user-mode instruction.
    UserMode,
    // Open when any core dispatches a kernel-mode instruction at one of these PCs.
    KernelPcs(Vec<u32>),
}

// Shared open/closed state for the measurement window. Every core holds an
// Arc to the same instance.
//
// Ordering: SeqCst is used for simplicity. The flag only gates statistics, so
// a core may observe a transition made by another core a tick late; that
// skews counts by at most one instruction per core per transition and never
// affects emulated behavior.
pub struct ProfileWindow {
    start: WindowStart,
    // Kernel-mode PCs that close the window. The trigger instruction itself is
    // not counted; a start trigger instruction is counted.
    stop: Vec<u32>,
    description: String,
    open: AtomicBool,
    openings: AtomicU64,
}

impl ProfileWindow {
    // A window that is open for the whole run.
    pub fn whole_run() -> Arc<ProfileWindow> {
        ProfileWindow::new(WindowStart::RunStart, Vec::new(), "whole run".to_string())
    }

    pub fn new(start: WindowStart, stop: Vec<u32>, description: String) -> Arc<ProfileWindow> {
        let open = matches!(start, WindowStart::RunStart);
        Arc::new(ProfileWindow {
            start,
            stop,
            description,
            open: AtomicBool::new(open),
            openings: AtomicU64::new(open as u64),
        })
    }

    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    // Apply any trigger at this instruction and return whether it is counted.
    #[inline]
    fn observe(&self, pc: u32, kmode: bool) -> bool {
        if self.open.load(Ordering::SeqCst) {
            if kmode && self.stop.contains(&pc) {
                self.open.store(false, Ordering::SeqCst);
                return false;
            }
            return true;
        }
        let opens = match &self.start {
            // RunStart only reaches here after a stop trigger closed it.
            WindowStart::RunStart => false,
            WindowStart::UserMode => !kmode,
            WindowStart::KernelPcs(pcs) => kmode && pcs.contains(&pc),
        };
        if opens && !self.open.swap(true, Ordering::SeqCst) {
            self.openings.fetch_add(1, Ordering::SeqCst);
        }
        opens
    }
}

// Profile state owned by one emulated core. Each core records into its own
// profile with no locking; profiles are merged only when the report is built.
pub struct CoreProfile {
    core_id: u32,
    window: Arc<ProfileWindow>,
    ticks: u64,
    asleep_ticks: u64,
    kernel_instructions: u64,
    user_instructions: u64,
    // Kernel-mode PCs below DENSE_PC_LIMIT, indexed by pc / 4.
    dense_counts: Vec<u64>,
    dense_instrs: Vec<u32>,
    // Everything else, keyed by (address space, pc).
    sparse: HashMap<(AddrSpace, u32), PcCount>,

    // Entry attribution state, tracked even while the window is closed so it
    // is correct the moment the window opens.
    prev_kmode: bool,
    prev_pc: u32,
    entry_pc: Option<u32>,
    // Counted instructions for `entry_pc` not yet added to `entry_stats`
    // (flushed when the entry changes, to keep hashing off the hot path).
    entry_pending: u64,
    // Kernel instructions attributed to a user entry, by PC. Background
    // counts are the total (dense_counts / sparse) minus these.
    dense_from_user: Vec<u64>,
    sparse_from_user: HashMap<u32, u64>,
    entry_stats: HashMap<u32, EntryStats>,
    // (call site pc, callee pc) -> calls, per context.
    calls: [HashMap<(u32, u32), u64>; CONTEXT_COUNT],
    // Every kernel call target seen at any time, used to tell real functions
    // from jump-only assembly labels.
    called_ever: HashSet<u32>,
}

impl CoreProfile {
    pub fn new(core_id: u32, window: Arc<ProfileWindow>) -> CoreProfile {
        CoreProfile {
            core_id,
            window,
            ticks: 0,
            asleep_ticks: 0,
            kernel_instructions: 0,
            user_instructions: 0,
            dense_counts: vec![0; DENSE_SLOTS],
            dense_instrs: vec![0; DENSE_SLOTS],
            sparse: HashMap::new(),
            // Cores start in kernel mode (PSR = 1 at reset).
            prev_kmode: true,
            prev_pc: 0,
            entry_pc: None,
            entry_pending: 0,
            dense_from_user: vec![0; DENSE_SLOTS],
            sparse_from_user: HashMap::new(),
            entry_stats: HashMap::new(),
            calls: [HashMap::new(), HashMap::new()],
            called_ever: HashSet::new(),
        }
    }

    // Record one emulator tick if the window is open at the end of it, so a
    // tick whose instruction opened the window is counted and one whose
    // instruction closed it is not. `asleep` is the core's sleep state after
    // interrupt delivery for this tick, i.e. whether it skipped fetch.
    #[inline]
    pub fn record_tick(&mut self, asleep: bool) {
        if asleep {
            // A sleeping core is no longer working for the user code that
            // last entered the kernel.
            self.set_entry(None);
        }
        if !self.window.is_open() {
            return;
        }
        self.ticks += 1;
        if asleep {
            self.asleep_ticks += 1;
        }
    }

    // Record one dispatched instruction. `kmode`, `pid`, and `link` (r29) must
    // be sampled before `execute` runs, because the instruction itself may
    // change them (`rfe`, `trap`, `mov pid, ...`, `call`).
    #[inline]
    pub fn record_instruction(&mut self, pc: u32, instr: u32, kmode: bool, pid: u32, link: u32) {
        let counted = self.window.observe(pc, kmode);

        let entered = kmode && !self.prev_kmode;
        if entered {
            self.set_entry(Some(pc));
            if counted {
                self.entry_stats.entry(pc).or_default().entries += 1;
            }
        } else if !kmode {
            self.set_entry(None);
        }
        let is_call = kmode
            && self.prev_kmode
            && link == self.prev_pc.wrapping_add(4)
            && pc != self.prev_pc.wrapping_add(4);
        let call_site = self.prev_pc;
        self.prev_pc = pc;
        self.prev_kmode = kmode;
        if is_call {
            self.called_ever.insert(pc);
        }

        if !counted {
            return;
        }
        if kmode {
            let context = if self.entry_pc.is_some() {
                self.entry_pending += 1;
                if pc < DENSE_PC_LIMIT {
                    self.dense_from_user[(pc >> 2) as usize] += 1;
                } else {
                    *self.sparse_from_user.entry(pc).or_default() += 1;
                }
                CONTEXT_FROM_USER
            } else {
                CONTEXT_BACKGROUND
            };
            if is_call {
                *self.calls[context].entry((call_site, pc)).or_default() += 1;
            }
            self.kernel_instructions += 1;
            if pc < DENSE_PC_LIMIT {
                // Fetch rejects misaligned PCs, so pc / 4 is exact.
                let slot = (pc >> 2) as usize;
                self.dense_counts[slot] += 1;
                self.dense_instrs[slot] = instr;
                return;
            }
        } else {
            self.user_instructions += 1;
        }
        let space = if kmode {
            AddrSpace::Kernel
        } else {
            AddrSpace::User(pid)
        };
        let entry = self.sparse.entry((space, pc)).or_default();
        entry.count += 1;
        entry.instr = instr;
    }

    fn instructions(&self) -> u64 {
        self.kernel_instructions + self.user_instructions
    }

    // Switch the current kernel entry, flushing the pending count first.
    #[inline]
    fn set_entry(&mut self, entry: Option<u32>) {
        if self.entry_pc == entry {
            return;
        }
        if let Some(old) = self.entry_pc {
            if self.entry_pending != 0 {
                self.entry_stats.entry(old).or_default().instructions += self.entry_pending;
            }
        }
        self.entry_pending = 0;
        self.entry_pc = entry;
    }

    // Entry statistics including the not-yet-flushed current entry.
    fn entry_totals(&self) -> HashMap<u32, EntryStats> {
        let mut totals = self.entry_stats.clone();
        if let Some(entry) = self.entry_pc {
            totals.entry(entry).or_default().instructions += self.entry_pending;
        }
        totals
    }

    // Visit every kernel PC with nonzero count as (pc, background, from_user).
    fn for_each_kernel_pc_by_context(&self, mut visit: impl FnMut(u32, u64, u64)) {
        for (slot, &count) in self.dense_counts.iter().enumerate() {
            if count != 0 {
                let from_user = self.dense_from_user[slot];
                visit((slot as u32) << 2, count - from_user, from_user);
            }
        }
        for (&(space, pc), entry) in &self.sparse {
            if space == AddrSpace::Kernel {
                let from_user = self.sparse_from_user.get(&pc).copied().unwrap_or(0);
                visit(pc, entry.count - from_user, from_user);
            }
        }
    }

    // Visit every PC with a nonzero count.
    fn for_each_pc(&self, mut visit: impl FnMut(AddrSpace, u32, PcCount)) {
        for (slot, &count) in self.dense_counts.iter().enumerate() {
            if count != 0 {
                let pc = (slot as u32) << 2;
                visit(
                    AddrSpace::Kernel,
                    pc,
                    PcCount {
                        count,
                        instr: self.dense_instrs[slot],
                    },
                );
            }
        }
        for (&(space, pc), &entry) in &self.sparse {
            visit(space, pc, entry);
        }
    }
}

// A source line marker from `#line <file> <line> <addr>`.
struct LineEntry {
    addr: u32,
    file: String,
    line: u32,
}

// Kernel symbol tables loaded from one or more `-g` hex files.
pub struct Symbols {
    files: Vec<String>,
    // Every label (including dotted locals), for resolving window triggers.
    labels: LabelMap,
    // Function start addresses (ascending) and their display names. Several
    // labels at the same address are joined with '/'.
    functions: Vec<(u32, String)>,
    // Source line markers sorted by address.
    lines: Vec<LineEntry>,
}

impl Symbols {
    // Load the `#label`, `#line`, and `#data` metadata from each hex file.
    //
    // A label counts as a function start unless it contains '.' (the compiler
    // and assembler use dotted names for local labels such as
    // `kernel_main.end.0` and `string.label.12`) or it names a `#data` global.
    // Plain labels inside hand-written assembly also count, so assembly
    // routines may be split at their internal labels.
    pub fn load(paths: &[String]) -> Result<Symbols, String> {
        let mut labels = LabelMap::new();
        let mut debug = DebugInfo::default();
        for path in paths {
            let lines = read_lines(path).map_err(|err| {
                format!("Profiler: failed to read symbol file {}: {}", path, err)
            })?;
            for line in lines.map_while(Result::ok) {
                let line = line.trim();
                if line.starts_with('#') {
                    parse_label_line(line, &mut labels);
                    parse_debug_line(line, &mut debug);
                }
            }
        }

        let globals: HashSet<(&str, u32)> = debug
            .globals
            .iter()
            .map(|g| (g.name.as_str(), g.addr))
            .collect();
        let mut by_addr: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
        for (name, addrs) in &labels {
            if name.contains('.') {
                continue;
            }
            for &addr in addrs {
                if !globals.contains(&(name.as_str(), addr)) {
                    by_addr.entry(addr).or_default().push(name);
                }
            }
        }
        let functions = by_addr
            .into_iter()
            .map(|(addr, mut names)| {
                names.sort_unstable();
                (addr, names.join("/"))
            })
            .collect();

        let mut lines: Vec<LineEntry> = debug
            .lines
            .into_iter()
            .map(|l| LineEntry {
                addr: l.addr,
                file: l.file,
                line: l.line,
            })
            .collect();
        // Stable sort keeps the assembler's order among markers at one address,
        // so lookups resolve to the last marker emitted for that address.
        lines.sort_by_key(|l| l.addr);

        Ok(Symbols {
            files: paths.to_vec(),
            labels,
            functions,
            lines,
        })
    }

    // Resolve a window trigger given as a label name or a 0x-prefixed
    // address. A label defined at several addresses triggers at all of them.
    pub fn resolve_trigger(&self, flag: &str, token: &str) -> Result<Vec<u32>, String> {
        if let Some(hex) = token.strip_prefix("0x").or_else(|| token.strip_prefix("0X")) {
            return u32::from_str_radix(hex, 16).map(|addr| vec![addr]).map_err(|_| {
                format!("Profiler: {} expects a label or 0x-prefixed hex address, got {}", flag, token)
            });
        }
        match self.labels.get(token) {
            Some(addrs) => Ok(addrs.clone()),
            None => Err(format!(
                "Profiler: {} label {} is not defined in the symbol files ({}). Pass the kernel .hex built with -g via --symbols.",
                flag,
                token,
                self.files.join(", ")
            )),
        }
    }

    // Index of the function containing `pc`: the nearest function start at or
    // below it.
    fn function_index(&self, pc: u32) -> Option<usize> {
        let after = self.functions.partition_point(|&(addr, _)| addr <= pc);
        after.checked_sub(1)
    }

    // Source line for `pc`: the nearest line marker at or below it, as long
    // as that marker is not before the start of pc's function. The bound stops
    // an assembly routine from inheriting the last line of a preceding C
    // function.
    fn line_for(&self, pc: u32) -> Option<&LineEntry> {
        let after = self.lines.partition_point(|l| l.addr <= pc);
        let entry = &self.lines[after.checked_sub(1)?];
        if let Some(idx) = self.function_index(pc) {
            if entry.addr < self.functions[idx].0 {
                return None;
            }
        }
        Some(entry)
    }

    // Format `pc` as `function+0xoffset` for the hot instruction table.
    fn describe_pc(&self, space: AddrSpace, pc: u32) -> String {
        match space {
            AddrSpace::User(_) => String::new(),
            AddrSpace::Kernel => match self.function_index(pc) {
                Some(idx) => {
                    let (start, name) = &self.functions[idx];
                    format!("{}+0x{:x}", name, pc - start)
                }
                None => NO_SYMBOL.to_string(),
            },
        }
    }
}

// Bucket name for kernel PCs below the first known function start.
const NO_SYMBOL: &str = "[kernel: no symbol]";

// Row key for the function table.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum FunctionKey {
    Kernel(Option<usize>),
    User(u32),
}

fn percent(count: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        100.0 * count as f64 / total as f64
    }
}

// Sort rows by count (descending), then by key so reports are deterministic.
fn sort_by_count_desc<K: Ord>(rows: &mut [(K, u64)]) {
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
}

// Group PC counts into function-table rows (kernel PCs by containing symbol,
// user PCs by PID), sorted by count.
fn group_by_function(
    pcs: impl Iterator<Item = ((AddrSpace, u32), u64)>,
    symbols: &Symbols,
    groups: &[usize],
) -> Vec<(String, u64)> {
    let mut by_function: HashMap<FunctionKey, u64> = HashMap::new();
    for ((space, pc), count) in pcs {
        let key = match space {
            AddrSpace::Kernel => FunctionKey::Kernel(symbols.function_index(pc).map(|i| groups[i])),
            AddrSpace::User(pid) => FunctionKey::User(pid),
        };
        *by_function.entry(key).or_default() += count;
    }
    let mut rows: Vec<(String, u64)> = by_function
        .into_iter()
        .map(|(key, count)| {
            let name = match key {
                FunctionKey::Kernel(Some(idx)) => symbols.functions[idx].1.clone(),
                FunctionKey::Kernel(None) => NO_SYMBOL.to_string(),
                FunctionKey::User(pid) => format!("[user pid 0x{:x}]", pid),
            };
            (name, count)
        })
        .collect();
    sort_by_count_desc(&mut rows);
    rows
}

// Map each function index to the index of the routine it belongs to.
//
// Hand-written assembly uses plain labels for loop targets (e.g. `smul_loop`
// inside `smul`), which would otherwise split one routine into many rows. A
// label is folded into the preceding routine when it was never a call target
// or kernel entry point during the run and its name extends that routine's
// name with '_' (`smul` -> `smul_loop`, `smul_skip_add`). Everything else is
// its own routine.
fn routine_groups(symbols: &Symbols, called: &HashSet<u32>, entries: &HashSet<u32>) -> Vec<usize> {
    let mut groups = Vec::with_capacity(symbols.functions.len());
    let mut root = 0;
    for (idx, (addr, name)) in symbols.functions.iter().enumerate() {
        let root_name = &symbols.functions[root].1;
        let folds = idx > 0
            && !called.contains(addr)
            && !entries.contains(addr)
            && name.len() > root_name.len() + 1
            && name.starts_with(root_name.as_str())
            && name.as_bytes()[root_name.len()] == b'_';
        if !folds {
            root = idx;
        }
        groups.push(root);
    }
    groups
}

// Write the function table and callers breakdown for one kernel context.
fn write_context_functions(
    out: &mut String,
    context: usize,
    counts: &HashMap<u32, u64>,
    context_total: u64,
    profiles: &[CoreProfile],
    symbols: &Symbols,
    groups: &[usize],
) {
    let group_of = |pc: u32| symbols.function_index(pc).map(|i| groups[i]);
    let name_of = |group: Option<usize>| match group {
        Some(idx) => symbols.functions[idx].1.clone(),
        None => NO_SYMBOL.to_string(),
    };

    // Self instructions per routine.
    let mut by_group: HashMap<Option<usize>, u64> = HashMap::new();
    for (&pc, &count) in counts {
        *by_group.entry(group_of(pc)).or_default() += count;
    }
    let mut rows: Vec<(Option<usize>, u64)> = by_group.into_iter().collect();
    sort_by_count_desc(&mut rows);

    let _ = writeln!(out, "Functions (self instructions, top {})", REPORT_TOP_FUNCTIONS_PER_CONTEXT);
    let _ = writeln!(out, "{:>8} {:>14}  {}", "%", "count", "function");
    for (group, count) in rows.iter().take(REPORT_TOP_FUNCTIONS_PER_CONTEXT) {
        let _ = writeln!(out, "{:>7.2}% {:>14}  {}", percent(*count, context_total), count, name_of(*group));
    }
    let _ = writeln!(out);

    // Incoming calls per routine, keyed by calling routine.
    let mut incoming: HashMap<usize, HashMap<Option<usize>, u64>> = HashMap::new();
    for profile in profiles {
        for (&(site, callee), &n) in &profile.calls[context] {
            if let Some(callee_group) = group_of(callee) {
                *incoming.entry(callee_group).or_default().entry(group_of(site)).or_default() += n;
            }
        }
    }
    let _ = writeln!(
        out,
        "Callers of the top {} called functions. Calls are exact; \"est.\" splits the callee's self",
        REPORT_FUNCTIONS_WITH_CALLERS
    );
    let _ = writeln!(out, "instructions across callers by call count, so it assumes a similar cost per call.");
    let mut shown = 0;
    for (group, self_count) in &rows {
        if shown == REPORT_FUNCTIONS_WITH_CALLERS {
            break;
        }
        let Some(group) = group else { continue };
        let Some(callers) = incoming.get(group) else { continue };
        let total_calls: u64 = callers.values().sum();
        let _ = writeln!(
            out,
            "{} (self {}, {} calls, {:.1} instructions/call)",
            name_of(Some(*group)),
            self_count,
            total_calls,
            *self_count as f64 / total_calls as f64
        );
        let mut caller_rows: Vec<(Option<usize>, u64)> = callers.iter().map(|(&k, &v)| (k, v)).collect();
        sort_by_count_desc(&mut caller_rows);
        for (caller, n) in caller_rows.iter().take(REPORT_CALLERS_PER_FUNCTION) {
            let estimate = (*self_count as f64 * *n as f64 / total_calls as f64).round() as u64;
            let _ = writeln!(
                out,
                "    {:>12} calls {:>14} est. ({:>5.1}% of callee)  {}",
                n,
                estimate,
                percent(*n, total_calls),
                name_of(*caller)
            );
        }
        if caller_rows.len() > REPORT_CALLERS_PER_FUNCTION {
            let _ = writeln!(out, "    ... {} more callers", caller_rows.len() - REPORT_CALLERS_PER_FUNCTION);
        }
        shown += 1;
    }
    let _ = writeln!(out);
}

// Write the "entered from user mode" and "background" kernel sections.
fn write_kernel_contexts(out: &mut String, profiles: &[CoreProfile], symbols: &Symbols, groups: &[usize], total: u64) {
    let mut counts: [HashMap<u32, u64>; CONTEXT_COUNT] = [HashMap::new(), HashMap::new()];
    for profile in profiles {
        profile.for_each_kernel_pc_by_context(|pc, background, from_user| {
            if background != 0 {
                *counts[CONTEXT_BACKGROUND].entry(pc).or_default() += background;
            }
            if from_user != 0 {
                *counts[CONTEXT_FROM_USER].entry(pc).or_default() += from_user;
            }
        });
    }
    let kernel_total: u64 = profiles.iter().map(|p| p.kernel_instructions).sum();
    let context_totals = [
        counts[CONTEXT_BACKGROUND].values().sum::<u64>(),
        counts[CONTEXT_FROM_USER].values().sum::<u64>(),
    ];

    let _ = writeln!(out, "Kernel work entered from user mode");
    let _ = writeln!(out, "----------------------------------");
    let _ = writeln!(
        out,
        "{} instructions: {:.2}% of kernel instructions, {:.2}% of all instructions.",
        context_totals[CONTEXT_FROM_USER],
        percent(context_totals[CONTEXT_FROM_USER], kernel_total),
        percent(context_totals[CONTEXT_FROM_USER], total)
    );
    let _ = writeln!(
        out,
        "Each kernel instruction is charged to the handler the core entered through when it"
    );
    let _ = writeln!(
        out,
        "last left user mode, until it returns to user mode or sleeps. Nested interrupts and"
    );
    let _ = writeln!(out, "context switches inside that span are charged to the same entry.");
    let _ = writeln!(out);
    if context_totals[CONTEXT_FROM_USER] == 0 {
        let _ = writeln!(out, "No kernel instructions were entered from user mode.");
        let _ = writeln!(out);
    }

    let mut entries: HashMap<u32, EntryStats> = HashMap::new();
    for profile in profiles {
        for (pc, stats) in profile.entry_totals() {
            let slot = entries.entry(pc).or_default();
            slot.entries += stats.entries;
            slot.instructions += stats.instructions;
        }
    }
    let mut entry_rows: Vec<(u32, EntryStats)> = entries.into_iter().filter(|(_, s)| s.instructions != 0).collect();
    entry_rows.sort_by(|a, b| b.1.instructions.cmp(&a.1.instructions).then(a.0.cmp(&b.0)));
    if context_totals[CONTEXT_FROM_USER] != 0 {
        let _ = writeln!(out, "Entry points");
        let _ = writeln!(
            out,
            "{:>8} {:>14} {:>12} {:>12}  {}",
            "%", "instructions", "entries", "avg/entry", "entry"
        );
        for (pc, stats) in &entry_rows {
            let avg = if stats.entries == 0 {
                "-".to_string()
            } else {
                format!("{:.1}", stats.instructions as f64 / stats.entries as f64)
            };
            let _ = writeln!(
                out,
                "{:>7.2}% {:>14} {:>12} {:>12}  {}",
                percent(stats.instructions, context_totals[CONTEXT_FROM_USER]),
                stats.instructions,
                stats.entries,
                avg,
                symbols.describe_pc(AddrSpace::Kernel, *pc)
            );
        }
        let _ = writeln!(out);
        write_context_functions(
            out,
            CONTEXT_FROM_USER,
            &counts[CONTEXT_FROM_USER],
            context_totals[CONTEXT_FROM_USER],
            profiles,
            symbols,
            groups,
        );
    }

    let _ = writeln!(out, "Background kernel work (not entered from user mode: boot, idle loops, kernel threads)");
    let _ = writeln!(out, "-------------------------------------------------------------------------------------");
    let _ = writeln!(
        out,
        "{} instructions: {:.2}% of kernel instructions, {:.2}% of all instructions.",
        context_totals[CONTEXT_BACKGROUND],
        percent(context_totals[CONTEXT_BACKGROUND], kernel_total),
        percent(context_totals[CONTEXT_BACKGROUND], total)
    );
    let _ = writeln!(out);
    write_context_functions(
        out,
        CONTEXT_BACKGROUND,
        &counts[CONTEXT_BACKGROUND],
        context_totals[CONTEXT_BACKGROUND],
        profiles,
        symbols,
        groups,
    );
}

// Build the text report for a finished run.
pub fn format_report(profiles: &[CoreProfile], symbols: &Symbols) -> String {
    let mut out = String::new();
    let total: u64 = profiles.iter().map(CoreProfile::instructions).sum();

    // Merge per-core PC counts; cores share kernel code, so kernel PCs combine.
    let mut merged: HashMap<(AddrSpace, u32), PcCount> = HashMap::new();
    for profile in profiles {
        profile.for_each_pc(|space, pc, entry| {
            let slot = merged.entry((space, pc)).or_default();
            slot.count += entry.count;
            slot.instr = entry.instr;
        });
    }

    let _ = writeln!(out, "Dioptase emulator profile");
    let _ = writeln!(out, "=========================");
    let _ = writeln!(
        out,
        "Counts are dispatched instructions, counted exactly (not sampled). They are"
    );
    let _ = writeln!(
        out,
        "not hardware cycles: the emulator does not model caches or pipeline stalls."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Symbol files: {} ({} functions, {} line markers)",
        symbols.files.join(", "),
        symbols.functions.len(),
        symbols.lines.len()
    );
    if symbols.functions.is_empty() {
        let _ = writeln!(
            out,
            "Warning: no function symbols loaded. Assemble the kernel with `basm -g` and pass its .hex with --symbols."
        );
    }
    let window = profiles.first().map(|p| &p.window);
    if let Some(window) = window {
        let openings = window.openings.load(Ordering::SeqCst);
        let _ = writeln!(
            out,
            "Window: {} (opened {} time{}; all cores counted while open)",
            window.description,
            openings,
            if openings == 1 { "" } else { "s" }
        );
        if openings == 0 {
            let _ = writeln!(
                out,
                "Warning: the window never opened, so every count below is zero. Check the --profile-start trigger."
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Per-core ticks");
    let _ = writeln!(
        out,
        "{:>6} {:>14} {:>14} {:>14} {:>14} {:>14}",
        "core", "ticks", "asleep", "instructions", "kernel", "user"
    );
    for p in profiles {
        let _ = writeln!(
            out,
            "{:>6} {:>14} {:>14} {:>14} {:>14} {:>14}",
            p.core_id,
            p.ticks,
            p.asleep_ticks,
            p.instructions(),
            p.kernel_instructions,
            p.user_instructions
        );
    }
    let kernel_total: u64 = profiles.iter().map(|p| p.kernel_instructions).sum();
    let user_total: u64 = profiles.iter().map(|p| p.user_instructions).sum();
    let _ = writeln!(
        out,
        "{:>6} {:>14} {:>14} {:>14} {:>14} {:>14}",
        "total",
        profiles.iter().map(|p| p.ticks).sum::<u64>(),
        profiles.iter().map(|p| p.asleep_ticks).sum::<u64>(),
        total,
        kernel_total,
        user_total
    );
    let _ = writeln!(
        out,
        "Kernel {:.2}%, user {:.2}% of instructions.",
        percent(kernel_total, total),
        percent(user_total, total)
    );
    let _ = writeln!(out);

    // Routine grouping uses call targets and entry points from every core.
    let mut called = HashSet::new();
    let mut entry_pcs = HashSet::new();
    for profile in profiles {
        called.extend(profile.called_ever.iter().copied());
        entry_pcs.extend(profile.entry_totals().into_keys());
    }
    let groups = routine_groups(symbols, &called, &entry_pcs);

    write_kernel_contexts(&mut out, profiles, symbols, &groups, total);

    let function_rows = group_by_function(merged.iter().map(|(&key, entry)| (key, entry.count)), symbols, &groups);
    let _ = writeln!(out, "All functions (self instructions, all cores and contexts)");
    let _ = writeln!(out, "{:>8} {:>14}  {}", "%", "count", "function");
    for (name, count) in &function_rows {
        let _ = writeln!(
            out,
            "{:>7.2}% {:>14}  {}",
            percent(*count, total),
            count,
            name
        );
    }
    let _ = writeln!(out);

    // Per-core breakdown, so an idle core's busy-waiting is not hidden inside
    // the merged table. Percentages are of that core's own instructions.
    if profiles.len() > 1 {
        let _ = writeln!(out, "Top functions per core (top {}, % of that core)", REPORT_TOP_FUNCTIONS_PER_CORE);
        for profile in profiles {
            let mut pcs = Vec::new();
            profile.for_each_pc(|space, pc, entry| pcs.push(((space, pc), entry.count)));
            let rows = group_by_function(pcs.into_iter(), symbols, &groups);
            let _ = writeln!(out, "core {}:", profile.core_id);
            for (name, count) in rows.iter().take(REPORT_TOP_FUNCTIONS_PER_CORE) {
                let _ = writeln!(
                    out,
                    "{:>7.2}% {:>14}  {}",
                    percent(*count, profile.instructions()),
                    count,
                    name
                );
            }
        }
        let _ = writeln!(out);
    }

    // Hot source lines: kernel PCs that resolve to a `#line` marker.
    let mut by_line: HashMap<(&str, u32), u64> = HashMap::new();
    for (&(space, pc), entry) in &merged {
        if space != AddrSpace::Kernel {
            continue;
        }
        if let Some(line) = symbols.line_for(pc) {
            *by_line.entry((line.file.as_str(), line.line)).or_default() += entry.count;
        }
    }
    if !by_line.is_empty() {
        let mut line_rows: Vec<((&str, u32), u64)> = by_line.into_iter().collect();
        sort_by_count_desc(&mut line_rows);
        let _ = writeln!(out, "Hot source lines (kernel, top {})", REPORT_TOP_LINES);
        let _ = writeln!(out, "{:>8} {:>14}  {}", "%", "count", "location");
        for ((file, line), count) in line_rows.iter().take(REPORT_TOP_LINES) {
            let _ = writeln!(
                out,
                "{:>7.2}% {:>14}  {}:{}",
                percent(*count, total),
                count,
                file,
                line
            );
        }
        let _ = writeln!(out);
    }

    // Hot instructions across all address spaces.
    let mut pc_rows: Vec<((AddrSpace, u32), u64)> = merged
        .iter()
        .map(|(&key, entry)| (key, entry.count))
        .collect();
    sort_by_count_desc(&mut pc_rows);
    let _ = writeln!(out, "Hot instructions (top {})", REPORT_TOP_INSTRUCTIONS);
    let _ = writeln!(
        out,
        "{:>8} {:>14}  {:<14} {:<10} {:<36} {}",
        "%", "count", "space", "pc", "symbol", "instruction"
    );
    for ((space, pc), count) in pc_rows.iter().take(REPORT_TOP_INSTRUCTIONS) {
        let space_name = match space {
            AddrSpace::Kernel => "kernel".to_string(),
            AddrSpace::User(pid) => format!("pid 0x{:x}", pid),
        };
        let instr = merged[&(*space, *pc)].instr;
        let _ = writeln!(
            out,
            "{:>7.2}% {:>14}  {:<14} {:08x}   {:<36} {}",
            percent(*count, total),
            count,
            space_name,
            pc,
            symbols.describe_pc(*space, *pc),
            disassemble(instr)
        );
    }

    out
}

// Build the report and write it to `path`.
pub fn write_report(path: &str, profiles: &[CoreProfile], symbols: &Symbols) -> Result<(), String> {
    fs::write(path, format_report(profiles, symbols))
        .map_err(|err| format!("Profiler: failed to write report {}: {}", path, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Write a symbol-only hex file into the test scratch directory.
    fn write_symbol_file(name: &str, contents: &str) -> String {
        let dir = std::env::temp_dir().join("dioptase-profiler-tests");
        fs::create_dir_all(&dir).expect("failed to create profiler test dir");
        let path = dir.join(name);
        fs::write(&path, contents).expect("failed to write profiler symbol file");
        path.to_string_lossy().to_string()
    }

    // Dotted local labels and #data globals must not become function starts,
    // or hot PCs would be attributed to branch targets and variables.
    #[test]
    fn symbols_skip_local_labels_and_globals() {
        let path = write_symbol_file(
            "skip_locals.hex",
            "#label foo 00010000\n\
             #label foo.end.0 00010010\n\
             #label bar 00010020\n\
             #label counter 00090000\n\
             #data counter 00090000\n",
        );
        let symbols = Symbols::load(&[path]).unwrap();
        let names: Vec<&str> = symbols.functions.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, vec!["foo", "bar"], "only foo and bar are function starts");
        assert_eq!(symbols.describe_pc(AddrSpace::Kernel, 0x10014), "foo+0x14");
        assert_eq!(symbols.describe_pc(AddrSpace::Kernel, 0x10020), "bar+0x0");
        assert_eq!(symbols.describe_pc(AddrSpace::Kernel, 0xFFFC), NO_SYMBOL);
    }

    // An assembly routine with no #line markers must not inherit the last
    // source line of the C function placed before it.
    #[test]
    fn line_lookup_stops_at_function_start() {
        let path = write_symbol_file(
            "line_bound.hex",
            "#label c_func 00010000\n\
             #label asm_func 00010040\n\
             #line kernel/a.c 7 00010000\n\
             #line kernel/a.c 9 00010020\n",
        );
        let symbols = Symbols::load(&[path]).unwrap();
        assert_eq!(symbols.line_for(0x10024).map(|l| l.line), Some(9));
        assert!(
            symbols.line_for(0x10044).is_none(),
            "asm_func has no line markers, so it must not resolve to kernel/a.c:9"
        );
    }

    #[test]
    fn missing_symbol_file_reports_path() {
        let err = Symbols::load(&["/nonexistent/kernel.hex".to_string()])
            .err()
            .expect("loading a missing symbol file must fail");
        assert!(err.contains("/nonexistent/kernel.hex"), "error should name the file: {err}");
    }

    // A user-mode start with a kernel stop must count only the user stretch
    // and the kernel work after it, reopen on the next user instruction, and
    // not count the stop trigger instruction itself.
    #[test]
    fn window_counts_only_between_triggers() {
        const STOP_PC: u32 = 0x1_0100;
        let window = ProfileWindow::new(WindowStart::UserMode, vec![STOP_PC], "test".to_string());
        let mut p = CoreProfile::new(0, Arc::clone(&window));

        p.record_instruction(0x1_0000, 0, true, 0, 0); // boot: window closed
        p.record_instruction(0x8000, 0, false, 7, 0); // opens
        p.record_instruction(0x1_0004, 0, true, 7, 0); // syscall work: counted
        p.record_instruction(STOP_PC, 0, true, 7, 0); // closes, not counted
        p.record_instruction(0x1_0008, 0, true, 0, 0); // closed
        p.record_instruction(0x8004, 0, false, 7, 0); // reopens

        assert_eq!(p.user_instructions, 2);
        assert_eq!(p.kernel_instructions, 1);
        assert_eq!(window.openings.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn trigger_resolution_accepts_labels_and_addresses() {
        let path = write_symbol_file("triggers.hex", "#label stop 00071358\n#label stop.end.1 00071400\n");
        let symbols = Symbols::load(&[path]).unwrap();
        assert_eq!(symbols.resolve_trigger("--profile-stop", "stop").unwrap(), vec![0x71358]);
        assert_eq!(symbols.resolve_trigger("--profile-stop", "stop.end.1").unwrap(), vec![0x71400]);
        assert_eq!(symbols.resolve_trigger("--profile-stop", "0x1234").unwrap(), vec![0x1234]);
        let err = symbols.resolve_trigger("--profile-stop", "nope").unwrap_err();
        assert!(err.contains("nope") && err.contains("--profile-stop"), "error names flag and label: {err}");
    }

    // Kernel work after a user->kernel transition is charged to the entry PC
    // until the core returns to user mode or sleeps; everything else is
    // background. Calls (link == previous pc + 4, not a fallthrough) are
    // recorded per context.
    #[test]
    fn kernel_work_is_charged_to_user_entry() {
        const HANDLER: u32 = 0x1_0000;
        const HELPER: u32 = 0x1_0100;
        let mut p = CoreProfile::new(0, ProfileWindow::whole_run());

        p.record_instruction(0x2_0000, 0, true, 0, 0); // boot: background
        p.record_instruction(0x8000, 0, false, 5, 0); // user
        p.record_instruction(HANDLER, 0, true, 5, 0); // entry from user
        p.record_instruction(HANDLER + 4, 0, true, 5, 0);
        p.record_instruction(HELPER, 0, true, 5, HANDLER + 8); // call from HANDLER+4
        p.record_instruction(0x8004, 0, false, 5, 0); // back to user
        p.record_instruction(HANDLER, 0, true, 5, 0); // second entry
        p.record_tick(true); // sleeping ends the attribution
        p.record_instruction(0x2_0004, 0, true, 0, 0); // background again

        let totals = p.entry_totals();
        assert_eq!(totals[&HANDLER].entries, 2);
        assert_eq!(totals[&HANDLER].instructions, 4, "3 in the first span, 1 in the second");
        let mut background = 0;
        let mut from_user = 0;
        p.for_each_kernel_pc_by_context(|_, b, u| {
            background += b;
            from_user += u;
        });
        assert_eq!((background, from_user), (2, 4));
        assert_eq!(p.calls[CONTEXT_FROM_USER][&(HANDLER + 4, HELPER)], 1);
        assert!(p.calls[CONTEXT_BACKGROUND].is_empty());
    }

    // A fallthrough whose link register happens to equal pc + 4 must not be
    // mistaken for a call.
    #[test]
    fn fallthrough_is_not_a_call() {
        let mut p = CoreProfile::new(0, ProfileWindow::whole_run());
        p.record_instruction(0x1_0000, 0, true, 0, 0);
        p.record_instruction(0x1_0004, 0, true, 0, 0x1_0004);
        assert!(p.called_ever.is_empty());
    }

    // Jump-only assembly labels fold into the routine they extend; called
    // labels and unrelated names stay separate.
    #[test]
    fn assembly_labels_fold_into_routine() {
        let path = write_symbol_file(
            "groups.hex",
            "#label smul 00010000\n#label smul_loop 00010010\n#label smul_skip_add 00010020\n\
             #label spin_lock 00010040\n#label spin_lock_acquire 00010050\n#label smulx 00010060\n",
        );
        let symbols = Symbols::load(&[path]).unwrap();
        let called: HashSet<u32> = [0x10050].into_iter().collect();
        let groups = routine_groups(&symbols, &called, &HashSet::new());
        let names: Vec<&str> = groups.iter().map(|&g| symbols.functions[g].1.as_str()).collect();
        assert_eq!(names, vec!["smul", "smul", "smul", "spin_lock", "spin_lock_acquire", "smulx"]);
    }

    // Kernel PCs from different cores merge; user PCs stay separated by PID
    // even when two processes run the same virtual address.
    #[test]
    fn report_merges_cores_and_separates_pids() {
        let path = write_symbol_file("merge.hex", "#label work 00010000\n");
        let symbols = Symbols::load(&[path]).unwrap();

        let mut core0 = CoreProfile::new(0, ProfileWindow::whole_run());
        let mut core1 = CoreProfile::new(1, ProfileWindow::whole_run());
        for _ in 0..3 {
            core0.record_instruction(0x10000, 0, true, 0, 0);
        }
        core1.record_instruction(0x10004, 0, true, 0, 0);
        core0.record_instruction(0x8000_0000, 0, false, 1, 0);
        core1.record_instruction(0x8000_0000, 0, false, 2, 0);
        core1.record_instruction(0x8000_0000, 0, false, 2, 0);

        let report = format_report(&[core0, core1], &symbols);
        let function_line = |name: &str| {
            report
                .lines()
                .find(|l| l.ends_with(&format!("  {}", name)))
                .unwrap_or_else(|| panic!("missing function row {name} in:\n{report}"))
                .to_string()
        };
        assert!(function_line("work").contains(" 4  "), "work: 3 on core 0 + 1 on core 1");
        assert!(function_line("[user pid 0x1]").contains(" 1  "));
        assert!(function_line("[user pid 0x2]").contains(" 2  "));
    }
}
