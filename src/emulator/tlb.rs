// Software model of the per-core TLB (docs/ISA.md "TLB").
//
// An entry is either private (keyed by PID and VPN) or global (keyed by VPN
// only, `G` flag set). At most one private entry exists per (PID, VPN) and at
// most one global entry per VPN. Lookups prefer the private entry and fall
// back to the global one.
//
// Replacement is implementation-defined by the ISA. This model evicts the
// oldest entry of the same class (private/global) as the incoming entry, or
// the oldest entry overall if that class is empty. It is deterministic across
// runs, unlike the earlier HashMap-based model whose eviction order depended
// on the process's random hash seed.

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
    // Insertion sequence number used for oldest-first eviction.
    seq: u64,
}

// Fixed-capacity TLB for one core.
#[derive(Debug)]
pub struct Tlb {
    entries: Vec<TlbEntry>,
    next_seq: u64,
}

impl Tlb {
    // Create an empty TLB.
    pub fn new() -> Tlb {
        Tlb {
            entries: Vec::with_capacity(TLB_ENTRIES),
            next_seq: 0,
        }
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

    // Translate a virtual page for a read, write, or execute access. A private
    // entry that denies the access does not hide a permitting global entry;
    // if no global entry exists the private entry's fault flags are reported.
    pub(super) fn access(&self, pid: u32, vpn: u32, access: Access, kmode: bool) -> TlbAccess {
        let mut private_fault = None;
        if let Some(i) = self.find_private(pid, vpn) {
            match Self::classify(self.entries[i].value, access, kmode) {
                hit @ TlbAccess::Hit(_) => return hit,
                TlbAccess::Fault(flags) => private_fault = Some(flags),
            }
        }
        if let Some(i) = self.find_global(vpn) {
            return Self::classify(self.entries[i].value, access, kmode);
        }
        TlbAccess::Fault(private_fault.unwrap_or(TLB_FAULT_ABSENT))
    }

    // Raw entry value for tlbr, preferring the private mapping.
    pub fn read(&self, pid: u32, vpn: u32) -> Option<u32> {
        self.find_private(pid, vpn)
            .or_else(|| self.find_global(vpn))
            .map(|i| self.entries[i].value)
    }

    // Insert or replace a mapping (tlbw), evicting at capacity.
    pub fn write(&mut self, pid: u32, vpn: u32, value: u32) {
        let global = value & TLB_FLAG_GLOBAL != 0;
        let existing = if global {
            self.find_global(vpn)
        } else {
            self.find_private(pid, vpn)
        };
        if let Some(i) = existing {
            self.entries[i].value = value;
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
            seq: self.next_seq,
        });
        self.next_seq += 1;
    }

    // Drop the oldest entry, preferring the incoming entry's class.
    fn evict_one(&mut self, prefer_global: bool) {
        let oldest_in = |global: bool| {
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.global == global)
                .min_by_key(|(_, e)| e.seq)
                .map(|(i, _)| i)
        };
        if let Some(i) = oldest_in(prefer_global).or_else(|| oldest_in(!prefer_global)) {
            self.entries.swap_remove(i);
        }
    }

    // Remove both the private (pid, vpn) and the global vpn mapping (tlbi).
    pub fn invalidate(&mut self, pid: u32, vpn: u32) {
        self.entries
            .retain(|e| !(e.vpn == vpn && (e.global || e.pid == pid)));
    }

    // Remove every mapping (tlbc).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    // Print resident entries for the debugger, oldest first within each class.
    pub(super) fn debug_dump(&self) {
        let mut sorted = self.entries.clone();
        sorted.sort_by_key(|e| e.seq);
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
- Capacity is enforced with deterministic oldest-first, same-class eviction.
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

    #[test]
    fn eviction_is_oldest_first_within_class() {
        let mut tlb = Tlb::new();
        for vpn in 0..TLB_ENTRIES as u32 {
            tlb.write(0, vpn, RWX);
        }
        tlb.write(0, 100, RWX);
        assert_eq!(tlb.read(0, 0), None, "the oldest private entry is evicted first");
        assert!(tlb.read(0, 1).is_some());
        assert_eq!(tlb.entries.len(), TLB_ENTRIES);
    }
}
