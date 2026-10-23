//! where a title spends its time, by the place of the code: the executable's
//! addresses, and a module's offsets from where it starts, which are the same
//! every run wherever the module is loaded. 3dsrecomp build --hot compiles
//! first the functions that cover most of it, so that a game is playable
//! after a short build while the rest is built in the background.
//!
//! a profile taken with --interpreter sees every function that runs; with a
//! library, only the places the library is entered from the host.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::cro::Module;

/// the program a place is in: the executable, or a module by its name's
/// index in names.
const EXECUTABLE: u32 = u32::MAX;

pub struct Heat {
    path: PathBuf,
    names: Vec<String>,
    /// samples by program and address, odd for Thumb.
    counts: HashMap<(u32, u32), u64>,
}

impl Heat {
    /// the heat kept in path, going on from what an earlier run wrote there.
    pub fn new(path: PathBuf) -> Heat {
        let mut heat = Heat { path, names: Vec::new(), counts: HashMap::new() };
        let text = std::fs::read_to_string(&heat.path).unwrap_or_default();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [program, at, hits] = fields[..] else { continue };
            let (Ok(at), Ok(hits)) = (u32::from_str_radix(at, 16), hits.parse::<u64>()) else { continue };
            let program = if program == "executable" { EXECUTABLE } else { heat.name(program) };
            *heat.counts.entry((program, at)).or_insert(0) += hits;
        }
        heat
    }

    fn name(&mut self, name: &str) -> u32 {
        match self.names.iter().position(|known| known == name) {
            Some(index) => index as u32,
            None => {
                self.names.push(name.to_owned());
                self.names.len() as u32 - 1
            }
        }
    }

    /// the code at pc ran, in the executable or in one of modules.
    pub fn sample(&mut self, pc: u32, thumb: bool, modules: &[Module]) {
        let at = (pc & !1) | thumb as u32;
        let key = match modules.iter().find(|module| (module.base..module.base.saturating_add(module.size)).contains(&pc)) {
            Some(module) => {
                let program = self.name(&module.name);
                (program, at - module.base)
            }
            None => (EXECUTABLE, at),
        };
        *self.counts.entry(key).or_insert(0) += 1;
    }

    pub fn save(&self) {
        let mut lines: Vec<(&str, u32, u64)> = self
            .counts
            .iter()
            .map(|(&(program, at), &hits)| {
                let name = if program == EXECUTABLE { "executable" } else { &self.names[program as usize] };
                (name, at, hits)
            })
            .collect();
        lines.sort_unstable();
        let mut text = String::from("# where the title ran: program, address or offset in it (odd for Thumb), samples\n");
        for (name, at, hits) in lines {
            text.push_str(&format!("{name} {at:08X} {hits}\n"));
        }
        if let Err(error) = std::fs::write(&self.path, text) {
            log::warn!("could not write the heat to {}, {error}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(name: &str, base: u32, size: u32) -> Module {
        Module {
            name: name.to_owned(),
            base,
            size,
            exports: HashMap::new(),
            indexed_exports: Vec::new(),
            segments: Vec::new(),
            auto_link: false,
        }
    }

    /// a module's places are its offsets, the same wherever it is loaded,
    /// and a second run adds to what the first saved.
    #[test]
    fn modules_count_by_offset_and_runs_add_up() {
        let path = std::env::temp_dir().join(format!("zakuro-heat-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut heat = Heat::new(path.clone());
        heat.sample(0x0010_0040, false, &[]);
        heat.sample(0x006D_F101, true, &[module("Battle", 0x006D_F000, 0x1000)]);
        heat.sample(0x0080_0101, true, &[module("Battle", 0x0080_0000, 0x1000)]);
        heat.save();
        let mut again = Heat::new(path.clone());
        again.sample(0x0010_0040, false, &[]);
        again.save();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("executable 00100040 2\n"), "{text}");
        assert!(text.contains("Battle 00000101 2\n"), "{text}");
        std::fs::remove_file(path).unwrap();
    }
}
