//! checks the decomp with nothing of the title running: from a saved state,
//! each replaced function runs, the title's code and ours, on inputs made
//! up for it, and what each did is compared as the check does a real call.
//!
//! the memory a run reads is the state's where the state has it mapped, and
//! made up where it has not: each word a mix of pointers (to more made-up
//! memory), small numbers, zero, -1 and floats, the same for both runs. a
//! function's `this` is a live object of its class when the state has one
//! (found by the vtable its function is in), or a made-up one with that
//! vtable. modules the state has not loaded are loaded from the romfs, as
//! LoadCRO would, so every module's functions can be checked.
//!
//! a run that goes into made-up memory as code (a virtual call on a
//! made-up object, mostly) says nothing, and counts as incomplete.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use zakuro_cpu::Bus;

use crate::decomp_check::{compare, run, tell, undo, Raw, TOLD};
use crate::memory::physical::MemoryRegion;
use crate::memory::{Memory, MemoryState, Permission};
use crate::System;

/// where made-up memory starts; the state never maps it.
const MADE_UP: u32 = 0x7000_0000;
/// made-up objects, one page each, by run.
const OBJECTS: u32 = 0x7800_0000;
/// the made-up stack.
const STACK: u32 = 0x7F00_0000;
/// instructions a run may take.
const RUN_LIMIT: u64 = 400_000;

/// a word of made-up memory, the same for every run of one seed.
fn made_up(seed: u64, addr: u32) -> u32 {
    let h = mix(seed ^ (addr as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let low = (h >> 8) as u32;
    match h % 100 {
        0..=29 => MADE_UP + (low % 0x10_0000) * 16,
        30..=49 => low % 8,
        50..=64 => 0,
        65..=69 => u32::MAX,
        70..=74 => 1,
        75..=89 => [0.0f32, 1.0, 0.5, -1.0, 2.0, 100.0, 0.25, 30.0][low as usize % 8].to_bits(),
        _ => (h >> 32) as u32,
    }
}

fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// the state's memory where it is mapped, made-up memory elsewhere; writes
/// stay here, the state is never changed.
struct Synth<'a> {
    memory: &'a mut Memory,
    written: HashMap<u32, u8>,
    mapped: HashMap<u32, bool>,
    /// pages instructions may come from: the title's code, the modules',
    /// the decomp's.
    code: HashMap<u32, bool>,
    seed: u64,
    /// a run fetched an instruction from made-up memory.
    astray: bool,
}

impl Synth<'_> {
    fn is_mapped(&mut self, addr: u32) -> bool {
        let page = addr & !0xFFF;
        if let Some(&known) = self.mapped.get(&page) {
            return known;
        }
        let known = self.memory.mapping_at(page).is_some_and(|m| m.state != MemoryState::Io);
        self.mapped.insert(page, known);
        known
    }
    fn is_code(&mut self, addr: u32) -> bool {
        let page = addr & !0xFFF;
        if let Some(&known) = self.code.get(&page) {
            return known;
        }
        let known = self.memory.mapping_at(page).is_some_and(|m| {
            matches!(m.state, MemoryState::Code | MemoryState::AliasCode) || (m.state == MemoryState::Static && m.permission.contains(Permission::EXECUTE))
        });
        self.code.insert(page, known);
        known
    }
}

impl Raw for Synth<'_> {
    fn peek(&mut self, addr: u32) -> u8 {
        if let Some(&value) = self.written.get(&addr) {
            return value;
        }
        if self.is_mapped(addr) {
            let mut value = [0];
            self.memory.read_bytes(addr, &mut value);
            value[0]
        } else if (STACK - 0x10_0000..STACK).contains(&addr) {
            // the stack under the call reads as 0: a local read before it is
            // written is then the same at any depth, as the two runs' frames
            // are not
            0
        } else {
            made_up(self.seed, addr & !3).to_le_bytes()[(addr & 3) as usize]
        }
    }
    fn poke(&mut self, addr: u32, value: u8) {
        self.written.insert(addr, value);
    }
    fn lost(&self) -> bool {
        self.astray
    }
}

impl Bus for Synth<'_> {
    fn read8(&mut self, addr: u32) -> u8 {
        self.peek(addr)
    }
    fn read16(&mut self, addr: u32) -> u16 {
        u16::from_le_bytes([self.peek(addr), self.peek(addr.wrapping_add(1))])
    }
    fn read32(&mut self, addr: u32) -> u32 {
        u32::from_le_bytes([0, 1, 2, 3].map(|i| self.peek(addr.wrapping_add(i))))
    }
    fn write8(&mut self, addr: u32, value: u8) {
        self.poke(addr, value)
    }
    fn write16(&mut self, addr: u32, value: u16) {
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.poke(addr.wrapping_add(i as u32), byte);
        }
    }
    fn write32(&mut self, addr: u32, value: u32) {
        for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.poke(addr.wrapping_add(i as u32), byte);
        }
    }
    fn fetch32(&mut self, addr: u32) -> u32 {
        if !self.is_code(addr) {
            self.astray = true;
        }
        self.read32(addr)
    }
    fn fetch16(&mut self, addr: u32) -> u16 {
        if !self.is_code(addr) {
            self.astray = true;
        }
        self.read16(addr)
    }
}

/// somewhere size bytes are free, from 0x0A000000 up.
fn free_range(memory: &Memory, size: u32) -> Option<u32> {
    let mut at = 0x0A00_0000u32;
    while at < 0x0F00_0000 {
        let end = at + size;
        match memory.mappings().find(|m| m.base < end && at < m.base + m.size) {
            Some(m) => at = (m.base + m.size + 0xFFFFF) & !0xFFFFF,
            None => return Some(at),
        }
    }
    None
}

fn map_new(system: &mut System, at: u32, bytes: &[u8], size: u32) -> Result<(), String> {
    let size = (size.max(1) + 0xFFF) & !0xFFF;
    let block = system.memory.phys.allocate(MemoryRegion::System, size).ok_or("no memory left")?;
    let mut padded = bytes.to_vec();
    padded.resize(size as usize, 0);
    system.memory.write_physical(block.addr, &padded);
    system.memory.map(at, block.addr, size, Permission::RW | Permission::EXECUTE, MemoryState::Code);
    Ok(())
}

/// loads romfs/<name>.cro as LoadCRO does, with its data and BSS of their
/// own, linked to what is loaded; its functions are then checked.
pub fn load_module(system: &mut System, romfs: &Path, name: &str) -> Result<u32, String> {
    let path = romfs.join(format!("{name}.cro"));
    let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let (table, count) = (word(0xC8) as usize, word(0xCC) as usize);
    let (mut data, mut bss) = ((0usize, 0u32), 0u32);
    for i in 0..count {
        let (offset, size, kind) = (word(table + 12 * i), word(table + 12 * i + 4), word(table + 12 * i + 8));
        match kind {
            2 => data = (offset as usize, size),
            3 => bss = size,
            _ => {}
        }
    }
    let size = (bytes.len() as u32 + 0xFFF) & !0xFFF;
    let data_size = (data.1 + 0xFFF) & !0xFFF;
    let total = size + data_size + ((bss + 0xFFF) & !0xFFF) + 0x1000;
    let base = free_range(&system.memory, total).ok_or("no address space left")?;
    map_new(system, base, &bytes, size)?;
    let data_at = base + size;
    map_new(system, data_at, &bytes[data.0..data.0 + data.1 as usize], data.1)?;
    let bss_at = data_at + data_size;
    map_new(system, bss_at, &[], bss)?;
    system
        .cro
        .load(&mut system.memory, base, bytes.len() as u32, data_at, data.1, bss_at, bss, true, 0)
        .map_err(|error| format!("{name}: {error:?}"))?;
    log::info!("decomp synth: {name} loaded at 0x{base:08X}");
    crate::decomp::hook_module(system, name, base);
    Ok(base)
}

/// for each function, a vtable it is in and the live objects that have it:
/// a word in code or read-only data equal to the function is a slot of a
/// vtable that starts a few words before; the start the most objects in the
/// heap point to is it.
fn classes(memory: &mut Memory, entries: &[u32]) -> HashMap<u32, (u32, Vec<u32>)> {
    let wanted: HashSet<u32> = entries.iter().copied().collect();
    let mut chunks: Vec<(u32, u32, MemoryState)> = memory.mappings().map(|m| (m.base, m.size, m.state)).collect();
    chunks.sort_by_key(|&(base, ..)| base);
    let read = |memory: &mut Memory, base: u32, size: u32| {
        let mut bytes = vec![0u8; size as usize];
        memory.read_bytes(base, &mut bytes);
        bytes
    };
    // the slots
    let mut candidates: HashMap<u32, Vec<(u32, u32)>> = HashMap::new(); // start -> (entry, words before)
    for &(base, size, state) in &chunks {
        if !matches!(state, MemoryState::Code | MemoryState::Static | MemoryState::AliasCode) {
            continue;
        }
        let bytes = read(memory, base, size);
        for (i, word) in bytes.chunks_exact(4).enumerate() {
            let value = u32::from_le_bytes(word.try_into().unwrap());
            if wanted.contains(&value) {
                let slot = base + 4 * i as u32;
                for k in 0..96u32 {
                    candidates.entry(slot.wrapping_sub(4 * k)).or_default().push((value, k));
                }
            }
        }
    }
    // the objects
    let mut objects: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(base, size, state) in &chunks {
        if !matches!(state, MemoryState::Private | MemoryState::Continuous | MemoryState::Shared | MemoryState::Aliased) {
            continue;
        }
        let bytes = read(memory, base, size);
        for (i, word) in bytes.chunks_exact(4).enumerate() {
            let value = u32::from_le_bytes(word.try_into().unwrap());
            if candidates.contains_key(&value) {
                let found = objects.entry(value).or_default();
                if found.len() < 64 {
                    found.push(base + 4 * i as u32);
                }
            }
        }
    }
    let mut best: HashMap<u32, (u32, Vec<u32>, u32)> = HashMap::new();
    let mut runs_back: HashMap<u32, u32> = HashMap::new();
    for (&start, list) in &candidates {
        for &(entry, k) in list {
            // the slot itself is start + 4k; the vtable starts where the
            // code pointers before it stop
            if k == 0 {
                let mut at = start;
                let mut back = 0;
                while back < 95 {
                    let mut word = [0u8; 4];
                    memory.read_bytes(at.wrapping_sub(4), &mut word);
                    let value = u32::from_le_bytes(word);
                    let code = memory.mapping_at(value & !1).is_some_and(|m| matches!(m.state, MemoryState::Code | MemoryState::Static | MemoryState::AliasCode));
                    if !code {
                        break;
                    }
                    at = at.wrapping_sub(4);
                    back += 1;
                }
                runs_back.entry(entry).or_insert(at);
            }
        }
    }
    for (start, found) in objects {
        for &(entry, k) in &candidates[&start] {
            let better = match best.get(&entry) {
                None => true,
                Some((_, old, old_k)) => found.len() > old.len() || (found.len() == old.len() && k < *old_k),
            };
            if better {
                best.insert(entry, (start, found.clone(), k));
            }
        }
    }
    let mut classes: HashMap<u32, (u32, Vec<u32>)> = best.into_iter().map(|(entry, (start, found, _))| (entry, (start, found))).collect();
    for (entry, start) in runs_back {
        classes.entry(entry).or_insert((start, Vec::new()));
    }
    classes
}

/// what came of a function's runs.
#[derive(Default)]
pub struct Synthesized {
    pub runs: u32,
    pub same: u32,
    pub differ: u32,
    pub incomplete: u32,
    /// live objects of its class the runs used.
    pub objects: usize,
    pub told: Vec<String>,
}

/// every function the decomp replaces, runs times each; modules the state
/// has not loaded are loaded from romfs first.
pub fn synthesize(system: &mut System, romfs: &Path, runs: u32, seed: u64) -> (BTreeMap<u32, Synthesized>, Vec<String>) {
    let mut notes = Vec::new();
    let loaded: HashSet<String> = system.cro.modules.iter().map(|m| m.name.trim_end_matches(".cro").to_owned()).collect();
    let wanted: Vec<String> = system.decomp_module_hooks.iter().map(|hook| hook.module.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    for name in wanted.iter().filter(|name| !loaded.contains(*name)) {
        match load_module(system, romfs, name) {
            Ok(base) => notes.push(format!("loaded {name}.cro at 0x{base:08X}")),
            Err(error) => notes.push(format!("could not load {name}.cro: {error}")),
        }
    }
    let Some(check) = system.decomp_check.as_ref() else {
        notes.push("no decomp check (run with --decomp-check)".into());
        return (BTreeMap::new(), notes);
    };
    let hooks = check.hooks.clone();
    let segment = check.segment;
    let kinds: HashMap<u32, String> = hooks.iter().map(|&(old, _)| (old, check.kind(old).to_owned())).collect();
    let entries: Vec<u32> = hooks.iter().map(|&(old, _)| old).collect();
    let classes = classes(&mut system.memory, &entries);

    let base_cpu = system.cpu.clone();
    let mut results = BTreeMap::new();
    for &(old, new) in &hooks {
        let result: &mut Synthesized = results.entry(old).or_default();
        let class = classes.get(&old);
        result.objects = class.map_or(0, |(_, found)| found.len());
        let started = std::time::Instant::now();
        if std::env::var_os("ZAKURO_SYNTH_TRACE").is_some() {
            eprintln!("synth trace: 0x{old:08X}");
        }
        for i in 0..runs {
            // the answer is in: it differs, or made-up inputs never get it
            // to the end (the runs that hit the limit cost the most); and no
            // function takes more than a few seconds of the whole run
            if result.differ >= TOLD || (result.same == 0 && result.incomplete >= 16) || result.incomplete >= 64
                || started.elapsed().as_secs() >= 5 {
                break;
            }
            let run_seed = mix(seed ^ (old as u64) << 20 ^ i as u64);
            let mut memory = Synth { memory: &mut system.memory, written: HashMap::new(), mapped: HashMap::new(), code: HashMap::new(), seed: run_seed, astray: false };
            let mut cpu = base_cpu.clone();
            let pick = |n: u64| mix(run_seed ^ n);
            // this: a live object every other run, else a made-up one with
            // the class's vtable
            let this = match class {
                Some((_, found)) if !found.is_empty() && i % 2 == 0 => found[(i as usize / 2) % found.len()],
                _ => {
                    let at = OBJECTS + (i % 0x100) * 0x1000;
                    if let Some((vtable, _)) = class {
                        for (k, byte) in vtable.to_le_bytes().into_iter().enumerate() {
                            memory.poke(at + k as u32, byte);
                        }
                    }
                    at
                }
            };
            cpu.regs[0] = this;
            for r in 1..4 {
                cpu.regs[r] = match pick(r as u64) % 6 {
                    0 => MADE_UP + (pick(10 + r as u64) as u32 % 0x10_0000) * 16,
                    1 => pick(20 + r as u64) as u32 % 8,
                    2 => 0,
                    3 => u32::MAX,
                    4 => this,
                    _ => pick(30 + r as u64) as u32,
                };
            }
            for s in 0..4 {
                cpu.vfp.regs[s] = [0.0f32, 1.0, 0.5, -1.0, 2.0, 100.0, 0.25, -30.0][pick(40 + s as u64) as usize % 8].to_bits();
            }
            cpu.regs[13] = STACK;
            result.runs += 1;
            let mut theirs_cpu = cpu.clone();
            let theirs = run(&mut theirs_cpu, &mut memory, old, RUN_LIMIT, None, None);
            if let Some(theirs) = &theirs {
                undo(&mut memory, &theirs.old);
            }
            let astray = memory.astray;
            let mut ours_cpu = cpu.clone();
            let ours = theirs.as_ref().and_then(|_| run(&mut ours_cpu, &mut memory, new, RUN_LIMIT, None, None));
            let (Some(theirs), Some(ours)) = (theirs, ours) else {
                result.incomplete += 1;
                continue;
            };
            if astray || memory.astray {
                result.incomplete += 1;
                continue;
            }
            let (differ, registers) = compare(&theirs, &ours, &cpu, segment, &kinds[&old]);
            if differ.is_empty() && registers.is_empty() {
                result.same += 1;
            } else {
                result.differ += 1;
                if result.differ <= TOLD {
                    result.told.push(format!(
                        "synth (r0=0x{:08X} r1=0x{:08X} r2=0x{:08X} r3=0x{:08X}): {}",
                        cpu.regs[0], cpu.regs[1], cpu.regs[2], cpu.regs[3],
                        tell(old, cpu.regs[14], &differ, &registers)
                    ));
                }
            }
        }
    }
    (results, notes)
}
