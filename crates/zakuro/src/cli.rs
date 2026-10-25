//! command line parsing.

use zakuro_gpu::RendererKind;

#[derive(Debug, Clone)]
pub struct Options {
    /// the game to start, none to open the library.
    pub rom: Option<String>,
    /// the settings' choice when not given.
    pub renderer: Option<RendererKind>,
    /// window scale relative to the console's 400x480 combined screens, the
    /// settings' when not given.
    pub scale: Option<u32>,
    /// run without a window for this many frames, then report.
    pub headless: Option<u64>,
    pub profile: bool,
    pub new3ds: bool,
    /// skip loading a ROM entirely and just paint both screens solid colors.
    pub test_pattern: bool,
    /// a library of recompiled code for the title, or a directory of them.
    pub recompiled: Option<String>,
    /// draw on the host GPU rather than in software, the settings' choice
    /// when not given.
    pub hardware_rasterizer: Option<bool>,
    /// where saves live, instead of the usual place.
    pub data: Option<String>,
    /// the settings file, instead of the usual one.
    pub settings: Option<String>,
    /// where to keep where the game spends its time, for 3dsrecomp's --hot.
    pub heat: Option<String>,
    /// with --recompile, heat to build only the hottest functions by.
    pub hot: Option<String>,
    /// with --recompile, C files of functions written by hand, or a folder
    /// of them, see 3dsrecomp's docs/overrides.md.
    pub overrides: Option<String>,
    /// with --headless, a folder to save both screens to every
    /// screenshot_every frames.
    pub screenshots: Option<String>,
    pub screenshot_every: u64,
    /// interpret everything, whatever recompiled code there is.
    pub interpreter: bool,
    /// play no sound.
    pub mute: bool,
    /// recompile the game with its update and mods, then quit.
    pub recompile: bool,
    /// the game's update, in place of one found beside it.
    pub update: Option<String>,
    /// with --headless, a saved state to go on from, at its frame.
    pub load_state: Option<String>,
    /// with --headless, save the state after this many frames, to a file.
    pub save_state: Option<(u64, String)>,
    /// with --headless, write a hash of each writable page every this many
    /// frames, to a file, for comparing two runs.
    pub memory_hashes: Option<(u64, String)>,
    /// with --headless, write the writable pages after this many frames.
    pub dump_memory: Option<(u64, String)>,
    /// a folder with code written apart from the game (3dsrecomp's
    /// --decomp), run in place of the functions it replaces.
    pub decomp: Option<String>,
    /// run the title's code and the decomp's at each replaced function and
    /// compare them, in the interpreter.
    pub decomp_check: bool,
    /// with decomp_check, keep calls as samples in this file.
    pub decomp_capture: Option<String>,
    /// run the title's code and the decomp's again on the samples in this
    /// file, with no title running, and stop.
    pub decomp_replay: Option<String>,
    /// with decomp_replay, runs on changed samples for each function.
    pub decomp_fuzz: u32,
    /// run the title's code and the decomp's on every input a spec file
    /// gives, with the title's code.bin (decomp_code), and stop.
    pub decomp_exhaustive: Option<String>,
    pub decomp_code: Option<String>,
    /// with --load-state and --decomp-check, run every replaced function
    /// this many times on made-up inputs, the title's code and ours, with
    /// nothing of the title running, and stop.
    pub decomp_synth: Option<u32>,
    /// the title's romfs as files, for the modules decomp_synth loads.
    pub decomp_romfs: Option<String>,
}

const USAGE: &str = "\
zakuro - a high-level-emulation Nintendo 3DS emulator

usage: zakuro [rom.3ds|.cxi] [options]

Without a game it opens the library, the games in a folder you pick. The
options below override the settings for this run.

options:
  --renderer <vulkan|gl|software>  presentation backend (default: vulkan,
                                   or gl where Vulkan does not start)
  --rasterizer <hardware|software> where the 3D is drawn (default: hardware,
                                   the host GPU through Vulkan, or software
                                   where that does not start)
  --scale <n>                      window scale factor (default: 2)
  (Esc opens the menu over a game, F1 pauses, F11 goes fullscreen)
  --headless <frames>              run without a window and print a report
  --new3ds                         emulate a New 3DS
  --profile                        collect a sampling profile and print it
  --recompiled <path>              run code 3dsrecomp built for the title, a
                                   library or a directory holding <title id>.so
                                   (.dll on Windows, default: the one 3dsrecomp
                                   build installed for it, in the system's
                                   place for data, ~/.local/share/3dsrecomp or
                                   %APPDATA%\\3dsrecomp)
  --interpreter                    interpret everything, even with recompiled
                                   code around
  --mute                           play no sound
  --recompile                      recompile the game with its update and
                                   mods, as Recompile in the library does,
                                   then quit
  --update <cia>                   the game's update, decrypted, in place of
                                   one found beside it
  --load-state <file>              with --headless, go on from a saved state
                                   (a recording goes on from its frame)
  --save-state <frames>:<file>     with --headless, save the state after
                                   that many frames
  --memory-hashes <every>:<file>   with --headless, write a hash of each
                                   writable page every that many frames
  --dump-memory <frames>:<file>    with --headless, write each writable page,
                                   its address and its 4 KiB, after that many
                                   frames
  --decomp <dir>                   code written apart from the game, decomp.bin
                                   and decomp.txt, that replaces functions of
                                   it (with --recompile too)
  --decomp-check                   with --decomp, run both at each replaced
                                   function and compare (interpreter only)
  --decomp-capture <file>          --decomp-check, and keep calls as samples
                                   in file, to run again with
  --decomp-replay <file>           with --decomp, run both again on the
                                   samples in file, no game needed, and
                                   tell how they fared and what they cover
  --decomp-synth <count>           with --load-state and --decomp-check, run
                                   each replaced function so many times on
                                   made-up inputs, the title's code and ours,
                                   and stop (no title running); modules not
                                   loaded come from --decomp-romfs <dir>
  --decomp-fuzz <count>            with --decomp-replay, as many runs for each
                                   function on samples with an argument or a
                                   word of their data changed
  --decomp-exhaustive <spec>       with --decomp and --decomp-code <code.bin>,
                                   run both on every input the spec gives
                                   (lines `0xADDRESS r0=0-13 r1=0,1`)
  --data <dir>                     where saves live (default: the system's
                                   place for data, ~/.local/share/zakuro or
                                   %APPDATA%\\zakuro)
  --settings <file>                keep the settings in this file instead of
                                   the usual one, for a program running
                                   Zakuro with settings of its own
  --heat <file>                    add where the game spends its time to this
                                   file, for 3dsrecomp build --hot (all of it
                                   with --interpreter)
  --hot <file>                     with --recompile, build only the functions
                                   that file's heat says the game spends
                                   nearly all its time in, at full speed
  --overrides <file or dir>        with --recompile, functions written by hand
                                   in C that replace the game's
  --screenshots <dir>              with --headless, save both screens as PNGs
                                   there every --screenshot-every frames
                                   (default 600); needs --rasterizer software
  -h, --help                       show this message

The system font cannot be generated: put a dump at sysdata/shared_font.bin, in
the working directory or where saves live, for titles that render text with it.
";

pub fn parse() -> Result<Options, String> {
    let mut rom = None;
    let mut options = Options {
        rom: None,
        renderer: None,
        scale: None,
        headless: None,
        profile: false,
        new3ds: false,
        test_pattern: false,
        recompiled: None,
        hardware_rasterizer: None,
        data: None,
        interpreter: false,
        mute: false,
        recompile: false,
        update: None,
        load_state: None,
        save_state: None,
        memory_hashes: None,
        dump_memory: None,
        decomp: None,
        decomp_check: false,
        decomp_capture: None,
        decomp_replay: None,
        decomp_fuzz: 0,
        decomp_exhaustive: None,
        decomp_code: None,
        decomp_synth: None,
        decomp_romfs: None,
        settings: None,
        heat: None,
        hot: None,
        overrides: None,
        screenshots: None,
        screenshot_every: 600,
    };

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--renderer" => {
                let value = args.next().ok_or("--renderer needs a value")?;
                options.renderer = Some(
                    RendererKind::parse(&value).ok_or_else(|| format!("unknown renderer '{value}'"))?,
                );
            }
            "--scale" => {
                let value = args.next().ok_or("--scale needs a value")?;
                options.scale = Some(value.parse().map_err(|_| format!("'{value}' is not a scale"))?);
            }
            "--headless" => {
                let value = args.next().ok_or("--headless needs a frame count")?;
                options.headless = Some(
                    value
                        .parse()
                        .map_err(|_| format!("'{value}' is not a frame count"))?,
                );
            }
            "--new3ds" => options.new3ds = true,
            "--rasterizer" => {
                let value = args.next().ok_or("--rasterizer needs a value")?;
                options.hardware_rasterizer = Some(match value.as_str() {
                    "hardware" | "vulkan" | "gpu" => true,
                    "software" | "cpu" => false,
                    other => return Err(format!("unknown rasterizer '{other}'")),
                });
            }
            "--recompiled" => {
                options.recompiled = Some(args.next().ok_or("--recompiled needs a path")?);
            }
            "--data" => {
                options.data = Some(args.next().ok_or("--data needs a directory")?);
            }
            "--settings" => {
                options.settings = Some(args.next().ok_or("--settings needs a file")?);
            }
            "--heat" => {
                options.heat = Some(args.next().ok_or("--heat needs a file")?);
            }
            "--hot" => {
                options.hot = Some(args.next().ok_or("--hot needs a file")?);
            }
            "--overrides" => {
                options.overrides = Some(args.next().ok_or("--overrides needs a file or a folder")?);
            }
            "--screenshots" => {
                options.screenshots = Some(args.next().ok_or("--screenshots needs a folder")?);
            }
            "--screenshot-every" => {
                let value = args.next().ok_or("--screenshot-every needs a frame count")?;
                options.screenshot_every = value.parse().ok().filter(|&n| n > 0).ok_or(format!("'{value}' is not a frame count"))?;
            }
            "--interpreter" => options.interpreter = true,
            "--mute" => options.mute = true,
            "--recompile" => options.recompile = true,
            "--update" => {
                options.update = Some(args.next().ok_or("--update needs a path")?);
            }
            "--load-state" => {
                options.load_state = Some(args.next().ok_or("--load-state needs a path")?);
            }
            "--save-state" => {
                let value = args.next().ok_or("--save-state needs frames:file")?;
                let (frames, file) = value.split_once(':').ok_or("--save-state is frames:file")?;
                let frames = frames.parse().map_err(|_| format!("'{frames}' is not a frame count"))?;
                options.save_state = Some((frames, file.to_owned()));
            }
            "--memory-hashes" => {
                let value = args.next().ok_or("--memory-hashes needs every:file")?;
                let (every, file) = value.split_once(':').ok_or("--memory-hashes is every:file")?;
                let every = every.parse().ok().filter(|&every| every > 0).ok_or_else(|| format!("'{every}' is not a frame count"))?;
                options.memory_hashes = Some((every, file.to_owned()));
            }
            "--dump-memory" => {
                let value = args.next().ok_or("--dump-memory needs frames:file")?;
                let (frames, file) = value.split_once(':').ok_or("--dump-memory is frames:file")?;
                let frames = frames.parse().map_err(|_| format!("'{frames}' is not a frame count"))?;
                options.dump_memory = Some((frames, file.to_owned()));
            }
            "--decomp" => {
                options.decomp = Some(args.next().ok_or("--decomp needs a folder")?);
            }
            "--decomp-check" => {
                // the check steps both runs one instruction at a time
                options.decomp_check = true;
                options.interpreter = true;
            }
            "--decomp-capture" => {
                options.decomp_capture = Some(args.next().ok_or("--decomp-capture needs a file")?.to_owned());
                options.decomp_check = true;
                options.interpreter = true;
            }
            "--decomp-exhaustive" => {
                options.decomp_exhaustive = Some(args.next().ok_or("--decomp-exhaustive needs a spec file")?.to_owned());
            }
            "--decomp-code" => {
                options.decomp_code = Some(args.next().ok_or("--decomp-code needs the title's code.bin")?.to_owned());
            }
            "--decomp-fuzz" => {
                let count = args.next().ok_or("--decomp-fuzz needs a count")?;
                options.decomp_fuzz = count.parse().map_err(|_| format!("--decomp-fuzz: '{count}' is not a count"))?;
            }
            "--decomp-synth" => {
                let count = args.next().ok_or("--decomp-synth needs a count")?;
                options.decomp_synth = Some(count.parse().map_err(|_| format!("--decomp-synth: '{count}' is not a count"))?);
            }
            "--decomp-romfs" => {
                options.decomp_romfs = Some(args.next().ok_or("--decomp-romfs needs a folder")?.to_owned());
            }
            "--decomp-replay" => {
                options.decomp_replay = Some(args.next().ok_or("--decomp-replay needs a file")?.to_owned());
            }
            "--profile" => options.profile = true,
            "--test-pattern" => options.test_pattern = true,
            other if other.starts_with('-') => {
                return Err(format!("unknown option '{other}'"));
            }
            path => rom = Some(path.to_owned()),
        }
    }

    options.rom = rom;
    if options.headless.is_some() && options.rom.is_none() && !options.test_pattern {
        print!("{USAGE}");
        return Err("--headless needs a game".to_owned());
    }
    if options.recompile && options.rom.is_none() {
        print!("{USAGE}");
        return Err("--recompile needs a game".to_owned());
    }
    Ok(options)
}
