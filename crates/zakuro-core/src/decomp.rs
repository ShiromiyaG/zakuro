//! code written apart from the title and linked at an address of its own,
//! see 3dsrecomp's decomp module, which reads the same two files: it is
//! mapped there and each function it replaces starts with a jump to it.

use std::path::Path;

/// the name the recompiled code of it is known by, as a module's is.
pub const NAME: &str = "decomp";

pub struct Decomp {
    pub base: u32,
    pub code: Vec<u8>,
    /// the title's functions replaced and the code that takes over, the
    /// first odd for Thumb.
    pub hooks: Vec<(u32, u32)>,
    /// what each replaced function returns, by its address: void, int,
    /// long, float or double, the last word of its hook line.
    pub returns: std::collections::BTreeMap<u32, String>,
    /// functions of modules (CROs) replaced, hooked as each loads.
    pub module_hooks: Vec<ModuleHook>,
    /// addresses in modules the decomp uses: the module, the offset (odd
    /// for Thumb), the word of the decomp's the address goes in, and the
    /// segment the offset is from (none: from where the module loads).
    pub module_imports: Vec<(String, u32, u32, Option<usize>)>,
}

/// a function of a module the decomp replaces: the module's name, its
/// offset from where the module is loaded (odd for Thumb), the code that
/// takes over, and what it returns.
#[derive(Clone)]
pub struct ModuleHook {
    pub module: String,
    pub offset: u32,
    pub new: u32,
    pub kind: Option<String>,
}

impl Decomp {
    /// reads dir/decomp.bin and dir/decomp.txt (`base` and `hook` lines).
    pub fn read(dir: &Path) -> Result<Decomp, String> {
        let read = |name: &str| std::fs::read(dir.join(name)).map_err(|error| format!("could not read {}, {error}", dir.join(name).display()));
        let code = read("decomp.bin")?;
        let text = String::from_utf8_lossy(&read("decomp.txt")?).into_owned();
        let mut decomp = Decomp { base: 0, code, hooks: Vec::new(), returns: Default::default(), module_hooks: Vec::new(), module_imports: Vec::new() };
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let words: Vec<&str> = line.split_whitespace().collect();
            let number = |i: usize| {
                let word = words.get(i).ok_or_else(|| format!("decomp.txt: '{line}' is short"))?;
                u32::from_str_radix(word.trim_start_matches("0x"), 16).map_err(|_| format!("decomp.txt: '{word}' is not an address"))
            };
            match words[0] {
                "base" => decomp.base = number(1)?,
                "hook" => {
                    decomp.hooks.push((number(1)?, number(2)?));
                    if let Some(kind) = words.get(3) {
                        decomp.returns.insert(number(1)?, kind.to_string());
                    }
                }
                "module_import" => decomp.module_imports.push((
                    words.get(1).ok_or_else(|| format!("decomp.txt: '{line}' is short"))?.trim_end_matches(".cro").to_owned(),
                    number(2)?,
                    number(3)?,
                    words.get(4).and_then(|word| word.strip_prefix("seg")).and_then(|n| n.parse().ok()),
                )),
                "module_hook" => decomp.module_hooks.push(ModuleHook {
                    module: words.get(1).ok_or_else(|| format!("decomp.txt: '{line}' is short"))?.trim_end_matches(".cro").to_owned(),
                    offset: number(2)?,
                    new: number(3)?,
                    kind: words.get(4).map(|kind| kind.to_string()),
                }),
                _ => {}
            }
        }
        if decomp.base == 0 || decomp.base % 0x1000 != 0 {
            return Err("decomp.txt needs a base on a page".to_owned());
        }
        Ok(decomp)
    }

    /// the jump over a function's first instructions: `ldr pc, [pc, #-4]`
    /// and the address, from Thumb a `bx pc` first, which lands on the next
    /// whole word.
    pub fn stub(old: u32, new: u32) -> Vec<u8> {
        const LDR_PC: u32 = 0xE51F_F004;
        let mut bytes = Vec::new();
        if old & 1 != 0 {
            bytes.extend(0x4778u16.to_le_bytes());
            if old & 2 == 0 {
                bytes.extend(0x46C0u16.to_le_bytes());
            }
        }
        bytes.extend(LDR_PC.to_le_bytes());
        bytes.extend((new & !1).to_le_bytes());
        bytes
    }

    /// writes the jumps into the text segment's bytes, which start at
    /// address text.
    pub fn patch(&self, text: u32, bytes: &mut [u8]) {
        for &(old, new) in &self.hooks {
            let stub = Decomp::stub(old, new);
            let at = (old & !1).wrapping_sub(text) as usize;
            if let Some(slot) = bytes.get_mut(at..at + stub.len()) {
                slot.copy_from_slice(&stub);
            } else {
                log::warn!("decomp: 0x{old:08X} is not in the code, left alone");
            }
        }
    }
}

/// a module was loaded at base: its functions the decomp replaces get the
/// jump, or, when checking, are checked as the title's own are.
pub fn hook_module(system: &mut crate::System, name: &str, base: u32) {
    let name = name.trim_end_matches(".cro");
    let segments = system.cro.modules.iter().find(|m| m.base == base).map(|m| m.segments.clone()).unwrap_or_default();
    for (_, offset, slot, segment) in system.decomp_module_imports.iter().filter(|(module, ..)| module == name).cloned().collect::<Vec<_>>() {
        system.memory.write_bytes(slot, &module_address(base, &segments, offset, segment).to_le_bytes());
    }
    let hooks: Vec<ModuleHook> = system.decomp_module_hooks.iter().filter(|hook| hook.module == name).cloned().collect();
    for hook in hooks {
        let old = base + hook.offset;
        log::info!("decomp: {}.cro+0x{:08X} hooked at 0x{old:08X}", hook.module, hook.offset);
        match system.decomp_check.as_mut() {
            Some(check) => {
                check.hooks.push((old, hook.new));
                if let Some(kind) = &hook.kind {
                    check.returns.insert(old, kind.clone());
                }
            }
            None => system.memory.write_bytes(old & !1, &Decomp::stub(old, hook.new)),
        }
    }
}

/// a module at base, size bytes, is unloaded: its functions are no longer
/// checked where it was.
pub fn unhook_module(system: &mut crate::System, base: u32, size: u32) {
    // a call into it from the decomp now goes nowhere, as it should
    let name = system.cro.modules.iter().find(|m| m.base == base).map(|m| m.name.trim_end_matches(".cro").to_owned());
    for (_, _, slot, _) in system.decomp_module_imports.iter().filter(|(module, ..)| Some(module) == name.as_ref()).cloned().collect::<Vec<_>>() {
        system.memory.write_bytes(slot, &0u32.to_le_bytes());
    }
    if let Some(check) = system.decomp_check.as_mut() {
        check.hooks.retain(|(old, _)| !(base..base + size).contains(&(old & !1)));
    }
}

/// a state was loaded: the decomp as built now goes where the state has
/// its segment, and the jumps (or the checks) go where they belong, in the
/// text and in the modules the state has loaded. the state has to have
/// been saved with the decomp checked (--decomp-check): its segment mapped,
/// and no jumps of an older build in the title's code.
pub fn after_state(system: &mut crate::System) -> Result<(), String> {
    let Some(dir) = system.config.decomp.clone() else { return Ok(()) };
    let decomp = Decomp::read(&dir)?;
    if !system.memory.is_executable(decomp.base) {
        return Err("the state was saved without the decomp: save it again with --decomp <dir> --decomp-check".into());
    }
    system.memory.write_bytes(decomp.base, &decomp.code);
    if system.decomp_check.is_none() {
        for &(old, new) in &decomp.hooks {
            system.memory.write_bytes(old & !1, &Decomp::stub(old, new));
        }
    }
    let modules: Vec<(String, u32, u32)> = system.cro.modules.iter().map(|m| (m.name.clone(), m.base, m.size)).collect();
    for (name, base, size) in modules {
        unhook_module(system, base, size);
        hook_module(system, &name, base);
    }
    Ok(())
}

/// an address in a module loaded at base with those segments: the offset
/// from the module's start, or from the start of a segment.
pub fn module_address(base: u32, segments: &[u32], offset: u32, segment: Option<usize>) -> u32 {
    segment.map_or(base, |n| segments.get(n).copied().unwrap_or(0)).wrapping_add(offset)
}
