// Software model of the per-core TLB (docs/ISA.md "TLB").
//
// An entry is either private (keyed by PID and VPN) or global (keyed by VPN
// only, `G` flag set). At most one private entry exists per (PID, VPN) and at
// most one global entry per VPN. Lookups prefer the private entry and fall
// back to the global one.
//
// Replacement (docs/ISA.md "TLB Structure") is least recently used: a
// translation that hits, or a tlbw that fills or rewrites an entry, marks it
// used; tlbr and faulting lookups do not. At capacity the least recently used
// entry of the incoming entry's class (private/global) is evicted, or of the
// other class if that one is empty. Oldest-first eviction, used before, threw
// out hot code, stack and global pages every 16 misses. The model is
// deterministic across runs.

pub(super) const TLB_ENTRIES: usize = 16;

// Entry flag bits (`G U X W R`, R in bit 0).
const TLB_FLAG_READ: u32 = 1 << 0;
const TLB_FLAG_WRITE: u32 = 1 << 1;
const TLB_FLAG_EXEC: u32 = 1 << 2;
const TLB_FLAG_USER: u32 = 1 << 3;
const TLB_FLAG_GLOBAL: u32 = 1 << 4;
// TLBF value when no entry matched at all.
pub(super) const TLB_FAULT_ABSENT: u32 = 0;
const PAGE_MASK: u32 = 0xFFFF_F000;

// Kind of memory access being translated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    Read,
    Write,
    Execute,
}

impl Access {
    // Permission bit an entry must carry for this access.
    fn required_flag(self) -> u32 {
        match self {
            Access::Read => TLB_FLAG_READ,
            Access::Write => TLB_FLAG_WRITE,
            Access::Execute => TLB_FLAG_EXEC,
        }
    }
}

// Translation result: the physical page base on success, or the TLBF bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TlbAccess {
    Hit(u32),
    Fault(u32),
}

// One resident mapping. `value` is the raw tlbw operand (PPN and flags).
#[derive(Clone, Copy, Debug)]
struct TlbEntry {
    pid: u32,
    vpn: u32,
    value: u32,
    global: bool,
    // Stamp of the last use (hit or fill); the smallest stamp is evicted.
    used: u64,
}

// A key of the diagnostic shadow TLB and the stamp of its last use. While the
// key is resident in the real TLB, the real entry's stamp is the current one;
// the record's own stamp is brought up to date when the entry leaves (evicted
// or cleared), which keeps hits from touching the shadow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShadowKey {
    global: bool,
    pid: u32,
    vpn: u32,
    used: u64,
}

// Fixed-capacity TLB for one core.
#[derive(Debug)]
pub struct Tlb {
    entries: Vec<TlbEntry>,
    // Use clock for the LRU stamps.
    clock: u64,
    // Diagnostic only (the profiler's flush re-miss count); never affects
    // translation. Keys of a same-size LRU TLB that sees the same fills, hits
    // and `tlbi`s but ignores `tlbc`: a real miss that this shadow would have
    // hit was caused by a flush.
    shadow: Vec<ShadowKey>,
}

impl Tlb {
    // Create an empty TLB.
    pub fn new() -> Tlb {
        Tlb {
            entries: Vec::with_capacity(TLB_ENTRIES),
            clock: 0,
            shadow: Vec::with_capacity(TLB_ENTRIES),
        }
    }

    // Advance the use clock and return the new stamp.
    fn stamp(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    // Whether a shadow record names the same mapping as a real entry.
    fn same_key(k: &ShadowKey, e: &TlbEntry) -> bool {
        k.global == e.global && k.vpn == e.vpn && (k.global || k.pid == e.pid)
    }

    // Bring the shadow record of a departing real entry up to its last use.
    fn sync_shadow(&mut self, e: TlbEntry) {
        if let Some(k) = self.shadow.iter_mut().find(|k| Self::same_key(k, &e)) {
            k.used = e.used;
        }
    }

    // A shadow record's current stamp: the real entry's while it is resident.
    fn shadow_used(&self, k: &ShadowKey) -> u64 {
        self.entries
            .iter()
            .find(|e| Self::same_key(k, e))
            .map_or(k.used, |e| e.used)
    }

    // Index of the private entry for (pid, vpn), if resident.
    fn find_private(&self, pid: u32, vpn: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| !e.global && e.pid == pid && e.vpn == vpn)
    }

    // Index of the global entry for vpn, if resident.
    fn find_global(&self, vpn: u32) -> Option<usize> {
        self.entries.iter().position(|e| e.global && e.vpn == vpn)
    }

    // Missing permission bits for `access`; 0 means the access is allowed.
    fn fault_flags(value: u32, access: Access, kmode: bool) -> u32 {
        let mut flags = access.required_flag() & !value;
        if !kmode && value & TLB_FLAG_USER == 0 {
            flags |= TLB_FLAG_USER;
        }
        flags
    }

    // Check one entry's permissions for `access`.
    fn classify(value: u32, access: Access, kmode: bool) -> TlbAccess {
        match Self::fault_flags(value, access, kmode) {
            0 => TlbAccess::Hit(value & PAGE_MASK),
            flags => TlbAccess::Fault(flags),
        }
    }

    // Translate a virtual page for a read, write, or execute access, marking
    // the entry that permits it as used. A private entry that denies the
    // access does not hide a permitting global entry; if no global entry
    // exists the private entry's fault flags are reported.
    pub(super) fn access(&mut self, pid: u32, vpn: u32, access: Access, kmode: bool) -> TlbAccess {
        let mut private_fault = None;
        if let Some(i) = self.find_private(pid, vpn) {
            match Self::classify(self.entries[i].value, access, kmode) {
                hit @ TlbAccess::Hit(_) => {
                    self.entries[i].used = self.stamp();
                    return hit;
                }
                TlbAccess::Fault(flags) => private_fault = Some(flags),
            }
        }
        if let Some(i) = self.find_global(vpn) {
            let result = Self::classify(self.entries[i].value, access, kmode);
            if let TlbAccess::Hit(_) = result {
                self.entries[i].used = self.stamp();
            }
            return result;
        }
        TlbAccess::Fault(private_fault.unwrap_or(TLB_FAULT_ABSENT))
    }

    // Raw entry value for tlbr, preferring the private mapping.
    pub fn read(&self, pid: u32, vpn: u32) -> Option<u32> {
        self.find_private(pid, vpn)
            .or_else(|| self.find_global(vpn))
            .map(|i| self.entries[i].value)
    }

    // Insert or replace a mapping (tlbw), evicting the least recently used
    // entry at capacity. Either way the mapping counts as just used.
    pub fn write(&mut self, pid: u32, vpn: u32, value: u32) {
        let global = value & TLB_FLAG_GLOBAL != 0;
        let pid = if global { 0 } else { pid };
        let now = self.stamp();
        self.shadow_fill(ShadowKey { global, pid, vpn, used: now });
        let existing = if global {
            self.find_global(vpn)
        } else {
            self.find_private(pid, vpn)
        };
        if let Some(i) = existing {
            self.entries[i].value = value;
            self.entries[i].used = now;
            return;
        }
        if self.entries.len() >= TLB_ENTRIES {
            self.evict_one(global);
        }
        self.entries.push(TlbEntry {
            pid,
            vpn,
            value,
            global,
            used: now,
        });
    }

    // Record a fill in the shadow, evicting its least recently used key.
    fn shadow_fill(&mut self, key: ShadowKey) {
        let same = |k: &ShadowKey| k.global == key.global && k.vpn == key.vpn && (k.global || k.pid == key.pid);
        if let Some(k) = self.shadow.iter_mut().find(|k| same(k)) {
            k.used = key.used;
            return;
        }
        if self.shadow.len() >= TLB_ENTRIES {
            let victim = (0..self.shadow.len()).min_by_key(|&i| self.shadow_used(&self.shadow[i]));
            if let Some(i) = victim {
                self.shadow.swap_remove(i);
            }
        }
        self.shadow.push(key);
    }

    // Drop the least recently used entry, preferring the incoming entry's class.
    fn evict_one(&mut self, prefer_global: bool) {
        let lru_in = |global: bool| {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.global == global)
                .min_by_key(|(_, e)| e.used)
                .map(|(i, _)| i)
        };
        if let Some(i) = lru_in(prefer_global).or_else(|| lru_in(!prefer_global)) {
            let gone = self.entries.swap_remove(i);
            self.sync_shadow(gone);
        }
    }

    // Remove both the private (pid, vpn) and the global vpn mapping (tlbi).
    pub fn invalidate(&mut self, pid: u32, vpn: u32) {
        self.entries
            .retain(|e| !(e.vpn == vpn && (e.global || e.pid == pid)));
        self.shadow
            .retain(|k| !(k.vpn == vpn && (k.global || k.pid == pid)));
    }

    // Remove every mapping (tlbc). The diagnostic shadow keeps its keys, with
    // their last-use stamps brought up to date.
    pub fn clear(&mut self) {
        let gone: Vec<TlbEntry> = self.entries.drain(..).collect();
        for e in gone {
            self.sync_shadow(e);
        }
    }

    // Whether a miss on (pid, vpn) would have hit had no `tlbc` ever run: the
    // shadow TLB, which ignores clears but evicts at the same capacity, still
    // holds the key.
    pub(super) fn miss_caused_by_clear(&self, pid: u32, vpn: u32) -> bool {
        self.shadow
            .iter()
            .any(|k| k.vpn == vpn && (k.global || k.pid == pid))
    }

    // Print resident entries for the debugger, least recently used first
    // within each class.
    pub(super) fn debug_dump(&self) {
        let mut sorted = self.entries.clone();
        sorted.sort_by_key(|e| e.used);
        for (title, global) in [("private", false), ("global", true)] {
            let class: Vec<&TlbEntry> = sorted.iter().filter(|e| e.global == global).collect();
            println!("TLB {}: {} entries", title, class.len());
            if class.is_empty() {
                println!("  (empty)");
            }
            for e in class {
                if global {
                    println!("  vpn {:08X} -> {:08X}", e.vpn, e.value);
                } else {
                    println!("  pid {:08X} vpn {:08X} -> {:08X}", e.pid, e.vpn, e.value);
                }
            }
        }
        println!("TLB total: {}/{} entries", self.entries.len(), TLB_ENTRIES);
    }
}

#[cfg(test)]
/*
Summary:
- Private entries shadow global ones only when they permit the access.
- Capacity is enforced with deterministic least-recently-used, same-class
  eviction; hits and fills count as uses.
*/
mod tests {
    use super::*;

    const RWX: u32 = TLB_FLAG_READ | TLB_FLAG_WRITE | TLB_FLAG_EXEC;

    #[test]
    fn private_fault_falls_back_to_global_entry() {
        let mut tlb = Tlb::new();
        tlb.write(1, 5, 0x1000 | TLB_FLAG_READ);
        tlb.write(1, 5, 0x2000 | RWX | TLB_FLAG_GLOBAL);
        assert_eq!(tlb.access(1, 5, Access::Read, true), TlbAccess::Hit(0x1000));
        assert_eq!(tlb.access(1, 5, Access::Write, true), TlbAccess::Hit(0x2000));
        tlb.invalidate(1, 5);
        assert_eq!(tlb.access(1, 5, Access::Read, true), TlbAccess::Fault(TLB_FAULT_ABSENT));
    }

    #[test]
    fn user_access_requires_user_flag() {
        let mut tlb = Tlb::new();
        tlb.write(0, 1, 0x3000 | RWX);
        assert_eq!(
            tlb.access(0, 1, Access::Write, false),
            TlbAccess::Fault(TLB_FLAG_USER)
        );
    }

    // The shadow survives clears (so A -> B -> A switches that clear twice
    // still count A's re-misses) but not tlbi or capacity eviction.
    #[test]
    fn shadow_attributes_misses_to_clears() {
        let mut tlb = Tlb::new();
        tlb.write(7, 1, RWX);
        tlb.write(0, 2, RWX | TLB_FLAG_GLOBAL);
        tlb.clear();
        tlb.write(8, 3, RWX);
        tlb.clear();
        assert!(tlb.miss_caused_by_clear(7, 1), "resident until the clears");
        assert!(tlb.miss_caused_by_clear(9, 2), "global entries match any PID");
        assert!(!tlb.miss_caused_by_clear(8, 1), "another PID's key is not a re-miss");
        tlb.invalidate(7, 1);
        assert!(!tlb.miss_caused_by_clear(7, 1), "tlbi removes the key for real");
        for vpn in 100..100 + TLB_ENTRIES as u32 {
            tlb.write(8, vpn, RWX);
        }
        assert!(!tlb.miss_caused_by_clear(8, 3), "evicted by capacity, not by a clear");
    }

    #[test]
    fn eviction_is_least_recently_used_within_class() {
        let mut tlb = Tlb::new();
        for vpn in 0..TLB_ENTRIES as u32 {
            tlb.write(0, vpn, RWX);
        }
        tlb.write(0, 100, RWX);
        assert_eq!(tlb.read(0, 0), None, "an untouched first fill is the least recently used");
        assert!(tlb.read(0, 1).is_some());
        assert_eq!(tlb.entries.len(), TLB_ENTRIES);
        assert_eq!(tlb.access(0, 1, Access::Read, true), TlbAccess::Hit(0));
        tlb.write(0, 101, RWX);
        assert!(tlb.read(0, 1).is_some(), "a hit keeps the oldest fill resident");
        assert_eq!(tlb.read(0, 2), None, "the least recently used entry goes instead");
    }

    // tlbr and faulting lookups are management, not use.
    #[test]
    fn reads_and_faults_do_not_refresh() {
        let mut tlb = Tlb::new();
        for vpn in 0..TLB_ENTRIES as u32 {
            tlb.write(0, vpn, TLB_FLAG_READ);
        }
        assert!(tlb.read(0, 0).is_some());
        assert_eq!(tlb.access(0, 0, Access::Write, true), TlbAccess::Fault(TLB_FLAG_WRITE));
        tlb.write(0, 100, RWX);
        assert_eq!(tlb.read(0, 0), None, "neither tlbr nor a fault counted as a use");
    }

    // A hot global page survives a stream of private fills.
    #[test]
    fn hot_entries_survive_a_sweep() {
        let mut tlb = Tlb::new();
        tlb.write(0, 7, RWX | TLB_FLAG_GLOBAL);
        for vpn in 100..100 + 4 * TLB_ENTRIES as u32 {
            tlb.write(1, vpn, RWX);
            assert_eq!(tlb.access(1, 7, Access::Execute, true), TlbAccess::Hit(0));
        }
        assert!(tlb.read(0, 7).is_some(), "an entry used between fills is never the victim");
    }
}
