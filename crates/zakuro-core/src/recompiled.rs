//! running code that 3dsrecomp turned into a library ahead of time. the
//! library's functions work on a context holding the registers and reach
//! memory through the same page tables the interpreter uses, calling back
//! here for anything else. whatever the library has no code for stays with
//! the interpreter.

use std::cell::Cell;
use std::ffi::c_void;
use std::path::Path;

use recomp_abi::{Code, Context, Host, EXIT_BUDGET, EXIT_SVC, EXIT_UNWIND};
use zakuro_cpu::{Bus, Cpu, Exit};

use crate::memory::Memory;

pub use recomp_abi::Linked;

/// the library 3dsrecomp build installed for a title, if there is one.
pub fn installed(program_id: u64) -> Option<std::path::PathBuf> {
    recomp_abi::installed(program_id)
}

/// recompiled code, and how much of its work it handed back.
pub struct Library {
    code: recomp_abi::Library,
    /// instructions the code handed to the interpreter one at a time.
    fallbacks: Cell<u64>,
    /// the code found for addresses lately, each in a slot its address
    /// picks. a jump through a register looks its target up, which is a
    /// search through every function otherwise.
    found: Box<[Found]>,
    /// what the code says a touch of the top screen stands for, see
    /// Library::top_touch.
    top_touch: Option<TopTouch>,
}

/// recomp_top_touch(x, y, &bottom_x, &bottom_y), a function the overrides
/// of a title shown on one screen may export: the point of the bottom
/// screen that a point of the top one stands for, nonzero when there is one.
type TopTouch = unsafe extern "C" fn(i32, i32, *mut i32, *mut i32) -> i32;

/// a slot of Library::found, an address and the code found for it.
type Found = Cell<(u32, Option<Code>)>;

/// slots of Library::found, a power of two.
const FOUND: usize = 65536;

/// the slot of Library::found an address goes in, the top bits of the
/// address times the golden ratio. ARM addresses step by four and Thumb ones
/// by two with bit 0 set, and taking low bits would leave half the slots to
/// one of them.
fn slot(address: u32) -> usize {
    (address.wrapping_mul(0x9E37_79B1) >> (32 - FOUND.trailing_zeros())) as usize
}

/// why a run of recompiled code ended.
pub enum Stop {
    /// an svc, the cpu is already past it.
    Svc(u32),
    /// the interpreter stopped on an instruction the code handed it.
    Exit(Exit),
    /// the budget ran out, or execution left for code the library does not
    /// have.
    Left,
}

/// what the callbacks reach through the context.
struct Machine<'a> {
    cpu: &'a mut Cpu,
    memory: &'a mut Memory,
    library: &'a Library,
    /// what the interpreter stopped with inside a callback.
    pending: Option<Exit>,
    /// carries out a system call for run, on the cpu as it is; false when
    /// the thread has to wait.
    svc: &'a mut dyn FnMut(u32) -> bool,
}

/// # Safety
///
/// ctx has to be a context run set up, whose user field points at a live
/// Machine.
unsafe fn machine<'a>(ctx: *mut Context) -> &'a mut Machine<'a> {
    unsafe { &mut *((*ctx).user as *mut Machine) }
}

unsafe extern "C" fn read8(ctx: *mut Context, address: u32) -> u8 {
    unsafe { machine(ctx).memory.read8(address) }
}

unsafe extern "C" fn read16(ctx: *mut Context, address: u32) -> u16 {
    unsafe { machine(ctx).memory.read16(address) }
}

unsafe extern "C" fn read32(ctx: *mut Context, address: u32) -> u32 {
    unsafe { machine(ctx).memory.read32(address) }
}

unsafe extern "C" fn write8(ctx: *mut Context, address: u32, value: u8) {
    unsafe { machine(ctx).memory.write8(address, value) }
}

unsafe extern "C" fn write16(ctx: *mut Context, address: u32, value: u16) {
    unsafe { machine(ctx).memory.write16(address, value) }
}

unsafe extern "C" fn write32(ctx: *mut Context, address: u32, value: u32) {
    unsafe { machine(ctx).memory.write32(address, value) }
}

/// runs one instruction the code left to the interpreter.
unsafe extern "C" fn interpret(ctx: *mut Context, address: u32, _opcode: u32) {
    unsafe {
        let machine = machine(ctx);
        machine.library.fallbacks.set(machine.library.fallbacks.get() + 1);
        let ctx = &mut *ctx;
        load(ctx, machine.cpu);
        machine.cpu.regs[15] = address;
        let exit = machine.cpu.step(machine.memory);
        store(machine.cpu, ctx);
        let step = if ctx.thumb != 0 { 2 } else { 4 };
        if exit.is_some() || ctx.r[15] != address.wrapping_add(step) {
            machine.pending = exit;
            ctx.exit = EXIT_UNWIND;
        }
    }
}

unsafe extern "C" fn lookup(ctx: *mut Context, address: u32) -> Option<Code> {
    unsafe { machine(ctx).library.lookup(address) }
}

/// how many instructions a run for C may take before it gives up, a guest
/// that never gets back to stop.
const RUN_LIMIT: i64 = 1 << 30;

/// runs the guest on to stop for C calling into it (an override, or code
/// written by hand): the code where there is any, the interpreter elsewhere,
/// system calls as the scheduler's step would. a thread cannot wait in the
/// middle of C, so a system call that has it wait ends the run with the
/// thread waiting; the C returns, and the guest goes on from where it was
/// once the thread wakes, towards a return address no code is at.
unsafe extern "C" fn run(ctx: *mut Context, stop: u32) {
    unsafe {
        let machine = machine(ctx);
        let budget = (*ctx).budget;
        let mut used: i64 = 0;
        loop {
            if let Some(exit) = machine.pending.take() {
                match exit {
                    Exit::Supervisor(number) => {
                        (*ctx).svc = number;
                        (*ctx).exit = EXIT_SVC;
                    }
                    other => {
                        log::error!("a call from C into the guest stopped on {other:?} at 0x{:08X}", (*ctx).r[15]);
                        machine.pending = Some(other);
                        (*ctx).exit = EXIT_UNWIND;
                        break;
                    }
                }
            }
            if (*ctx).exit == EXIT_SVC {
                let number = (*ctx).svc;
                load(&*ctx, machine.cpu);
                let went_on = (machine.svc)(number);
                store(machine.cpu, &mut *ctx);
                if !went_on {
                    log::error!("a call from C into the guest made system call 0x{number:02X}, which waits; the call is lost");
                    // the scheduler has to look before anything runs on
                    (*ctx).exit = EXIT_BUDGET;
                    break;
                }
            }
            (*ctx).exit = 0;
            if (*ctx).r[15] == stop && (*ctx).thumb == 0 {
                break;
            }
            if used > RUN_LIMIT {
                log::error!("a call from C into the guest did not return, at 0x{:08X}", (*ctx).r[15]);
                (*ctx).exit = EXIT_UNWIND;
                break;
            }
            if let Some(code) = machine.library.lookup((*ctx).r[15] | (*ctx).thumb as u32) {
                (*ctx).budget = i32::MAX / 2;
                code(ctx);
                used += (i32::MAX / 2 - (*ctx).budget) as i64;
                if (*ctx).exit == EXIT_BUDGET {
                    (*ctx).exit = 0;
                }
                continue;
            }
            machine.library.fallbacks.set(machine.library.fallbacks.get() + 1);
            load(&*ctx, machine.cpu);
            let exit = machine.cpu.step(machine.memory);
            store(machine.cpu, &mut *ctx);
            used += 1;
            machine.pending = exit;
        }
        (*ctx).budget = (budget as i64 - used).max(i32::MIN as i64) as i32;
    }
}

static HOST: Host = Host { read8, read16, read32, write8, write16, write32, interpret, lookup, run };

fn load(ctx: &Context, cpu: &mut Cpu) {
    cpu.regs = ctx.r;
    cpu.cpsr.n = ctx.n != 0;
    cpu.cpsr.z = ctx.z != 0;
    cpu.cpsr.c = ctx.c != 0;
    cpu.cpsr.v = ctx.v != 0;
    cpu.cpsr.q = ctx.q != 0;
    cpu.cpsr.ge = ctx.ge;
    cpu.cpsr.thumb = ctx.thumb != 0;
    cpu.exclusive_addr = (ctx.exclusive != 0).then_some(ctx.exclusive_address);
}

fn store(cpu: &Cpu, ctx: &mut Context) {
    ctx.r = cpu.regs;
    ctx.n = cpu.cpsr.n as u8;
    ctx.z = cpu.cpsr.z as u8;
    ctx.c = cpu.cpsr.c as u8;
    ctx.v = cpu.cpsr.v as u8;
    ctx.q = cpu.cpsr.q as u8;
    ctx.ge = cpu.cpsr.ge;
    ctx.thumb = cpu.cpsr.thumb as u8;
    ctx.exclusive = cpu.exclusive_addr.is_some() as u8;
    ctx.exclusive_address = cpu.exclusive_addr.unwrap_or(0);
    ctx.tls = cpu.cp15.thread_id_ro;
}

impl Library {
    /// a library 3dsrecomp built.
    pub fn open(path: &Path) -> Result<Library, String> {
        recomp_abi::Library::open(path).map(Library::new)
    }

    /// the code linked into the program.
    pub fn linked(linked: &Linked) -> Result<Library, String> {
        recomp_abi::Library::linked(linked).map(Library::new)
    }

    fn new(code: recomp_abi::Library) -> Library {
        // SAFETY: an override exporting the name gives it this type
        let top_touch = code.symbol(b"recomp_top_touch").map(|address| unsafe { std::mem::transmute::<*const c_void, TopTouch>(address) });
        Library { code, fallbacks: Cell::new(0), found: Library::nothing_found(), top_touch }
    }

    /// the point of the bottom screen a touch of the top one at x, y stands
    /// for, when the title's overrides draw the bottom screen's things on
    /// the top one and say so.
    pub fn top_touch(&self, x: u16, y: u16) -> Option<(u16, u16)> {
        let top_touch = self.top_touch?;
        let (mut bottom_x, mut bottom_y) = (0, 0);
        // SAFETY: the function only reads its arguments and writes the two
        let found = unsafe { top_touch(x.into(), y.into(), &mut bottom_x, &mut bottom_y) };
        (found != 0).then(|| (bottom_x.clamp(0, 319) as u16, bottom_y.clamp(0, 239) as u16))
    }

    /// whether the title's overrides can tell what a touch of the top
    /// screen stands for.
    pub fn takes_top_touch(&self) -> bool {
        self.top_touch.is_some()
    }

    /// every slot of found empty, an address no code starts at in each.
    fn nothing_found() -> Box<[Found]> {
        (0..FOUND).map(|_| Cell::new((u32::MAX, None))).collect()
    }

    /// whether the code came from an older 3dsrecomp than the one this
    /// build has, so that recompiling the game again makes it better.
    pub fn outdated(&self) -> bool {
        self.code.generation() < recomp_abi::GENERATION
    }

    /// whether the code can tell the functions whose code changed in
    /// memory, which it can from generation 2 on.
    pub fn checks_itself(&self) -> bool {
        self.code.origins(0).is_some()
    }

    /// how many instructions the code has handed to the interpreter.
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks.get()
    }

    /// how many functions and modules there is code for.
    pub fn describe(&self) -> String {
        self.code.describe()
    }

    /// the code that can run from address, bit 0 set for Thumb, in the
    /// executable or in a module that is loaded.
    fn lookup(&self, address: u32) -> Option<Code> {
        let slot = &self.found[slot(address)];
        match slot.get() {
            (seen, code) if seen == address => code,
            _ => {
                let code = self.code.lookup(address);
                slot.set((address, code));
                code
            }
        }
    }

    pub fn has_code(&self, address: u32) -> bool {
        self.lookup(address).is_some()
    }

    /// marks the executable's functions whose code in memory is not what
    /// they were recompiled from, which a mod or another version of the
    /// game does, so that the interpreter runs them. how many there are.
    pub fn check(&mut self, memory: &mut Memory) -> usize {
        let stale = self.code.check(0, 0, |address, bytes| memory.read_bytes(address, bytes));
        self.found = Library::nothing_found();
        stale
    }

    /// tells the code of the module called name where the title loaded it,
    /// zero when it unloads it, and checks the module's code once it is in
    /// memory.
    pub fn place(&mut self, name: &str, base: u32, memory: &mut Memory) {
        let Some(index) = self.code.module_index(name) else { return };
        self.code.place(index, base);
        // what was found before may have moved or gone
        self.found = Library::nothing_found();
        if base == 0 {
            return;
        }
        log::info!("recompiled code for {name} runs at 0x{base:08X}");
        let stale = self.code.check(index + 1, base, |address, bytes| memory.read_bytes(address, bytes));
        if stale > 0 {
            log::warn!("{stale} functions of {name} changed since it was recompiled, they run in the interpreter");
        }
    }

    /// runs recompiled code from the cpu's pc for about budget instructions,
    /// returning how many ran and why it stopped. svc carries out a system
    /// call made in a call from C into the guest, on the cpu as it is, and
    /// says whether the thread can go on.
    pub fn run(&self, cpu: &mut Cpu, memory: &mut Memory, budget: u64, svc: &mut dyn FnMut(u32) -> bool) -> (u64, Stop) {
        let cycles = cpu.cycles;
        let (read_pages, write_pages) = memory.page_tables();
        let mut ctx = Context {
            r: [0; 16],
            n: 0,
            z: 0,
            c: 0,
            v: 0,
            q: 0,
            thumb: 0,
            ge: 0,
            exclusive: 0,
            budget: budget.min(i32::MAX as u64) as i32,
            exit: 0,
            svc: 0,
            depth: 0,
            exclusive_address: 0,
            tls: 0,
            read_pages,
            write_pages,
            vfp: std::ptr::null_mut(),
            fpscr: std::ptr::null_mut(),
            host: &HOST,
            user: std::ptr::null_mut(),
        };
        store(cpu, &mut ctx);
        let mut machine = Machine { cpu, memory, library: self, pending: None, svc };
        // the code works on the VFP registers where they are, so the
        // interpreter sees its changes without copying them around
        ctx.vfp = machine.cpu.vfp.regs.as_mut_ptr();
        ctx.fpscr = &mut machine.cpu.vfp.fpscr;
        ctx.user = &mut machine as *mut Machine as *mut c_void;

        let stop = loop {
            if ctx.budget <= 0 {
                break Stop::Left;
            }
            let Some(code) = self.lookup(ctx.r[15] | ctx.thumb as u32) else {
                break Stop::Left;
            };
            ctx.exit = 0;
            ctx.depth = 0;
            // SAFETY: the code came from the library and ctx is set up the
            // way it expects, with user pointing at machine
            unsafe { code(&mut ctx) };
            if let Some(exit) = machine.pending.take() {
                break Stop::Exit(exit);
            }
            match ctx.exit {
                EXIT_SVC => break Stop::Svc(ctx.svc),
                EXIT_BUDGET => break Stop::Left,
                _ => {}
            }
        };

        load(&ctx, machine.cpu);
        let ran = budget.saturating_sub(ctx.budget.max(0) as u64);
        // the interpreter counted the instructions it ran for the code, which
        // the budget already has
        machine.cpu.cycles = cycles + ran;
        (ran, stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_and_thumb_code_use_every_slot() {
        let arm: std::collections::BTreeSet<usize> = (0..FOUND as u32).map(|i| slot(0x0010_0000 + 4 * i)).collect();
        let thumb: std::collections::BTreeSet<usize> = (0..FOUND as u32).map(|i| slot(0x0010_0001 + 2 * i)).collect();
        assert!(arm.len() > FOUND * 3 / 4, "{} of {FOUND}", arm.len());
        assert!(thumb.len() > FOUND * 3 / 4, "{} of {FOUND}", thumb.len());
    }
}
