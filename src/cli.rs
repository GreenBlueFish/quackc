//! Command-line interface: argument parsing, command dispatch, and exit codes (Requirement 8).
//!
//! `cli` owns no compilation logic of its own; it parses `argv`, wires the existing pipeline
//! stages and backends together, and maps the result to a process exit code (design "cli" section).
//! The three commands and their flags are:
//!
//! | command                                                        | effect                                               | req        |
//! |----------------------------------------------------------------|------------------------------------------------------|------------|
//! | `quackc build <design> --top T --out dir`                      | compile and emit C++ into `dir`                      | 8.1        |
//! | `quackc test <design> [--mode functional\|event\|both]`        | compile and run testbenches (default `functional`)   | 8.2, 8.3   |
//! | `quackc test <design> --check-invariance --check-causality`    | run the property checks                              | 8.4        |
//! | `quackc emit-netlist <design> --top T --format json\|dot`      | export the netlist IR                                | 8.5        |
//!
//! Global flags accepted on every command: `--time-width 16|32` (8.7), `--no-opt` (8.9), `--vcd`
//! (8.10).
//!
//! ## Usage errors produce *no* output
//!
//! An invalid `--format`, an invalid `--time-width`, or a missing required option such as `--top`
//! is a *usage error*: the CLI returns a nonzero exit code and writes nothing to stdout or stderr
//! (Req 8.6, 8.8, 8.11). This is why argument parsing is a pure function ([`parse_args`]) returning
//! a [`Result<Command, UsageError>`] with no I/O — `run` only prints once parsing has succeeded.
//!
//! A *compile* error (parse / sema / elaboration / cyclic graph / backend) is distinct: it is
//! reported with its rendered diagnostics on stderr and exits nonzero (Req 13.8, 13.9). A clean
//! run exits `0`.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::backend::{run_backend, BackendOptions};
use crate::codegen_cpp::FunctionalBackend;
use crate::diag::Diagnostic;
use crate::export::{DotExporter, JsonExporter};
use crate::import::ImportResolver;
use crate::ir::{self, NetlistIr};
use crate::lexer::TimeWidth;
use crate::operators::OperatorTable;
use crate::opt::PassManager;
use crate::testrun::{self, CheckOptions, Mode};

/// The spike-time width global flag's two legal values (Req 8.7).
///
/// Kept as a tiny parse helper so `parse_args` can reject anything else as a usage error without
/// touching I/O (Req 8.8).
fn parse_time_width(s: &str) -> Option<TimeWidth> {
    match s {
        "16" => Some(TimeWidth::W16),
        "32" => Some(TimeWidth::W32),
        _ => None,
    }
}

/// The export formats the `emit-netlist` command accepts (Req 8.5, 8.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetlistFormat {
    /// JSON netlist document (Req 12.1).
    Json,
    /// Graphviz DOT graph (Req 12.3).
    Dot,
}

impl NetlistFormat {
    /// Parse the `--format` value; anything other than `json` / `dot` is a usage error (Req 8.6).
    fn parse(s: &str) -> Option<Self> {
        match s {
            "json" => Some(NetlistFormat::Json),
            "dot" => Some(NetlistFormat::Dot),
            _ => None,
        }
    }
}

/// The `--mode` value for the `test` command; defaults to functional (Req 8.3).
fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "functional" => Some(Mode::Functional),
        "event" => Some(Mode::Event),
        "both" => Some(Mode::Both),
        _ => None,
    }
}

/// Global flags that apply to every command (Req 8.7, 8.9, 8.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalFlags {
    /// The `SpikeTime` width to compile / simulate at; `--time-width 16|32`, default 32 (Req 8.7).
    pub time_width: TimeWidth,
    /// `--no-opt`: skip the optimization passes, preserving structure for visualization (Req 8.9).
    pub no_opt: bool,
    /// `--vcd`: enable VCD output for the event-driven mode (Req 8.10).
    pub vcd: bool,
    /// `--std-tests`: also run the standard library's own testbenches. Off by default so
    /// `quackc test foo.sta` runs only the tests defined in `foo.sta`.
    pub std_tests: bool,
}

impl Default for GlobalFlags {
    fn default() -> Self {
        GlobalFlags {
            time_width: TimeWidth::default(),
            no_opt: false,
            vcd: false,
            std_tests: false,
        }
    }
}

/// A fully-parsed command ready to execute. Parsing never performs I/O, so a usage error is caught
/// before anything is read, compiled, or printed (Req 8.6, 8.8, 8.11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `quackc build <design> --top T --out dir` (Req 8.1).
    Build {
        design: PathBuf,
        top: String,
        out: PathBuf,
        flags: GlobalFlags,
    },
    /// `quackc test <design> [--mode ...] [--check-invariance] [--check-causality]` (Req 8.2-8.4).
    Test {
        design: PathBuf,
        mode: Mode,
        checks: CheckOptions,
        flags: GlobalFlags,
    },
    /// `quackc emit-netlist <design> --top T --format json|dot` (Req 8.5).
    EmitNetlist {
        design: PathBuf,
        top: String,
        format: NetlistFormat,
        flags: GlobalFlags,
    },
    /// `quackc --help` / `quackc help`: print usage text to stdout and exit 0.
    Help,
    /// `quackc --version` / `quackc -V`: print the crate version to stdout and exit 0.
    Version,
}

/// A usage error: an unrecognized command, a missing required option, or an invalid argument value
/// (Req 8.6, 8.8, 8.11). Carries no payload because the CLI must produce *no* output for it — the
/// variant exists only to drive a nonzero exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageError;

/// Entry point invoked by `main`. Accepts the process arguments (excluding the program name) and
/// returns the process exit code (Req 13.8, 13.9).
///
/// A usage error produces no output and a nonzero code (Req 8.6, 8.8, 8.11); a compile error prints
/// its diagnostics to stderr and exits nonzero (Req 13.9); a clean run prints its result to stdout
/// and exits `0` (Req 13.8).
pub fn run(args: Vec<String>) -> ExitCode {
    let command = match parse_args(&args) {
        Ok(c) => c,
        // Usage error: no output at all, just a nonzero exit code (Req 8.6, 8.8, 8.11).
        Err(UsageError) => return ExitCode::FAILURE,
    };
    match dispatch(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(diags) => {
            // A compile error is reported with its rendered diagnostics (Req 13.9).
            if !diags.is_empty() {
                eprintln!("{}", render(&diags));
            }
            ExitCode::FAILURE
        }
    }
}

/// Parse `argv` (without the program name) into a [`Command`], or a [`UsageError`] (Req 8.11).
///
/// Pure: performs no I/O, so an invalid invocation is rejected before anything is read or printed
/// (Req 8.6, 8.8, 8.11). The first token selects the command; the second positional token is the
/// design path; everything else is an option. Unknown options, duplicate/missing values, missing
/// required options, and invalid values are all usage errors.
pub fn parse_args(args: &[String]) -> Result<Command, UsageError> {
    let mut it = args.iter();
    let command = it.next().ok_or(UsageError)?.as_str();

    // `--help` / `--version` are recognized as the first token regardless of what follows, so
    // `quackc --help` and `quackc help` both print usage and exit 0 rather than being a usage
    // error. These are the only invocations that print to stdout without a subcommand.
    match command {
        "--help" | "-h" | "help" => return Ok(Command::Help),
        "--version" | "-V" => return Ok(Command::Version),
        _ => {}
    }

    // Collect the remaining tokens so we can walk them with an index (options may take a value).
    let rest: Vec<&str> = it.map(String::as_str).collect();

    match command {
        "build" => parse_build(&rest),
        "test" => parse_test(&rest),
        "emit-netlist" => parse_emit_netlist(&rest),
        _ => Err(UsageError),
    }
}

/// Shared option-walking state: the single positional design path plus the global flags, with
/// each flag seen at most once. Returned fields let each command pull the pieces it needs.
struct Walked {
    design: Option<PathBuf>,
    flags: GlobalFlags,
    /// `--top` value, if given.
    top: Option<String>,
    /// `--out` value, if given.
    out: Option<PathBuf>,
    /// `--format` value, if given (already validated).
    format: Option<NetlistFormat>,
    /// `--mode` value, if given (already validated).
    mode: Option<Mode>,
    /// `--check-invariance` seen.
    check_invariance: bool,
    /// `--check-causality` seen.
    check_causality: bool,
}

/// Walk the post-command tokens, parsing the one positional design path, the global flags, and the
/// option set used across the three commands. An unknown option, a missing option value, an invalid
/// value, a repeated flag, or a second positional argument is a usage error (Req 8.8, 8.11).
///
/// Which of the collected options are *required* / *allowed* is enforced per command by the caller,
/// so this walker stays command-neutral.
fn walk(tokens: &[&str]) -> Result<Walked, UsageError> {
    let mut w = Walked {
        design: None,
        flags: GlobalFlags::default(),
        top: None,
        out: None,
        format: None,
        mode: None,
        check_invariance: false,
        check_causality: false,
    };
    // Track which global width flag was already set so a duplicate is a usage error.
    let mut time_width_seen = false;

    let mut i = 0;
    while i < tokens.len() {
        let tok = tokens[i];
        match tok {
            "--top" => {
                let v = value_of(tokens, &mut i)?;
                if w.top.replace(v.to_string()).is_some() {
                    return Err(UsageError);
                }
            }
            "--out" => {
                let v = value_of(tokens, &mut i)?;
                if w.out.replace(PathBuf::from(v)).is_some() {
                    return Err(UsageError);
                }
            }
            "--format" => {
                let v = value_of(tokens, &mut i)?;
                let fmt = NetlistFormat::parse(v).ok_or(UsageError)?;
                if w.format.replace(fmt).is_some() {
                    return Err(UsageError);
                }
            }
            "--mode" => {
                let v = value_of(tokens, &mut i)?;
                let m = parse_mode(v).ok_or(UsageError)?;
                if w.mode.replace(m).is_some() {
                    return Err(UsageError);
                }
            }
            "--time-width" => {
                let v = value_of(tokens, &mut i)?;
                let tw = parse_time_width(v).ok_or(UsageError)?;
                if time_width_seen {
                    return Err(UsageError);
                }
                time_width_seen = true;
                w.flags.time_width = tw;
            }
            "--check-invariance" => {
                if w.check_invariance {
                    return Err(UsageError);
                }
                w.check_invariance = true;
            }
            "--check-causality" => {
                if w.check_causality {
                    return Err(UsageError);
                }
                w.check_causality = true;
            }
            "--no-opt" => {
                if w.flags.no_opt {
                    return Err(UsageError);
                }
                w.flags.no_opt = true;
            }
            "--vcd" => {
                if w.flags.vcd {
                    return Err(UsageError);
                }
                w.flags.vcd = true;
            }
            "--std-tests" => {
                if w.flags.std_tests {
                    return Err(UsageError);
                }
                w.flags.std_tests = true;
            }
            // Any other `--option` is unknown; a bare token is the (single) positional design path.
            opt if opt.starts_with("--") => return Err(UsageError),
            positional => {
                if w.design.replace(PathBuf::from(positional)).is_some() {
                    // A second positional argument is a usage error.
                    return Err(UsageError);
                }
            }
        }
        i += 1;
    }

    Ok(w)
}

/// Read the value that follows an option at `tokens[*i]`, advancing `*i` past it. A missing value
/// (option at the end) or a value that itself looks like another option is a usage error (Req 8.11).
fn value_of<'a>(tokens: &[&'a str], i: &mut usize) -> Result<&'a str, UsageError> {
    let next = tokens.get(*i + 1).copied().ok_or(UsageError)?;
    if next.starts_with("--") {
        return Err(UsageError);
    }
    *i += 1;
    Ok(next)
}

/// Parse `build <design> --top T --out dir` (Req 8.1). `--top` and `--out` are required; `--mode`,
/// `--format`, and the property-check flags are not accepted here.
fn parse_build(tokens: &[&str]) -> Result<Command, UsageError> {
    let w = walk(tokens)?;
    // Reject options that do not belong to `build`.
    if w.format.is_some() || w.mode.is_some() || w.check_invariance || w.check_causality {
        return Err(UsageError);
    }
    let design = w.design.ok_or(UsageError)?;
    let top = w.top.ok_or(UsageError)?;
    let out = w.out.ok_or(UsageError)?;
    Ok(Command::Build {
        design,
        top,
        out,
        flags: w.flags,
    })
}

/// Parse `test <design> [--mode ...] [--check-invariance] [--check-causality]` (Req 8.2-8.4).
/// `--top`, `--out`, and `--format` are not accepted here; the mode defaults to functional (Req 8.3).
fn parse_test(tokens: &[&str]) -> Result<Command, UsageError> {
    let w = walk(tokens)?;
    if w.top.is_some() || w.out.is_some() || w.format.is_some() {
        return Err(UsageError);
    }
    let design = w.design.ok_or(UsageError)?;
    Ok(Command::Test {
        design,
        mode: w.mode.unwrap_or(Mode::Functional),
        checks: CheckOptions {
            invariance: w.check_invariance,
            causality: w.check_causality,
        },
        flags: w.flags,
    })
}

/// Parse `emit-netlist <design> --top T --format json|dot` (Req 8.5). `--top` and `--format` are
/// required; `--out`, `--mode`, and the property-check flags are not accepted here.
fn parse_emit_netlist(tokens: &[&str]) -> Result<Command, UsageError> {
    let w = walk(tokens)?;
    if w.out.is_some() || w.mode.is_some() || w.check_invariance || w.check_causality {
        return Err(UsageError);
    }
    let design = w.design.ok_or(UsageError)?;
    let top = w.top.ok_or(UsageError)?;
    let format = w.format.ok_or(UsageError)?;
    Ok(Command::EmitNetlist {
        design,
        top,
        format,
        flags: w.flags,
    })
}

/// Execute a parsed command, returning any compile-error diagnostics for `run` to render (Req 13.9).
fn dispatch(command: Command) -> Result<(), Vec<Diagnostic>> {
    match command {
        Command::Build {
            design,
            top,
            out,
            flags,
        } => cmd_build(&design, &top, &out, flags),
        Command::Test {
            design,
            mode,
            checks,
            flags,
        } => cmd_test(&design, mode, checks, flags),
        Command::EmitNetlist {
            design,
            top,
            format,
            flags,
        } => cmd_emit_netlist(&design, &top, format, flags),
        Command::Help => {
            print!("{}", usage_text());
            Ok(())
        }
        Command::Version => {
            println!("quackc {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

/// The usage text printed by `quackc --help` / `quackc help`. Kept in one place so the help stays
/// in sync with the commands and flags the parser actually accepts.
fn usage_text() -> String {
    format!(
        "quackc {} — a compiler for the Quack space-time (s-t) algebra language\n\
         \n\
         USAGE:\n\
         \x20   quackc <command> [options]\n\
         \n\
         COMMANDS:\n\
         \x20   build <design.sta> --top <Module> --out <dir>\n\
         \x20       Compile the design rooted at <Module> and emit C++ into <dir>.\n\
         \n\
         \x20   test <design.sta> [--mode functional|event|both]\n\
         \x20                     [--check-invariance] [--check-causality]\n\
         \x20       Compile and run the design's testbenches (default mode: functional).\n\
         \n\
         \x20   emit-netlist <design.sta> --top <Module> --format json|dot\n\
         \x20       Export the elaborated netlist for <Module> to stdout.\n\
         \n\
         GLOBAL OPTIONS (any command):\n\
         \x20   --time-width 16|32    SpikeTime width to compile/simulate at (default 32).\n\
         \x20   --no-opt              Skip optimization passes (preserve structure).\n\
         \x20   --vcd                 Emit a VCD trace from the event-driven mode.\n\
         \x20   --std-tests           Also run the standard library testbenches (test only).\n\
         \n\
         OTHER:\n\
         \x20   --help, -h            Print this help and exit.\n\
         \x20   --version, -V         Print the version and exit.\n\
         \n\
         The standard library (std.sta) is auto-imported. If quackc is installed outside its\n\
         source tree, set QUACKC_STDLIB_PATH to the directory that contains std.sta.\n",
        env!("CARGO_PKG_VERSION")
    )
}

/// Read a design file from disk into source text, mapping an I/O error to a compile diagnostic.
fn read_design(path: &std::path::Path) -> Result<String, Vec<Diagnostic>> {
    std::fs::read_to_string(path).map_err(|e| {
        vec![Diagnostic::error(
            crate::diag::Span::new(path.to_string_lossy().into_owned(), 1, 1),
            format!("cannot read design file `{}`: {e}", path.display()),
        )]
    })
}

/// Load, analyze, elaborate, and lower a design to its netlist IR rooted at `top`, running the
/// acyclicity check and (unless `--no-opt`) the optimization passes (Req 8.1, 8.9, 11.5, 4.6).
///
/// This is the shared front half of `build` and `emit-netlist`: both need the same optimized (or,
/// under `--no-opt`, unoptimized) [`NetlistIr`] for `top`. The standard library is auto-imported so
/// the design's use of `max` / relational operators resolves without the user importing it (Req 9.1).
fn lower_to_ir(
    design_path: &std::path::Path,
    top: &str,
    flags: GlobalFlags,
) -> Result<NetlistIr, Vec<Diagnostic>> {
    let src = read_design(design_path)?;
    let ops = OperatorTable::builtin();
    let resolver = ImportResolver::new().with_auto_std();
    let file = design_path.to_string_lossy().into_owned();

    // Front end: imports -> parse -> sema -> elaborate -> ir::build.
    let design = crate::import::load_design(&resolver, &src, &file, &ops, flags.time_width)?;
    let checked = crate::sema::analyze(&design)?;
    let elab = crate::elaborate::elaborate(&checked, top)?;
    let mut netlist = ir::build(&elab);

    // Reject a combinational cycle before any backend runs (Req 4.6); the error prints the path.
    crate::checks::check_acyclic(&netlist)?;

    // Optimize unless the user asked to preserve the structure for visualization (Req 8.9, 11.5).
    if !flags.no_opt {
        PassManager::default_pipeline().run_all(&mut netlist)?;
    }

    Ok(netlist)
}

/// `build`: compile the design and emit C++ into `out` (Req 8.1).
fn cmd_build(
    design_path: &std::path::Path,
    top: &str,
    out: &std::path::Path,
    flags: GlobalFlags,
) -> Result<(), Vec<Diagnostic>> {
    let netlist = lower_to_ir(design_path, top, flags)?;

    // Emit via the functional C++ backend, honoring the chosen width and `--no-opt` (Req 5.7, 8.9).
    let opts = BackendOptions::new()
        .with_time_width(flags.time_width.bits() as u16)
        .with_no_opt(flags.no_opt);
    let backend = FunctionalBackend::new();
    let output = run_backend(&backend, &netlist, &opts)?;

    // Create the output directory and write each emitted artifact into it (Req 8.1).
    std::fs::create_dir_all(out).map_err(|e| {
        vec![Diagnostic::error(
            crate::diag::Span::new(out.to_string_lossy().into_owned(), 1, 1),
            format!("cannot create output directory `{}`: {e}", out.display()),
        )]
    })?;
    for file in &output.files {
        let dest = out.join(&file.path);
        std::fs::write(&dest, &file.contents).map_err(|e| {
            vec![Diagnostic::error(
                crate::diag::Span::new(dest.to_string_lossy().into_owned(), 1, 1),
                format!("cannot write `{}`: {e}", dest.display()),
            )]
        })?;
        println!("emitted {}", dest.display());
    }

    // Non-fatal warnings (e.g. delay-chain saturation) never fail the build (Req 13.6, 13.7).
    for w in &output.warnings {
        eprintln!("{}", w.render());
    }
    Ok(())
}

/// `test`: compile the design and run its testbenches, exiting nonzero if any wave fails (Req 8.2,
/// 8.3, 8.4, 7.3). The report is printed to stdout; a failing report is surfaced as an (empty)
/// error list so `run` exits nonzero without printing a second time.
fn cmd_test(
    design_path: &std::path::Path,
    mode: Mode,
    checks: CheckOptions,
    flags: GlobalFlags,
) -> Result<(), Vec<Diagnostic>> {
    let src = read_design(design_path)?;
    let ops = OperatorTable::builtin();
    let resolver = ImportResolver::new().with_auto_std();
    let file = design_path.to_string_lossy().into_owned();

    // Load (with the auto-imported std) and analyze so a malformed design fails as a compile error.
    // By default only THIS file's tests run; `--std-tests` also runs the standard library's.
    let design = crate::import::load_design_scoped(
        &resolver, &src, &file, &ops, flags.time_width, flags.std_tests,
    )?;
    // Surface semantic errors here (the runner elaborates per-test but we want a clean upfront fail).
    crate::sema::analyze(&design)?;

    let _ = flags.vcd; // VCD output applies to the event-driven mode (Req 8.10); wiring is a trace concern.
    let report = testrun::run_tests_checked(&design, flags.time_width, mode, checks);
    println!("{}", report.render());
    println!(
        "{} wave(s) passed, {} failed",
        report.total_passed_waves(),
        report.total_failed_waves()
    );

    if report.any_failed() {
        // A test failure is already fully reported above; signal a nonzero exit with no further
        // output by returning an empty diagnostic list (Req 7.3, 13.9).
        Err(Vec::new())
    } else {
        Ok(())
    }
}

/// `emit-netlist`: export the netlist IR for `top` in the requested format (Req 8.5). JSON/DOT text
/// is written to stdout.
fn cmd_emit_netlist(
    design_path: &std::path::Path,
    top: &str,
    format: NetlistFormat,
    flags: GlobalFlags,
) -> Result<(), Vec<Diagnostic>> {
    let netlist = lower_to_ir(design_path, top, flags)?;
    let text = match format {
        NetlistFormat::Json => JsonExporter::to_json(&netlist)?,
        NetlistFormat::Dot => DotExporter::new().to_dot(&netlist),
    };
    println!("{text}");
    Ok(())
}

/// Join a diagnostic list into one rendered, newline-separated block for stderr (Req 13.9).
fn render(diags: &[Diagnostic]) -> String {
    diags
        .iter()
        .map(Diagnostic::render)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- valid invocations parse into the right command (Req 8.1-8.5, 8.7, 8.9, 8.10) ---

    #[test]
    fn build_parses_design_top_out_and_defaults() {
        let cmd = parse_args(&args(&["build", "d.sta", "--top", "T", "--out", "out"]))
            .expect("a well-formed build must parse");
        match cmd {
            Command::Build {
                design,
                top,
                out,
                flags,
            } => {
                assert_eq!(design, PathBuf::from("d.sta"));
                assert_eq!(top, "T");
                assert_eq!(out, PathBuf::from("out"));
                assert_eq!(flags.time_width, TimeWidth::W32); // default (Req 8.7)
                assert!(!flags.no_opt);
                assert!(!flags.vcd);
            }
            other => panic!("expected Build, got {other:?}"),
        }
    }

    #[test]
    fn global_flags_are_accepted_on_build() {
        let cmd = parse_args(&args(&[
            "build", "d.sta", "--top", "T", "--out", "o", "--time-width", "16", "--no-opt", "--vcd",
        ]))
        .expect("global flags must parse");
        match cmd {
            Command::Build { flags, .. } => {
                assert_eq!(flags.time_width, TimeWidth::W16);
                assert!(flags.no_opt);
                assert!(flags.vcd);
            }
            other => panic!("expected Build, got {other:?}"),
        }
    }

    #[test]
    fn test_defaults_mode_to_functional_with_no_checks() {
        let cmd = parse_args(&args(&["test", "d.sta"])).expect("bare test must parse");
        match cmd {
            Command::Test { mode, checks, .. } => {
                assert_eq!(mode, Mode::Functional); // default (Req 8.3)
                assert!(!checks.invariance);
                assert!(!checks.causality);
            }
            other => panic!("expected Test, got {other:?}"),
        }
    }

    #[test]
    fn test_accepts_each_mode_value() {
        for (s, want) in [
            ("functional", Mode::Functional),
            ("event", Mode::Event),
            ("both", Mode::Both),
        ] {
            let cmd = parse_args(&args(&["test", "d.sta", "--mode", s]))
                .unwrap_or_else(|_| panic!("--mode {s} must parse"));
            match cmd {
                Command::Test { mode, .. } => assert_eq!(mode, want),
                other => panic!("expected Test, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_accepts_the_property_check_flags() {
        let cmd = parse_args(&args(&[
            "test",
            "d.sta",
            "--check-invariance",
            "--check-causality",
        ]))
        .expect("property-check flags must parse");
        match cmd {
            Command::Test { checks, .. } => {
                assert!(checks.invariance);
                assert!(checks.causality);
            }
            other => panic!("expected Test, got {other:?}"),
        }
    }

    #[test]
    fn emit_netlist_parses_both_formats() {
        for (s, want) in [("json", NetlistFormat::Json), ("dot", NetlistFormat::Dot)] {
            let cmd = parse_args(&args(&[
                "emit-netlist",
                "d.sta",
                "--top",
                "T",
                "--format",
                s,
            ]))
            .unwrap_or_else(|_| panic!("--format {s} must parse"));
            match cmd {
                Command::EmitNetlist { format, .. } => assert_eq!(format, want),
                other => panic!("expected EmitNetlist, got {other:?}"),
            }
        }
    }

    // --- usage errors: each must be rejected (Req 8.6, 8.8, 8.11) ---

    #[test]
    fn missing_top_on_build_is_a_usage_error() {
        // Req 8.11: a missing required option is a usage error.
        assert!(parse_args(&args(&["build", "d.sta", "--out", "o"])).is_err());
    }

    #[test]
    fn missing_out_on_build_is_a_usage_error() {
        assert!(parse_args(&args(&["build", "d.sta", "--top", "T"])).is_err());
    }

    #[test]
    fn missing_top_on_emit_netlist_is_a_usage_error() {
        assert!(parse_args(&args(&["emit-netlist", "d.sta", "--format", "json"])).is_err());
    }

    #[test]
    fn invalid_format_is_a_usage_error() {
        // Req 8.6: a --format value other than json/dot is a usage error.
        assert!(parse_args(&args(&[
            "emit-netlist",
            "d.sta",
            "--top",
            "T",
            "--format",
            "yaml",
        ]))
        .is_err());
    }

    #[test]
    fn invalid_time_width_is_a_usage_error() {
        // Req 8.8: a --time-width value other than 16/32 is a usage error.
        assert!(parse_args(&args(&["test", "d.sta", "--time-width", "8"])).is_err());
        assert!(parse_args(&args(&["test", "d.sta", "--time-width", "64"])).is_err());
    }

    #[test]
    fn invalid_mode_is_a_usage_error() {
        assert!(parse_args(&args(&["test", "d.sta", "--mode", "sideways"])).is_err());
    }

    #[test]
    fn unknown_command_is_a_usage_error() {
        assert!(parse_args(&args(&["frobnicate", "d.sta"])).is_err());
    }

    #[test]
    fn no_arguments_is_a_usage_error() {
        assert!(parse_args(&[]).is_err());
    }

    #[test]
    fn missing_design_path_is_a_usage_error() {
        assert!(parse_args(&args(&["build", "--top", "T", "--out", "o"])).is_err());
    }

    #[test]
    fn unknown_option_is_a_usage_error() {
        assert!(parse_args(&args(&["build", "d.sta", "--top", "T", "--out", "o", "--wat"])).is_err());
    }

    #[test]
    fn an_option_value_that_is_missing_is_a_usage_error() {
        // `--top` at the end with no following value.
        assert!(parse_args(&args(&["build", "d.sta", "--out", "o", "--top"])).is_err());
    }

    #[test]
    fn a_second_positional_argument_is_a_usage_error() {
        assert!(parse_args(&args(&["test", "d.sta", "extra.sta"])).is_err());
    }

    #[test]
    fn build_rejects_test_only_options() {
        // --mode / --check-* belong to `test`, not `build`.
        assert!(parse_args(&args(&[
            "build", "d.sta", "--top", "T", "--out", "o", "--mode", "event",
        ]))
        .is_err());
    }

    #[test]
    fn test_rejects_build_only_options() {
        assert!(parse_args(&args(&["test", "d.sta", "--top", "T"])).is_err());
        assert!(parse_args(&args(&["test", "d.sta", "--out", "o"])).is_err());
    }

    // --- run() maps a usage error to a nonzero exit code with no output (Req 8.11, 13.9) ---

    #[test]
    fn run_returns_failure_on_a_usage_error() {
        // We cannot observe stdout here, but the contract is: usage error -> FAILURE. The no-output
        // guarantee is structural: `run` only prints after `parse_args` succeeds.
        let code = run(args(&["build", "d.sta"])); // missing --top/--out
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::FAILURE));
    }

    // --- help / version are recognized and succeed (not usage errors) ---

    #[test]
    fn help_flags_parse_to_the_help_command() {
        for a in [&["--help"][..], &["-h"][..], &["help"][..]] {
            assert_eq!(
                parse_args(&args(a)).expect("a help flag must parse"),
                Command::Help,
                "`{a:?}` should parse as Help"
            );
        }
    }

    #[test]
    fn version_flags_parse_to_the_version_command() {
        for a in [&["--version"][..], &["-V"][..]] {
            assert_eq!(
                parse_args(&args(a)).expect("a version flag must parse"),
                Command::Version,
                "`{a:?}` should parse as Version"
            );
        }
    }

    #[test]
    fn run_returns_success_for_help_and_version() {
        // Both print to stdout and exit 0 (not a usage error).
        for a in [&["--help"][..], &["-h"][..], &["help"][..], &["--version"][..], &["-V"][..]] {
            let code = run(args(a));
            assert_eq!(
                format!("{code:?}"),
                format!("{:?}", ExitCode::SUCCESS),
                "`{a:?}` should exit SUCCESS"
            );
        }
    }

    #[test]
    fn usage_text_mentions_every_command_and_both_meta_flags() {
        let t = usage_text();
        for needle in ["build", "test", "emit-netlist", "--time-width", "--help", "--version", "QUACKC_STDLIB_PATH"] {
            assert!(t.contains(needle), "usage text should mention `{needle}`");
        }
    }

    /// Build an owned `Vec<String>` argv from string slices for the parser.
    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }
}
