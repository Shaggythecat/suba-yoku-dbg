use sq_common::{*, dbg::SqLocalVarWithLvl};
use std::{
    sync::atomic,
    fs::File,
};
use clap::{Subcommand, Command, FromArgMatches};
use anyhow::{Result};
use serde::{Serialize, Deserialize};
use crate::hooks;


const DEFAULT_STATE_FILENAME: &str = "state.json";

#[derive(clap::ValueEnum, Copy, Clone, Debug)]
enum BoolVal {
    True,
    False,
}

impl From<BoolVal> for bool {
    fn from(value: BoolVal) -> Self {
        match value {
            BoolVal::True => true,
            BoolVal::False => false,
        }
    }
}

#[derive(Subcommand, Debug)]
enum SetCommands {
    /// Activate or deactivate printf hook of sqvm
    PrintfHook {
        #[arg(value_enum)]
        active: BoolVal,
    }
}

#[derive(Subcommand, Debug, Clone, Copy)]
enum BufferCommands {
    /// Create new empty buffer
    #[clap(visible_alias = "n")]
    New,

    /// Delete buffer by number
    #[clap(visible_alias = "d", visible_alias = "del")]
    Delete {
        /// Number of buffer
        num: u32
    },

    /// Edit existing buffer by number
    #[clap(visible_alias = "e")]
    Edit {
        /// Number of buffer
        num: u32
    },

    /// Print buffer by number 
    #[clap(visible_alias = "p")]
    Print {
        /// Number of buffer
        num: u32
    },

    /// List available buffers
    #[clap(visible_alias = "ls")]
    List,
}

/// CLI Frontend commands
#[derive(Subcommand, Debug, Default)]
enum Commands {
    /// Step one debug callback call
    #[clap(visible_alias = "s")]
    Step,

    /// Continue execution
    #[clap(visible_alias = "c")]
    Continue,

    /// Print call backtrace
    #[clap(visible_alias = "bt")]
    Backtrace,

    /// Print local variables list at specified call stack level
    #[clap(visible_alias = "loc")]
    Locals {
        /// Level of call stack. Can be found using backtrace.
        /// If not specified, print all
        level: Option<u32>,
    },

    /// Print value of local variable
    #[clap(visible_alias = "x")]
    Examine {
        /// Dot-separated path to target variable. 
        /// 
        /// e.g. `this.tableX.instanceY.target` or `this.arrayX.42`.
        /// 
        /// Also you can prefix path with call stack level like this: `1.this.varX`.
        target: String,

        /// Specify level of call stack.
        ///
        /// If not specified, print first found valid path.
        level: Option<SqUnsignedInteger>,

        /// Depth of eager containers (table, array, etc.) expansion.
        /// 
        /// - 0 - do not expand.
        /// 
        /// - 1 - expand this container.
        /// 
        /// - 2 - expand this container and all children
        /// 
        /// - 3.. - and so on
        #[clap(short, long, default_value = "1")]  
        depth: u32
    },

    /// Add new breakpoint
    #[clap(visible_alias = "b", visible_alias = "break")]
    BreakpointAdd {
        /// Breakpoint specification.
        ///
        /// Must be in format [file:<src>]:[function]:[line].
        ///
        /// At least 1 condition must be specified.
        spec: String
    },

    /// Enable breakpoint. If number not specified, enable all
    #[clap(visible_alias = "be")]
    BreakpointEnable {
        /// Breakpoint number
        num: Option<u32>
    },

    /// Disable breakpoint. If number not specified, disable all
    #[clap(visible_alias = "bd")]
    BreakpointDisable {
        /// Breakpoint number
        num: Option<u32>
    },

    /// Clear breakpoint. If number not specified, clear all
    #[clap(visible_alias = "bc")]
    BreakpointClear {
        /// Breakpoint number
        num: Option<u32>
    },

    /// List all breakpoints
    #[clap(visible_alias = "bl")]
    BreakpointList,

    /// Compile and run arbitrary squirrel code
    ///
    /// Local variables to be captured in compiled closure may be specified
    /// in list on first script line like this:
    ///
    ///     |3.this, 1.capture_local1, 2.capture_local2, ...|
    ///
    /// where local variable name is prefixed with call stack level.
    /// Note that `lvl.this` will be renamed to this_lvl, e.g. this_3
    #[clap(visible_alias = "eval")]
    Evaluate {
        /// If specified, enable debugging of compiled script
        #[clap(visible_alias = "dbg", long)]
        debug: bool, 

        /// Choose script buffer to evaluate. If not specified, new buffer will be created.
        buffer: Option<u32>,
    },

    /// Add, remove, edit and view script buffers
    #[clap(visible_alias = "buf")]
    #[command(subcommand)]
    Buffer(BufferCommands),

    /// Continue execution, but print every debug event.
    ///
    /// Warning: due to heavy use of stdout, it may be hard to send stop command to debugger,
    /// use breakpoints instead
    #[clap(visible_alias = "t")]
    Trace,

    /// Set values of different debugging variables
    #[command(subcommand)]
    Set(SetCommands),

    /// Save breakpoints and buffers.
    Save {
        /// File to save state.
        /// If not specified, default file will be used
        file: Option<String>,
    },
    
    /// Load breakpoints and buffers.
    Load {
        /// File to load state from.
        /// If not specified, default file will be used
        file: Option<String>,
    },

    /// Stub command for no-operation, does nothing
    #[default]
    Nop,

    /// Exit process
    Exit,
}

#[derive(Clone, Serialize, Deserialize)]
 struct SavedState {
    buffers: ScriptBuffers,
    breakpoints: dbg::BreakpointStore,
}

/// CLI Frontend for SQ debugger
pub struct DebuggerFrontend {
    last_cmd: Commands,
    buffers: ScriptBuffers,
    during_eval: bool,
}

/// Private methods
impl DebuggerFrontend {
    fn print_backtrace(bt: dbg::SqBacktrace) {
        println!("Backtrace:");
        for (lvl, info) in bt.into_iter().enumerate() {
            println!("{:03}: {info}", lvl + 1);
        }
    }

    /// Print locals in form
    /// ```rs
    /// Level X locals:
    /// loc: type [= val]
    /// 
    /// Level Y locals:
    /// ...
    /// ```
    fn print_locals(locals: Vec<SqLocalVarWithLvl>) {
        let mut curr_lvl = 0; // Non-existent
        for SqLocalVarWithLvl { var: SqLocalVar { name, val }, lvl } in locals {
            if lvl != curr_lvl {
                println!("\nLevel {lvl} locals:");
                curr_lvl = lvl;
            }

            print!("{name}: {:?}", val.get_type());

            match val {
                DynSqVar::Integer(i) => println!(" = {i}"),
                DynSqVar::Float(f) => println!(" = {f}"),
                DynSqVar::Bool(b) => println!(" = {b}"),
                DynSqVar::String(s) => println!(" = \"{s}\""),
                _ => println!(),
            }
        }
    }

    /// Get CLI parser
    fn cli() -> Command {
        // strip out usage
        const PARSER_TEMPLATE: &str = "\
            {all-args}
        ";

        Commands::augment_subcommands(
            Command::new("repl")
                .multicall(true)
                .arg_required_else_help(false)
                .subcommand_required(true)
                .subcommand_value_name("Command")
                .subcommand_help_heading("Commands")
                .help_template(PARSER_TEMPLATE)
        )
    }

    /// Set debugger variable
    fn set_var(var: &SetCommands) {
        match var {
            SetCommands::PrintfHook { active }
                => hooks::PRINTF_HOOK_ACTIVE.store((*active).into(), atomic::Ordering::Relaxed),
        }
    }

    /// Match dot-separated path in container recursively
    fn match_local_path<'a, I>(mut path: I, root: &DynSqVar) -> Option<&DynSqVar>
    where I: Iterator<Item = &'a str> + Clone {
        let Some(key) = path.next() else {
            return None;
        };

        let index: Option<SqInteger> = key.parse().ok();
        

        let child = match root {
            DynSqVar::Table(map)
            | DynSqVar::Class(map)
            | DynSqVar::Instance(SqInstance { this: map }) => {
                if let Some(idx) = index {
                    map.iter().find(|(k, _)| {
                        matches!(k, DynSqVar::Integer(i) if *i == idx)
                        || matches!(k, DynSqVar::String(s) if s == key)
                    })
                } else {
                    map.iter().find(|(k, _)| {
                        matches!(k, DynSqVar::String(s) if s == key)
                    })
                }.map(|(_, v)| v)
            },

            DynSqVar::Array(v) => 
            if let Some(idx @ 0..) = index {
                v.get(idx as usize)
            } else { None },

            _ => None,
        };

        // Peek if path has next segment
        match (child, path.clone().next()) {
            (Some(_), None) => child,
            (Some(next), Some(_)) => Self::match_local_path(path, next),
            _ => None
        }
    }

    // TODO: Make lazily evaluated containers
    // TODO: Make it possible to match keys with spaces for tables
    /// Pretty-print local variable, try to find local by it's dot-separated path
    fn examine(dbg: &dbg::SqDebugger, path: &str, mut level: Option<SqUnsignedInteger>, mut depth: u32) {
        let mut path_seg = path.split('.');

        let mut root_name = path_seg.next().unwrap();

        // Check if first path segment is call stack level
        if let Ok(lvl) = root_name.parse() { 
            level = Some(lvl);
            let Some(root) = path_seg.next() else {
                println!("Local path not specified, only call stack level");
                return;
            };
            root_name = root;
        };

        // Add minimal length needed to match path
        let seg_cnt = path_seg.clone().count();
        if seg_cnt > 0 {
            depth += seg_cnt as u32
        }

        match dbg.get_locals(level, depth) {
            Ok(locs) => {
                for SqLocalVarWithLvl { var: SqLocalVar { name, val }, ..} in locs {
                    if root_name == name {
                        // This is target
                        if path_seg.clone().next().is_none() {
                            println!("{name}: {typ:?} = {val}", typ = val.get_type());
                            return;
                        }
                        // Try to recursively find target in children
                        if let Some(target) = Self::match_local_path(path_seg.clone(), &val) {
                            println!("{path}: {typ:?} = {target}", typ = target.get_type());
                            return;
                        }
                    }
                }

                println!("failed to match path `{path}`");

            },
            Err(e) => {
                println!("failed to get locals: {e}");
            },
        }
    }

    /// Parse breakpoint specification 
    fn add_breakpoint(dbg: &dbg::SqDebugger, spec: &str) {
        let mut bp_proto = dbg::SqBreakpoint::new();
        let spec = spec.split(':').collect::<Vec<_>>();

        // Parse src file if present
        let spec = {
            let spec = &spec[..];
            if let ["file", src, ..] = spec[..] {
                bp_proto = bp_proto.src_file(src.to_string());
                &spec[2..]
            } else { spec }
        };

        // Parse remaining
        let bp = match spec {
            [func, line]
            if func.starts_with(|c: char| !c.is_numeric())
            && line.chars().all(|c| c.is_ascii_digit())
                => bp_proto.fn_name(func.to_string())
                    .line(line.parse().unwrap()),

            [line] if line.chars().all(|c| c.is_ascii_digit())
                => bp_proto.line(line.parse().unwrap()),

            [func] if func.starts_with(|c: char| !c.is_numeric())
                => bp_proto.fn_name(func.to_string()),

            [] => bp_proto,

            _ => {
                println!("Invalid breakpoint format");
                return;
            }
        };

        dbg.breakpoints().add(bp);
    }

    /// Create or edit buffer
    fn edit_buffer(prev: Option<&str>) -> Result<String> {
        match scrawl::editor::new()
            .editor("nvim")
            .extension(".nut")
            .contents(prev.unwrap_or_default())
            .open() 
        {
            Ok(s) => Ok(s),
            // Try to open default editor
            Err(_) => Ok(scrawl::with(prev.unwrap_or_default())?),
        }
    }

    /// Execute arbitrary script 
    fn eval_script(&mut self, dbg: &dbg::SqDebugger, debug: bool, buffer: Option<u32>) {
        if self.during_eval {
            println!("failed to evaluate: cannot evaluate during evaluation");
            return;
        }

        let script = match buffer {
            Some(num ) if self.buffers.get(num).is_some() => {
                self.buffers.get(num).unwrap()
            }, 
            _ => match Self::edit_buffer(None) {
                Ok(s) => {
                    let num = self.buffers.add(s);
                    self.buffers.get(num).unwrap()
                },
                Err(e) => {
                    println!("failed to open editor: {e}");
                    return;
                },
            },
        };

        // Parse list of captured local vars
        let (script, capture) = if let Some(mut line) = script.lines().next() { 'block: {
            line = line.trim();
            if !line.starts_with('|') || !line.ends_with('|') {
                // Return cloned script and no captured locals
                break 'block (script.clone(), vec![]);
            }

            let list = &line[1..line.len() - 1];
            let mut out = vec![];

            for spec in list.split(',') {
                let parts: Vec<_> = spec.trim().split('.').collect();
                match &parts[..] {
                    [lvl, name] if name.starts_with(|c: char| c.is_alphabetic())
                        => if let Ok(lvl) = lvl.parse() {
                        out.push((name.to_string(), lvl));
                    } else {
                        println!("invalid level specification: {lvl} ({spec})");
                        return;
                    }
                    _ => {
                        println!("invalid local var specification: {spec}");
                        return;
                    }
                }
            }

            // Return script without first line and vector with captured vars
            (script.lines().skip(1).collect(), out)
        }} else {
            println!("buffer is empty");
            return;
        };

        self.during_eval = true;

        let eval_res = |res| match res {
            Ok(res) => println!("evaluation result: {res}"),
            Err(e) => println!("failed to evaluate: {e}"),  
        };

        if !debug {
            eval_res(dbg.execute(script, capture))
        }
        else { match dbg.execute_debug(script, capture) {
            // Spawn thread that will wait for eval result
            Ok(fut) => { std::thread::spawn(move || eval_res(fut())); },
            Err(e) => println!("failed to evaluate: {e}"),
        }}


        self.during_eval = false;
    }

    /// Process buffer commands
    fn manipulate_buffer(&mut self, cmd: BufferCommands) {
        match cmd {
            BufferCommands::New => match Self::edit_buffer(None) {
                Ok(s) =>println!("new buffer number: {}", self.buffers.add(s)),
                Err(e) => println!("failed to open editor: {e}"),
            }

            BufferCommands::Delete { num } => self.buffers.delete(num),
            BufferCommands::Edit { num } => 
            if let Some(b) = self.buffers.get(num) {
                match Self::edit_buffer(Some(b)) {
                    Ok(s) => self.buffers.replace(num, s),
                    Err(e) => println!("failed to open editor: {e}"),
                }
            } else {
                println!("no such buffer")
            }
        
            BufferCommands::Print { num } => 
            if let Some(b) = self.buffers.get(num) {
                println!("{b}");
            } else {
                println!("no such buffer")
            }
            
            BufferCommands::List => println!("{}", self.buffers),
        }
    }

    /// Save state to file
    fn save(state: SavedState, path: &str) -> Result<()> {
        let f = File::create(path)?;
        serde_json::to_writer_pretty(&f, &state)?;
        Ok(())
    }

    /// Load state from file
    fn load(path: &str) -> Result<SavedState> {
        let f = File::open(path)?;
        let state: SavedState = serde_json::from_reader(f)?;
        Ok(state)
    }
}

/// Public methods
impl DebuggerFrontend {
    pub fn new() -> Self {
        Self { 
            last_cmd: Commands::default(),
            buffers: ScriptBuffers::new(),
            during_eval: false
        }
    }

    /// Send last parsed args to debugger
    pub fn do_actions(&mut self, dbg: &mut dbg::SqDebugger) {
        match &self.last_cmd {
            Commands::Step => if let Err(e) = dbg.step() {
                println!("step failed: {e}");
            }

            Commands::Continue => dbg.resume(),

            Commands::Backtrace => match dbg.get_backtrace() {
                Ok(bt) => Self::print_backtrace(bt),
                Err(e) => println!("failed to get backtrace: {e}"),
            }

            Commands::Locals { level } =>
            match dbg.get_locals(*level, 0) {
                Ok(locals) => Self::print_locals(locals),
                Err(e) => println!("failed to get locals: {e}"),
            }

            Commands::Examine { level, target, depth } 
                => Self::examine(dbg, target, *level, *depth),
                
            Commands::BreakpointAdd { spec } => Self::add_breakpoint(dbg, spec),
            Commands::BreakpointEnable { num } => dbg.breakpoints().enable(*num, true),
            Commands::BreakpointDisable { num } => dbg.breakpoints().enable(*num, false),
            Commands::BreakpointClear { num } => dbg.breakpoints().remove(*num),
            Commands::BreakpointList => println!("Breakpoints:\n{}", dbg.breakpoints()),

            Commands::Evaluate { debug , buffer } 
                => self.eval_script(dbg, *debug, *buffer),

            Commands::Buffer(cmd) => self.manipulate_buffer(*cmd),

            Commands::Trace => if let Err(e) = dbg.start_tracing() {
                println!("trace failed: {e}")
            }

            Commands::Load { file } => 
            match Self::load(file.as_deref().unwrap_or(DEFAULT_STATE_FILENAME)) {
                Ok(SavedState { buffers, breakpoints }) => {
                    self.buffers = buffers;
                    dbg.set_breakpoints(breakpoints);
                },
                Err(e) => println!("Failed to load state: {e}"),
            },

            Commands::Save { file } => {
                let state = SavedState { 
                    buffers: self.buffers.clone(),
                    breakpoints: dbg.breakpoints().clone()
                };

                if let Err(e) = Self::save(state, file.as_deref().unwrap_or(DEFAULT_STATE_FILENAME)) {
                    println!("Failed to save state: {e}")
                }
            }


            Commands::Set(var) => Self::set_var(var),
            Commands::Nop => (),
            Commands::Exit => std::process::exit(0),
        }
    }

    /// Save arguments to internal buffer, if successful
    pub fn parse_args(&mut self, args: &str) -> Result<()> {
        match Self::cli().try_get_matches_from(args.trim().split(' ')) {
            Ok(m) => {
                self.last_cmd = Commands::from_arg_matches(&m)?;
                Ok(())
            },
            Err(e) => Err(e.into()),
        }
    }
}

/// Struct that holds multiple string with script
#[derive(Clone, Serialize, Deserialize)]
struct ScriptBuffers {
    store: Vec<(u32, String)>,
    counter: u32,
}

impl ScriptBuffers {
    /// Create new ScriptBuffers
    pub fn new() -> Self {
        Self { store: vec![], counter: 1 }
    }

    /// Add buffer. Returns added buffer number
    pub fn add(&mut self, buf: String) -> u32 {
        self.store.push((self.counter, buf));
        self.counter += 1;
        self.counter - 1
    }

    /// Get buffer by number
    pub fn get(&mut self, number: u32) -> Option<&String> {
        self.store.iter().find(|(n, _)| *n == number).map(|(_, b)| b)
    }

    /// Replace buffer by number. If buffer with specified number doesn't exist, do nothing
    pub fn replace(&mut self, number: u32, buf: String) {
        if let Some(b) = self.store.iter_mut().find(|(n, _)| *n == number) {
            *b = (number, buf);
        }
    }

    /// Delete buffer by number
    pub fn delete(&mut self, number: u32) {
        self.store.retain(|(n, _)| *n != number)
    }
}

impl std::fmt::Display for ScriptBuffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const NUM_FIELD: usize = 8;

        if self.store.is_empty() {
            return write!(f, "no buffers available");
        }

        write!(f, "{:<NUM_FIELD$}content", "number")?;
        for (n, buf) in &self.store {
            // print separating newline
            writeln!(f)?;
            let line = buf.lines().next();
            write!(f, "{n:<NUM_FIELD$}{} ...", if let Some(l) = line{ l } else { "" })?;
        }
        Ok(())
    }
}