//! End-to-end CLI tests (Requirement 8, 13.8, 13.9).
//!
//! These drive the real `quackc` binary (via `CARGO_BIN_EXE_quackc`) so they exercise the actual
//! process exit code and output behavior, not just the pure argument parser (which is unit-tested
//! inside `src/cli.rs`). They prove:
//!
//! * a valid `build` compiles a design and emits a `<top>.hpp` into the output directory, exit `0`
//!   (Req 8.1, 13.8);
//! * a valid `emit-netlist --format json|dot` prints the netlist and exits `0` (Req 8.5, 13.8);
//! * a valid `test` runs the testbenches and exits `0` when they pass (Req 8.2, 13.8);
//! * each usage error — invalid `--format`, invalid `--time-width`, missing `--top` — exits
//!   nonzero and prints *nothing* to stdout or stderr (Req 8.6, 8.8, 8.11).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A minimal self-contained design: `max` resolves through the auto-imported `std.sta`, and a
/// `test` block with one passing wave lets the `test` command exit `0`.
const DESIGN: &str = r#"module MySort (
    input  time a, input time b,
    output time lo, output time hi
) {
    lo = min(a, b);
    hi = max(a, b);
}

test mysort_t for MySort {
    wave { a = 3; b = 5; expect lo = 3; expect hi = 5; }
    wave { a = 7; b = 2; expect lo = 2; expect hi = 7; }
}"#;

/// Run the compiled `quackc` binary with `args` from a scratch working directory, returning its
/// captured `Output` (status + stdout + stderr).
fn run_quackc(workdir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quackc"))
        .args(args)
        .current_dir(workdir)
        .output()
        .expect("the quackc binary should be runnable")
}

/// Create a unique scratch directory under the OS temp dir and write `DESIGN` into `design.sta`.
/// Returns the directory path (the caller removes it when done).
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "quackc_cli_{tag}_{}_{}",
        std::process::id(),
        unique()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    std::fs::write(dir.join("design.sta"), DESIGN).expect("write design");
    dir
}

/// A tiny per-call counter so parallel test cases get distinct scratch directories.
fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

// --- valid commands succeed (Req 8.1, 8.2, 8.5, 13.8) -------------------------------------------

#[test]
fn build_emits_hpp_and_exits_zero() {
    let dir = scratch("build");
    let out = run_quackc(
        &dir,
        &["build", "design.sta", "--top", "MySort", "--out", "emitted"],
    );
    assert!(
        out.status.success(),
        "build should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The functional backend emits `<top>.hpp` into the --out directory (Req 8.1).
    let hpp = dir.join("emitted").join("MySort.hpp");
    assert!(hpp.is_file(), "expected emitted C++ at {}", hpp.display());
    let contents = std::fs::read_to_string(&hpp).unwrap();
    assert!(
        contents.contains("MySort_eval"),
        "emitted header should define the module eval entry point"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_netlist_json_prints_and_exits_zero() {
    let dir = scratch("json");
    let out = run_quackc(
        &dir,
        &["emit-netlist", "design.sta", "--top", "MySort", "--format", "json"],
    );
    assert!(
        out.status.success(),
        "emit-netlist json should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"schema_version\"") && stdout.contains("\"nodes\""),
        "JSON export should carry the schema version and nodes; got: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_netlist_dot_prints_and_exits_zero() {
    let dir = scratch("dot");
    let out = run_quackc(
        &dir,
        &["emit-netlist", "design.sta", "--top", "MySort", "--format", "dot"],
    );
    assert!(
        out.status.success(),
        "emit-netlist dot should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("digraph"),
        "DOT export should be a digraph; got: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_runs_passing_testbenches_and_exits_zero() {
    let dir = scratch("test");
    let out = run_quackc(&dir, &["test", "design.sta"]);
    assert!(
        out.status.success(),
        "test with passing waves should exit 0; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("PASS"), "report should show PASS; got: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_honors_time_width_global_flag() {
    let dir = scratch("tw");
    let out = run_quackc(&dir, &["test", "design.sta", "--time-width", "16"]);
    assert!(
        out.status.success(),
        "a valid --time-width 16 should be accepted; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// --- help / version print to stdout and exit 0 --------------------------------------------------

#[test]
fn help_prints_usage_and_exits_zero() {
    let dir = scratch("help");
    for flag in ["--help", "-h", "help"] {
        let out = run_quackc(&dir, &[flag]);
        assert!(out.status.success(), "`{flag}` should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("USAGE") && stdout.contains("build") && stdout.contains("emit-netlist"),
            "`{flag}` should print usage listing the commands; got: {stdout}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_prints_version_and_exits_zero() {
    let dir = scratch("version");
    for flag in ["--version", "-V"] {
        let out = run_quackc(&dir, &[flag]);
        assert!(out.status.success(), "`{flag}` should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("quackc") && stdout.contains(env!("CARGO_PKG_VERSION")),
            "`{flag}` should print the crate version; got: {stdout}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// --- usage errors: nonzero exit, no output (Req 8.6, 8.8, 8.11) ---------------------------------

/// Assert that `args` is a usage error: nonzero exit code and empty stdout *and* stderr.
fn assert_usage_error(tag: &str, args: &[&str]) {
    let dir = scratch(tag);
    let out = run_quackc(&dir, args);
    assert!(
        !out.status.success(),
        "`{args:?}` should exit nonzero (usage error)"
    );
    assert!(
        out.stdout.is_empty(),
        "`{args:?}` must produce no stdout; got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        out.stderr.is_empty(),
        "`{args:?}` must produce no stderr; got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_format_is_a_usage_error_with_no_output() {
    // Req 8.6: --format other than json/dot -> usage error, no output.
    assert_usage_error(
        "bad_format",
        &["emit-netlist", "design.sta", "--top", "MySort", "--format", "yaml"],
    );
}

#[test]
fn invalid_time_width_is_a_usage_error_with_no_output() {
    // Req 8.8: --time-width other than 16/32 -> usage error, no output.
    assert_usage_error("bad_tw", &["build", "design.sta", "--top", "MySort", "--out", "o", "--time-width", "8"]);
}

#[test]
fn missing_top_is_a_usage_error_with_no_output() {
    // Req 8.11: a missing required option -> usage error, no output.
    assert_usage_error("no_top", &["build", "design.sta", "--out", "o"]);
}

#[test]
fn missing_top_on_emit_netlist_is_a_usage_error_with_no_output() {
    assert_usage_error("no_top_emit", &["emit-netlist", "design.sta", "--format", "json"]);
}
