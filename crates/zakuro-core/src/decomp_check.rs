//! checks code written apart from the title against the functions it
//! replaces. the text is left as the title has it, and each call to a
//! replaced function runs twice from the same registers and memory, the
//! title's code and then ours, in the interpreter; what each wrote and
//! returned is compared. ours is kept, with the title's timing, so a run
//! goes on as the title's own would when the two agree.
//!
//! a run that makes a syscall is compared up to it: both have to reach the
//! same one, with the same arguments and memory. ours is then kept at that
//! point, the syscall made for real, and ours goes on as any code would,
//! unchecked past it.
//!
//! calls can also be kept as samples (Capture): the cpu at the call and
//! every byte either run read, enough to run both again with no title
//! running (replay). a call is kept when the title's code runs an
//! instruction no sample before it did, and a few more besides, so the
//! samples cover what the run reached.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use zakuro_cpu::{Bus, Cpu, Exit};

use crate::decomp::Decomp;
use crate::memory::Memory;
use crate::System;

/// where the two runs return to, never mapped.
const SENTINEL: u32 = 0xFFFF_FFF0;
/// instructions one run may take before it is given up on, and one run
/// on a fuzzed sample, which can easily loop for ever.
const LIMIT: u64 = 20_000_000;
const FUZZ_LIMIT: u64 = 20_000;
/// stack under the caller's that a run may use as scratch, left out.
const SCRATCH: u32 = 0x1_0000;
/// mismatches told in full, for each function.
pub(crate) const TOLD: u32 = 3;
/// samples kept for each function at most.
const SAMPLES: usize = 1000;
/// calls kept for each function whatever they run, the first ones, then
/// one in every so many.
const FIRST: usize = 32;
const EVERY: u32 = 4096;

#[derive(Default)]
pub struct Tally {
    pub calls: u32,
    pub same: u32,
    pub differ: u32,
    /// of the same, those compared up to a syscall only.
    pub to_syscall: u32,
    /// calls a run of which ran too long or stopped on something else.
    pub skipped: u32,
}

/// a call kept to be run again: the replaced function, the cpu at the
/// call, and the bytes the runs read, as they were before it.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Sample {
    pub entry: u32,
    pub cpu: Cpu,
    pub memory: Vec<(u32, Vec<u8>)>,
    /// the modules loaded then and where, for the decomp's calls into them.
    /// (name, base, segments)
    pub modules: Vec<(String, u32, Vec<u32>)>,
}

/// what a capture file holds: the samples, and each page of code they
/// ran, whole, so a fuzzed run can take a branch none of them did.
#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    samples: Vec<Sample>,
    pages: Vec<(u32, Vec<u8>)>,
}

#[derive(Default)]
pub struct Capture {
    pub samples: Vec<Sample>,
    pages: BTreeMap<u32, Vec<u8>>,
    /// for each function, the instructions of the title's code the samples
    /// run, and how many samples it has.
    seen: BTreeMap<u32, (BTreeSet<u32>, usize)>,
}

impl Capture {
    pub fn write(&self, path: &std::path::Path) -> Result<(), String> {
        let saved = Saved { samples: self.samples.clone(), pages: self.pages.iter().map(|(&page, bytes)| (page, bytes.clone())).collect() };
        let bytes = postcard::to_stdvec(&saved).map_err(|error| error.to_string())?;
        std::fs::write(path, lz4_flex::compress_prepend_size(&bytes)).map_err(|error| format!("{}: {error}", path.display()))
    }
}

pub struct Check {
    /// the replaced functions, the first odd for Thumb, and ours.
    pub hooks: Vec<(u32, u32)>,
    /// the decomp's own memory, left out of the comparison.
    pub segment: (u32, u32),
    /// what each returns, see Decomp::returns; int when not said.
    pub returns: BTreeMap<u32, String>,
    pub tally: BTreeMap<u32, Tally>,
    pub capture: Option<Capture>,
}

impl Check {
    pub fn new(hooks: Vec<(u32, u32)>, returns: BTreeMap<u32, String>, base: u32, size: u32) -> Check {
        Check { hooks, segment: (base, base + size), returns, tally: BTreeMap::new(), capture: None }
    }

    /// the replacement for a function starting at pc, in that state.
    #[inline(always)]
    pub fn hooked(&self, pc: u32, thumb: bool) -> Option<u32> {
        let entry = pc | thumb as u32;
        self.hooks.iter().find(|(old, _)| *old == entry).map(|&(_, new)| new)
    }

    /// a line for each function called, for the end of a run.
    pub fn report(&self) -> Vec<String> {
        self.tally
            .iter()
            .map(|(old, t)| {
                format!(
                    "decomp check 0x{old:08X}: {} calls, {} same, {} differ, {} skipped, {} to a syscall",
                    t.calls, t.same, t.differ, t.skipped, t.to_syscall
                )
            })
            .collect()
    }

    pub(crate) fn kind(&self, entry: u32) -> &str {
        self.returns.get(&entry).map_or("int", String::as_str)
    }
}

/// what became of a call, for System::step.
pub enum Call {
    /// checked, and the cpu is back at the caller.
    Done,
    /// checked up to this syscall, which is to be made now.
    Syscall(u32),
    /// not checked, the title's code is to run.
    Title,
}

/// memory as a run sees it, a byte at a time, without what reading or
/// writing through the bus does besides.
pub(crate) trait Raw: Bus {
    fn peek(&mut self, addr: u32) -> u8;
    fn poke(&mut self, addr: u32, value: u8);
    /// what a byte held before a run wrote it, for undoing the write.
    fn before(&mut self, addr: u32) -> u8 {
        self.peek(addr)
    }
    /// after undoing a run, what it wrote where nothing was is gone again.
    fn forget(&mut self) {}
    /// whether a run has read something the memory cannot give, and can
    /// stop.
    fn lost(&self) -> bool {
        false
    }
}

impl Raw for Memory {
    fn peek(&mut self, addr: u32) -> u8 {
        let mut value = [0];
        self.read_bytes(addr, &mut value);
        value[0]
    }
    fn poke(&mut self, addr: u32, value: u8) {
        self.write_bytes(addr, &[value]);
    }
}

/// the bytes of a sample, as memory. a byte it does not hold reads as 0
/// and is counted, the run then is not the one the sample saw; but for
/// fuzzing (lenient), where a byte of data not held is as good an input
/// as any, and only code not held counts.
#[derive(Default)]
struct Sparse {
    bytes: HashMap<u32, u8>,
    /// what every sample shares, the title's code and the decomp's.
    shared: Rc<HashMap<u32, u8>>,
    missing: u32,
    /// the bytes fetched as instructions, which fuzzing leaves alone.
    code: BTreeSet<u32>,
    /// bytes a run wrote that the sample did not hold.
    absent: Vec<u32>,
    lenient: bool,
    fetching: bool,
}

impl Raw for Sparse {
    fn peek(&mut self, addr: u32) -> u8 {
        match self.bytes.get(&addr).or_else(|| self.shared.get(&addr)) {
            Some(&value) => value,
            None => {
                if !self.lenient || self.fetching {
                    self.missing += 1;
                }
                0
            }
        }
    }
    fn poke(&mut self, addr: u32, value: u8) {
        self.bytes.insert(addr, value);
    }
    fn lost(&self) -> bool {
        self.missing > 0
    }
    fn before(&mut self, addr: u32) -> u8 {
        // a write to a byte not held, mostly the stack under the caller's,
        // is not a read of it
        match self.bytes.get(&addr).or_else(|| self.shared.get(&addr)) {
            Some(&value) => value,
            None => {
                self.absent.push(addr);
                0
            }
        }
    }
    fn forget(&mut self) {
        for addr in std::mem::take(&mut self.absent) {
            self.bytes.remove(&addr);
        }
    }
}

impl Bus for Sparse {
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
        self.code.extend(addr..addr.wrapping_add(4));
        self.fetching = true;
        let value = self.read32(addr);
        self.fetching = false;
        value
    }
    fn fetch16(&mut self, addr: u32) -> u16 {
        self.code.extend(addr..addr.wrapping_add(2));
        self.fetching = true;
        let value = self.read16(addr);
        self.fetching = false;
        value
    }
}

/// a bus that keeps what each byte held before a run first wrote it, and,
/// when asked, what each byte it read held before the run.
struct Logged<'a, M: Raw> {
    memory: &'a mut M,
    old: BTreeMap<u32, u8>,
    reads: Option<&'a mut BTreeMap<u32, u8>>,
}

impl<M: Raw> Logged<'_, M> {
    fn note(&mut self, addr: u32, len: u32) {
        for byte in addr..addr.wrapping_add(len) {
            if !self.old.contains_key(&byte) {
                let value = self.memory.before(byte);
                self.old.insert(byte, value);
            }
        }
    }
    fn read(&mut self, addr: u32, len: u32) {
        if let Some(reads) = self.reads.as_deref_mut() {
            for byte in addr..addr.wrapping_add(len) {
                // what the run wrote itself is not what it was given
                if !self.old.contains_key(&byte) && !reads.contains_key(&byte) {
                    reads.insert(byte, self.memory.peek(byte));
                }
            }
        }
    }
}

impl<M: Raw> Bus for Logged<'_, M> {
    fn read8(&mut self, addr: u32) -> u8 {
        self.read(addr, 1);
        self.memory.read8(addr)
    }
    fn read16(&mut self, addr: u32) -> u16 {
        self.read(addr, 2);
        self.memory.read16(addr)
    }
    fn read32(&mut self, addr: u32) -> u32 {
        self.read(addr, 4);
        self.memory.read32(addr)
    }
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read(addr, 4);
        self.memory.fetch32(addr)
    }
    fn fetch16(&mut self, addr: u32) -> u16 {
        self.read(addr, 2);
        self.memory.fetch16(addr)
    }
    fn write8(&mut self, addr: u32, value: u8) {
        self.note(addr, 1);
        self.memory.write8(addr, value)
    }
    fn write16(&mut self, addr: u32, value: u16) {
        self.note(addr, 2);
        self.memory.write16(addr, value)
    }
    fn write32(&mut self, addr: u32, value: u32) {
        self.note(addr, 4);
        self.memory.write32(addr, value)
    }
}

/// where a run stopped.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum End {
    Returned,
    Syscall(u32),
}

/// what a run left: the bytes it wrote, as they were before and after, and
/// the cpu where it stopped.
pub(crate) struct Run {
    pub(crate) old: BTreeMap<u32, u8>,
    pub(crate) new: BTreeMap<u32, u8>,
    pub(crate) cpu: Cpu,
    pub(crate) end: End,
}

/// runs from the cpu as it is, with lr at the sentinel, until it returns
/// there or makes a syscall; none when it ran more than limit or stopped
/// on anything else. its writes stay in memory. reads and visited, when
/// given, get the bytes it read and the instructions it ran.
pub(crate) fn run<M: Raw>(
    cpu: &mut Cpu,
    memory: &mut M,
    at: u32,
    limit: u64,
    reads: Option<&mut BTreeMap<u32, u8>>,
    mut visited: Option<&mut BTreeSet<u32>>,
) -> Option<Run> {
    cpu.regs[14] = SENTINEL;
    cpu.regs[15] = at & !1;
    cpu.cpsr.thumb = at & 1 != 0;
    let mut bus = Logged { memory, old: BTreeMap::new(), reads };
    let start = cpu.cycles;
    let mut end = None;
    // steps too: an instruction that counts no cycles (a wait) never ends
    // a run by cycles alone
    let mut steps = 0u64;
    while cpu.cycles - start < limit && steps < limit {
        steps += 1;
        if cpu.regs[15] & !1 == SENTINEL {
            end = Some(End::Returned);
            break;
        }
        if bus.memory.lost() {
            break;
        }
        if let Some(visited) = visited.as_deref_mut() {
            visited.insert(cpu.regs[15] | cpu.cpsr.thumb as u32);
        }
        match cpu.step(&mut bus) {
            None => {}
            Some(Exit::Supervisor(number)) => {
                end = Some(End::Syscall(number));
                break;
            }
            Some(_) => break,
        }
    }
    let Logged { memory, old, .. } = bus;
    let new = old.keys().map(|&addr| (addr, memory.peek(addr))).collect();
    end.map(|end| Run { old, new, cpu: cpu.clone(), end })
}

pub(crate) fn undo<M: Raw>(memory: &mut M, old: &BTreeMap<u32, u8>) {
    for (&addr, &value) in old {
        memory.poke(addr, value);
    }
}

/// what differs between the two runs of a call that started at caller:
/// bytes outside the scratch under its stack and the decomp's segment,
/// and registers, as lines.
pub(crate) fn compare(theirs: &Run, ours: &Run, caller: &Cpu, segment: (u32, u32), kind: &str) -> (Vec<(u32, u8, u8)>, Vec<String>) {
    let sp = caller.regs[13];
    let scratch = |addr: u32| sp.wrapping_sub(SCRATCH) <= addr && addr < sp;
    let kept = |addr: u32| !scratch(addr) && !(segment.0 <= addr && addr < segment.1);
    let mut differ = Vec::new();
    let addrs: BTreeSet<u32> = theirs.old.keys().chain(ours.old.keys()).copied().filter(|&a| kept(a)).collect();
    for addr in addrs {
        let before = theirs.old.get(&addr).or(ours.old.get(&addr)).copied().unwrap_or(0);
        let a = theirs.new.get(&addr).copied().unwrap_or(before);
        let b = ours.new.get(&addr).copied().unwrap_or(before);
        if a != b {
            differ.push((addr, a, b));
        }
    }
    let mut registers = Vec::new();
    match (theirs.end, ours.end) {
        (End::Returned, End::Returned) => {
            // the registers its return type uses, the stack pointer, and
            // the ones a function has to give back as it found them
            let returned: &[(&str, bool, usize)] = match kind {
                "void" => &[],
                // the game's calls are softfp: a float comes back in r0, a
                // double in r0 and r1. its code ends `vmov r0, s0; bx lr`,
                // so s0 holds the result too, and comparing s0 let C that
                // returned it in the wrong register pass
                "long" | "double" => &[("r0", false, 0), ("r1", false, 1)],
                "float" => &[("r0", false, 0)],
                _ => &[("r0", false, 0)],
            };
            let saved = (4..12).map(|r| (["r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11"][r - 4], false, r));
            let saved_vfp = (16..32).map(|s| ("s16-s31", true, s));
            for (name, vfp, r) in returned.iter().copied().chain([("sp", false, 13)]).chain(saved).chain(saved_vfp) {
                let (a, b) = if vfp { (theirs.cpu.vfp.regs[r], ours.cpu.vfp.regs[r]) } else { (theirs.cpu.regs[r], ours.cpu.regs[r]) };
                if a != b {
                    registers.push(format!("{name} 0x{a:08X} / 0x{b:08X}"));
                }
            }
        }
        (End::Syscall(a), End::Syscall(b)) if a == b => {
            // the arguments; pointers into each run's own stack frames
            // differ by layout alone, and are taken as the same
            for r in 0..4 {
                let (a, b) = (theirs.cpu.regs[r], ours.cpu.regs[r]);
                if a != b && !(scratch(a) && scratch(b)) {
                    registers.push(format!("r{r} 0x{a:08X} / 0x{b:08X}"));
                }
            }
        }
        (a, b) => {
            let say = |end: End| match end {
                End::Returned => "returned".to_owned(),
                End::Syscall(number) => format!("svc 0x{number:02X}"),
            };
            registers.push(format!("{} / {}", say(a), say(b)));
        }
    }
    (differ, registers)
}

pub(crate) fn tell(entry: u32, lr: u32, differ: &[(u32, u8, u8)], registers: &[String]) -> String {
    format!(
        "decomp check 0x{entry:08X} (lr 0x{lr:08X}): {} bytes differ{}{}",
        differ.len(),
        differ.iter().take(8).map(|(addr, a, b)| format!(", 0x{addr:08X} {a:02X}/{b:02X}")).collect::<String>(),
        registers.iter().map(|r| format!(", {r}")).collect::<String>()
    )
}

/// a call to a replaced function, at the cpu's pc: both runs, the
/// comparison, and the cpu left with our code's results, at the caller or
/// at the syscall both reached.
pub fn call(system: &mut System, new: u32) -> Call {
    let entry = system.cpu.regs[15] | system.cpu.cpsr.thumb as u32;
    let caller = system.cpu.clone();
    let segment = system.decomp_check.as_ref().map_or((0, 0), |check| check.segment);
    let capturing = system.decomp_check.as_ref().is_some_and(|check| check.capture.is_some());
    let mut reads = BTreeMap::new();
    let mut visited = BTreeSet::new();

    let gpu = &mut system.gpu as *mut zakuro_gpu::Gpu as *mut ();
    let linear_base = system.kernel.linear_base;
    // SAFETY: as in System::step, the GPU stays put while the cpu runs
    unsafe {
        system.memory.set_gpu_sync(crate::memory::GpuSync {
            gpu,
            linear_base,
            sync: crate::sync_for_cpu,
            sync_write: crate::sync_for_cpu_write,
        })
    };
    let mut cpu = caller.clone();
    let theirs = run(&mut cpu, &mut system.memory, entry, LIMIT, capturing.then_some(&mut reads), capturing.then_some(&mut visited));
    if let Some(theirs) = &theirs {
        undo(&mut system.memory, &theirs.old);
    }
    let mut cpu = caller.clone();
    let ours = theirs.as_ref().and_then(|_| run(&mut cpu, &mut system.memory, new, LIMIT, capturing.then_some(&mut reads), None));
    let (theirs, ours) = match (theirs, ours) {
        (Some(theirs), Some(ours)) => (theirs, ours),
        (_, ours) => {
            if let Some(ours) = ours {
                undo(&mut system.memory, &ours.old);
            }
            system.memory.clear_gpu_sync();
            system.cpu = caller;
            if let Some(check) = system.decomp_check.as_mut() {
                let tally = check.tally.entry(entry).or_default();
                tally.calls += 1;
                tally.skipped += 1;
            }
            return Call::Title;
        }
    };
    system.memory.clear_gpu_sync();

    let check = system.decomp_check.as_mut().expect("a decomp check");
    let (differ, registers) = compare(&theirs, &ours, &caller, segment, check.kind(entry));
    let tally = check.tally.entry(entry).or_default();
    tally.calls += 1;
    let calls = tally.calls;
    if !differ.is_empty() || !registers.is_empty() {
        tally.differ += 1;
        if tally.differ <= TOLD {
            log::warn!("{}", tell(entry, caller.regs[14], &differ, &registers));
        }
    } else {
        tally.same += 1;
        if theirs.end != End::Returned {
            tally.to_syscall += 1;
        }
    }
    if let Some(capture) = check.capture.as_mut() {
        let (seen, kept) = capture.seen.entry(entry).or_default();
        let new_ground = visited.iter().any(|pc| !seen.contains(pc));
        if *kept < SAMPLES && (new_ground || *kept < FIRST || calls % EVERY == 0) {
            // code pages are kept once for all samples; a page another
            // module has been loaded over since goes with this sample
            let mut own_pages: Vec<(u32, Vec<u8>)> = Vec::new();
            for page in visited.iter().map(|pc| pc & !0xFFF).collect::<BTreeSet<u32>>() {
                let mut bytes = vec![0; 0x1000];
                system.memory.read_bytes(page, &mut bytes);
                match capture.pages.get(&page) {
                    None => {
                        capture.pages.insert(page, bytes);
                    }
                    Some(kept_bytes) if *kept_bytes != bytes => own_pages.push((page, bytes)),
                    Some(_) => {}
                }
            }
            seen.extend(visited);
            *kept += 1;
            let mut memory: Vec<(u32, Vec<u8>)> = own_pages;
            // the decomp's own bytes come from its build at replay, not
            // from this one
            for (addr, value) in reads.into_iter().filter(|&(addr, _)| !(segment.0 <= addr && addr < segment.1)) {
                match memory.last_mut() {
                    Some((start, bytes)) if *start + bytes.len() as u32 == addr => bytes.push(value),
                    _ => memory.push((addr, vec![value])),
                }
            }
            let modules = system.cro.modules.iter().map(|m| (m.name.trim_end_matches(".cro").to_owned(), m.base, m.segments.clone())).collect();
            capture.samples.push(Sample { entry, cpu: caller.clone(), memory, modules });
        }
    }

    // ours stays, the cycles as the title's code took
    let cycles = theirs.cpu.cycles;
    system.cpu = ours.cpu;
    system.cpu.cycles = cycles;
    match ours.end {
        End::Returned => {
            // back at the caller
            system.cpu.regs[15] = caller.regs[14] & !1;
            system.cpu.cpsr.thumb = caller.regs[14] & 1 != 0;
            system.cpu.regs[14] = caller.regs[14];
            Call::Done
        }
        End::Syscall(number) => {
            // the sentinel is still where ours returns to, in lr or in its
            // frame: from here ours runs as any code, so it is pointed
            // back at the caller's return
            if system.cpu.regs[14] & !1 == SENTINEL {
                system.cpu.regs[14] = caller.regs[14];
            }
            fix_stack(&mut system.memory, system.cpu.regs[13], caller.regs[13], caller.regs[14]);
            Call::Syscall(number)
        }
    }
}

/// a run stopped at a syscall saved the sentinel as its return address in
/// its frame, between its sp and the caller's: put the caller's there.
fn fix_stack(memory: &mut Memory, low: u32, high: u32, lr: u32) {
    let mut addr = low & !3;
    while addr < high {
        if memory.read32(addr) & !1 == SENTINEL {
            memory.write32(addr, lr);
        }
        addr += 4;
    }
}

/// for each function, the samples run again and how they fared, and the
/// instructions of the title's code they ran; the same for the fuzzed
/// ones, and the instructions only those reached.
#[derive(Default)]
pub struct Replayed {
    pub samples: u32,
    pub same: u32,
    pub differ: u32,
    /// runs that read a byte the sample does not hold, or stopped early.
    pub incomplete: u32,
    pub covered: BTreeSet<u32>,
    pub fuzzed: u32,
    pub fuzz_same: u32,
    pub fuzz_differ: u32,
    pub fuzz_incomplete: u32,
    pub fuzz_covered: BTreeSet<u32>,
    /// `<module>.cro:0x<offset>` for a function of a module, which loads
    /// somewhere else in each run.
    pub module: Option<String>,
}

enum Outcome {
    Same,
    Differ(String),
    Incomplete,
}

/// both codes from cpu over memory, compared.
fn replay_one(cpu: &Cpu, mut memory: Sparse, entry: u32, new: u32, decomp: &Decomp, limit: u64, covered: &mut BTreeSet<u32>) -> (Outcome, BTreeSet<u32>) {
    let segment = (decomp.base, decomp.base + decomp.code.len() as u32);
    let mut theirs_cpu = cpu.clone();
    let theirs = run(&mut theirs_cpu, &mut memory, entry, limit, None, Some(covered));
    if let Some(theirs) = &theirs {
        undo(&mut memory, &theirs.old);
        memory.forget();
    }
    let missing = memory.missing;
    let mut ours_cpu = cpu.clone();
    let ours = theirs.as_ref().and_then(|_| run(&mut ours_cpu, &mut memory, new, limit, None, None));
    let code = std::mem::take(&mut memory.code);
    let (Some(theirs), Some(ours)) = (theirs, ours) else {
        return (Outcome::Incomplete, code);
    };
    // by our code, which a module's function has wherever the module was
    let kind = decomp
        .returns
        .get(&entry)
        .map(String::as_str)
        .or_else(|| decomp.module_hooks.iter().find(|hook| hook.new == new).and_then(|hook| hook.kind.as_deref()))
        .unwrap_or("int");
    let (differ, registers) = compare(&theirs, &ours, cpu, segment, kind);
    let outcome = if missing > 0 || memory.missing > missing {
        // a run read a byte the sample does not hold (ours, mostly, when it
        // reads what the code it was taken with did not): neither result
        // says anything
        Outcome::Incomplete
    } else if !differ.is_empty() || !registers.is_empty() {
        Outcome::Differ(tell(entry, cpu.regs[14], &differ, &registers))
    } else {
        Outcome::Same
    };
    (outcome, code)
}

/// the decomp's code for the function a sample calls: in the text, or in
/// a module, where the sample had it loaded.
fn replacement(decomp: &Decomp, sample: &Sample) -> Option<u32> {
    let text = decomp.hooks.iter().find(|(old, _)| *old == sample.entry).map(|&(_, new)| new);
    text.or_else(|| {
        decomp.module_hooks.iter().find_map(|hook| {
            let (_, base, _) = sample.modules.iter().find(|(name, ..)| *name == hook.module)?;
            (base + hook.offset == sample.entry).then_some(hook.new)
        })
    })
}

/// a small generator of numbers, the same every time.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n.max(1)
    }
}

/// a value near the edges a function tells apart, from one it had.
fn edge(rng: &mut Rng, value: u32) -> u32 {
    match rng.below(9) {
        0 => 0,
        1 => 1,
        2 => u32::MAX,
        3 => value.wrapping_add(1),
        4 => value.wrapping_sub(1),
        5 => 0x7FFF_FFFF,
        6 => 0x8000_0000,
        7 => value ^ (1 << rng.below(32)),
        _ => rng.next(),
    }
}

/// runs the title's code and the decomp's again on each sample in file,
/// with nothing of the title but the bytes the sample holds; then, fuzz
/// times for each function, on a sample with its arguments or a word of
/// the data it read changed.
pub fn replay(file: &std::path::Path, decomp: &Decomp, fuzz: u32) -> Result<(BTreeMap<u32, Replayed>, Vec<String>), String> {
    let packed = std::fs::read(file).map_err(|error| format!("{}: {error}", file.display()))?;
    let bytes = lz4_flex::decompress_size_prepended(&packed).map_err(|error| error.to_string())?;
    let Saved { samples, pages } = postcard::from_bytes(&bytes).map_err(|error| format!("{}: {error} (taken by an older zakuro? capture again)", file.display()))?;
    let mut shared = HashMap::new();
    for (page, bytes) in &pages {
        shared.extend(bytes.iter().enumerate().map(|(i, &byte)| (page + i as u32, byte)));
    }
    shared.extend(decomp.code.iter().enumerate().map(|(i, &byte)| (decomp.base + i as u32, byte)));
    let shared = Rc::new(shared);
    let mut results: BTreeMap<u32, Replayed> = BTreeMap::new();
    let mut told = Vec::new();
    // each sample's memory, and its data words, for fuzzing
    let mut kept: BTreeMap<u32, Vec<(&Sample, Sparse, Vec<u32>)>> = BTreeMap::new();
    for sample in &samples {
        let result = results.entry(sample.entry).or_default();
        result.samples += 1;
        if result.module.is_none() {
            result.module = decomp.module_hooks.iter().find_map(|hook| {
                let (_, base, _) = sample.modules.iter().find(|(name, ..)| *name == hook.module)?;
                (base + hook.offset == sample.entry).then(|| format!("{}.cro:0x{:08X}", hook.module, hook.offset))
            });
        }
        let Some(new) = replacement(decomp, sample) else {
            // a function the decomp no longer replaces
            result.incomplete += 1;
            continue;
        };
        let mut memory = Sparse { shared: shared.clone(), ..Default::default() };
        for (start, bytes) in &sample.memory {
            for (i, &byte) in bytes.iter().enumerate() {
                let addr = start + i as u32;
                // over the decomp, the build now is what runs; the code pages
                // are shared, and a sample keeps only what differs from them
                let in_decomp = decomp.base <= addr && addr < decomp.base + decomp.code.len() as u32;
                if !in_decomp && shared.get(&addr) != Some(&byte) {
                    memory.bytes.insert(addr, byte);
                }
            }
        }
        // the decomp's calls into modules, filled as the module was loaded
        for (module, offset, slot, segment) in &decomp.module_imports {
            if let Some((_, base, segments)) = sample.modules.iter().find(|(name, ..)| name == module) {
                let address = crate::decomp::module_address(*base, segments, *offset, *segment);
                for (i, byte) in address.to_le_bytes().into_iter().enumerate() {
                    memory.bytes.insert(slot + i as u32, byte);
                }
            }
        }
        let clean = Sparse { bytes: memory.bytes.clone(), shared: shared.clone(), ..Default::default() };
        let (outcome, code) = replay_one(&sample.cpu, memory, sample.entry, new, decomp, LIMIT, &mut result.covered);
        match outcome {
            Outcome::Same => result.same += 1,
            Outcome::Incomplete => result.incomplete += 1,
            Outcome::Differ(line) => {
                result.differ += 1;
                if result.differ <= TOLD {
                    told.push(line);
                }
            }
        }
        let segment = decomp.base..decomp.base + decomp.code.len() as u32;
        let words: Vec<u32> = sample
            .memory
            .iter()
            .flat_map(|(start, bytes)| (*start..*start + bytes.len() as u32).filter(|a| a % 4 == 0))
            .filter(|a| !segment.contains(a) && !(*a..a + 4).any(|b| code.contains(&b)) && (*a..a + 4).all(|b| clean.bytes.contains_key(&b)))
            .collect();
        kept.entry(sample.entry).or_default().push((sample, clean, words));
    }

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for (entry, list) in &kept {
        let new = list.first().and_then(|(sample, _, _)| replacement(decomp, sample)).unwrap_or(0);
        let result = results.get_mut(entry).expect("a result for each sample's function");
        let mut fuzz_told = 0;
        for _ in 0..fuzz {
            let (sample, clean, words) = &list[rng.below(list.len())];
            let mut cpu = sample.cpu.clone();
            let mut memory = Sparse { bytes: clean.bytes.clone(), shared: shared.clone(), lenient: true, ..Default::default() };
            let mut said = Vec::new();
            for _ in 0..1 + rng.below(3) {
                if words.is_empty() || rng.below(3) == 0 {
                    let r = rng.below(4);
                    cpu.regs[r] = edge(&mut rng, cpu.regs[r]);
                    said.push(format!("r{r}=0x{:08X}", cpu.regs[r]));
                } else {
                    let at = words[rng.below(words.len())];
                    let value = u32::from_le_bytes([0, 1, 2, 3].map(|i| clean.bytes[&(at + i)]));
                    let value = edge(&mut rng, value);
                    for (i, byte) in value.to_le_bytes().into_iter().enumerate() {
                        memory.bytes.insert(at + i as u32, byte);
                    }
                    said.push(format!("[0x{at:08X}]=0x{value:08X}"));
                }
            }
            result.fuzzed += 1;
            let mut covered = BTreeSet::new();
            let (outcome, _) = replay_one(&cpu, memory, *entry, new, decomp, FUZZ_LIMIT, &mut covered);
            match outcome {
                Outcome::Same => {
                    result.fuzz_same += 1;
                    result.fuzz_covered.extend(covered.difference(&result.covered).copied().collect::<Vec<_>>());
                }
                Outcome::Incomplete => result.fuzz_incomplete += 1,
                Outcome::Differ(line) => {
                    result.fuzz_differ += 1;
                    result.fuzz_covered.extend(covered.difference(&result.covered).copied().collect::<Vec<_>>());
                    if fuzz_told < TOLD {
                        fuzz_told += 1;
                        told.push(format!("fuzzed ({}): {line}", said.join(" ")));
                    }
                }
            }
        }
    }
    Ok((results, told))
}

/// where exhaustive runs keep their stack, nothing of the title's.
const EXHAUSTIVE_SP: u32 = 0x0FF0_0000;

/// a line of a spec: a function and, for each argument register, the
/// values it takes, every combination of which is run.
fn spec_line(line: &str) -> Result<(u32, Vec<(usize, Vec<u32>)>), String> {
    let mut words = line.split_whitespace();
    let number = |word: &str| {
        let word = word.trim();
        match word.strip_prefix("0x") {
            Some(hex) => u32::from_str_radix(hex, 16),
            None => word.parse(),
        }
        .map_err(|_| format!("'{word}' is not a number, in '{line}'"))
    };
    let entry = number(words.next().ok_or("an empty line")?)?;
    let mut registers = Vec::new();
    for word in words {
        let (register, values) = word.split_once('=').ok_or_else(|| format!("'{word}' is not rN=values, in '{line}'"))?;
        let register: usize = register.trim_start_matches('r').parse().map_err(|_| format!("'{register}' is not a register"))?;
        if register > 3 {
            return Err(format!("r{register}: only r0-r3 take arguments"));
        }
        let mut list = Vec::new();
        for part in values.split(',') {
            match part.split_once('-') {
                Some((low, high)) => list.extend(number(low)?..=number(high)?),
                None => list.push(number(part)?),
            }
        }
        registers.push((register, list));
    }
    Ok((entry, registers))
}

/// runs the title's code and the decomp's on every combination of
/// arguments a spec gives each function (lines `0xADDRESS r0=0-13
/// r1=0,1`; `#` starts a comment), with nothing in memory but the title's
/// code.bin at 0x00100000 and the decomp: for functions of small numbers
/// and tables of the title, a check of every input they take.
pub fn exhaustive(spec: &str, code: &[u8], decomp: &Decomp) -> Result<(BTreeMap<u32, Replayed>, Vec<String>), String> {
    let mut shared = HashMap::new();
    shared.extend(code.iter().enumerate().map(|(i, &byte)| (0x0010_0000 + i as u32, byte)));
    shared.extend(decomp.code.iter().enumerate().map(|(i, &byte)| (decomp.base + i as u32, byte)));
    let shared = Rc::new(shared);
    let mut results: BTreeMap<u32, Replayed> = BTreeMap::new();
    let mut told = Vec::new();
    for line in spec.lines().map(|line| line.split('#').next().unwrap_or("").trim()).filter(|line| !line.is_empty()) {
        let (entry, registers) = spec_line(line)?;
        let new = decomp.hooks.iter().find(|(old, _)| *old == entry).map(|&(_, new)| new).ok_or_else(|| format!("0x{entry:08X} is not replaced by the decomp"))?;
        let result = results.entry(entry).or_default();
        let mut index = vec![0usize; registers.len()];
        loop {
            let mut cpu = Cpu::new();
            cpu.regs[13] = EXHAUSTIVE_SP;
            for (i, (register, values)) in registers.iter().enumerate() {
                cpu.regs[*register] = values[index[i]];
            }
            let memory = Sparse { shared: shared.clone(), ..Default::default() };
            result.samples += 1;
            match replay_one(&cpu, memory, entry, new, decomp, LIMIT, &mut result.covered).0 {
                Outcome::Same => result.same += 1,
                Outcome::Incomplete => result.incomplete += 1,
                Outcome::Differ(line) => {
                    result.differ += 1;
                    if result.differ <= TOLD {
                        let args: Vec<String> = registers.iter().map(|(r, _)| format!("r{r}={}", cpu.regs[*r])).collect();
                        told.push(format!("exhaustive ({}): {line}", args.join(" ")));
                    }
                }
            }
            // the next combination, the last register fastest
            let mut i = registers.len();
            loop {
                if i == 0 {
                    break;
                }
                i -= 1;
                index[i] += 1;
                if index[i] < registers[i].1.len() {
                    break;
                }
                index[i] = 0;
                if i == 0 {
                    i = usize::MAX;
                    break;
                }
            }
            if i == usize::MAX || registers.is_empty() {
                break;
            }
        }
    }
    Ok((results, told))
}
