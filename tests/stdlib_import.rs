//! Standard-library auto-import end-to-end tests (task 25; Requirements 9.1, 9.2, 9.3, 9.4, 10.4,
//! 10.5, 14.3).
//!
//! These exercise the whole frontend path with the real `std.sta` resolved by the
//! [`ImportResolver`]'s auto-import:
//!
//!   1. `load_design` resolves and merges `std.sta` ahead of a user design, so the library's
//!      `func`s (`le`/`ge`/`eq`/`gt`/`neq`/`max`) and `module`s (`Le`..`Max`/`Min`) are in scope
//!      with no `import` written by the user and no compiler change (Req 9.1, 10.4).
//!   2. A user design that *instantiates* a std-library module (`Gt`, `Neq`, ...) — a construct the
//!      compiler has no built-in knowledge of — elaborates and lowers to a netlist IR, proving a
//!      library block is just ordinary modules/funcs (Req 10.4, 10.5).
//!   3. A user design that *calls* the derived operators by name compiles and, for sampled operand
//!      pairs that include `inf`, produces exactly the operators' if/else meanings (Req 9.2-9.4),
//!      evaluated through the public event engine.

use std::collections::HashMap;

use quackc::import::{self, ImportResolver};
use quackc::ir;
use quackc::lexer::TimeWidth;
use quackc::operators::OperatorTable;
use quackc::sema::analyze;
use quackc::sim_event::{self, SpikeTime, DEFAULT_MAX_TICK, INF};

/// Load `user_src` with the standard library auto-imported, returning the merged design (panicking
/// with rendered diagnostics on any parse/resolution error so the test starts from a clean state).
fn load_with_std(user_src: &str) -> quackc::ast::Design {
    let resolver = ImportResolver::new().with_auto_std();
    import::load_design(
        &resolver,
        user_src,
        "user.sta",
        &OperatorTable::builtin(),
        TimeWidth::W32,
    )
    .unwrap_or_else(|d| {
        let r: Vec<String> = d.iter().map(|x| x.render()).collect();
        panic!("load_design failed:\n{}", r.join("\n"))
    })
}

#[test]
fn auto_import_brings_std_blocks_into_scope() {
    // A user file that imports nothing still gets the standard library prepended (Req 9.1): the
    // merged design contains the library func forms and module forms.
    let design = load_with_std("module M (input time a, output time y) { y = a; }");

    let func_names: Vec<&str> = design.funcs.iter().map(|f| f.name.as_str()).collect();
    // `max` is now a built-in variadic operator (special-cased by the elaborator, like `min`), so
    // it is intentionally NOT a std.sta func. The relational/exclusive forms remain library funcs.
    for want in ["le", "ge", "eq", "gt", "neq", "emin", "emax"] {
        assert!(
            func_names.contains(&want),
            "std.sta func `{want}` should be auto-imported; have {func_names:?}"
        );
    }

    let module_names: Vec<&str> = design.modules.iter().map(|m| m.name.as_str()).collect();
    for want in ["Le", "Ge", "Eq", "Gt", "Neq", "Max", "Min"] {
        assert!(
            module_names.contains(&want),
            "std.sta module `{want}` should be auto-imported; have {module_names:?}"
        );
    }
    // The user's own module is merged in too.
    assert!(module_names.contains(&"M"));
}

#[test]
fn user_design_instantiating_a_std_module_elaborates_and_lowers() {
    // Instantiating `Gt` — a block defined purely in std.sta, with no compiler support — must
    // elaborate and build a netlist IR with no compiler change (Req 10.4, 10.5). This is the
    // "adding a library block requires no compiler change" guarantee: the block is just a module.
    let design = load_with_std(
        "module Top (input time a, input time b, output time y) {\n\
         \x20   inst g = Gt (.a(a), .b(b), .y(y));\n\
         }",
    );
    let checked = analyze(&design).unwrap_or_else(|e| {
        let r: Vec<String> = e.iter().map(|d| d.render()).collect();
        panic!("sema failed:\n{}", r.join("\n"))
    });
    let elab = quackc::elaborate::elaborate(&checked, "Top").unwrap_or_else(|e| {
        let r: Vec<String> = e.iter().map(|d| d.render()).collect();
        panic!("elaborate failed:\n{}", r.join("\n"))
    });
    // The instance boundary for `Gt` is present in the elaborated hierarchy.
    assert_eq!(elab.root.instances.len(), 1);
    assert_eq!(elab.root.instances[0].module.name, "Gt");

    // Lowering to IR succeeds (no panic / no missing node).
    let netlist = ir::build(&elab);
    assert!(
        !netlist.nodes.is_empty(),
        "the lowered netlist should contain nodes"
    );
}

// --- derived-op if/else meaning verification over sampled pairs including inf (Req 9.2-9.4) ------

/// The sampled operand domain: a spread of finite ticks plus `inf` (the never-fires sentinel), so
/// every `inf`-involving case of each operator is exercised.
const SAMPLE: [SpikeTime; 6] = [0, 1, 2, 3, 6, INF];

fn ref_le(a: SpikeTime, b: SpikeTime) -> SpikeTime { if a <= b { a } else { INF } }
fn ref_ge(a: SpikeTime, b: SpikeTime) -> SpikeTime { if a >= b { a } else { INF } }
fn ref_eq(a: SpikeTime, b: SpikeTime) -> SpikeTime { if a == b { a } else { INF } }
fn ref_gt(a: SpikeTime, b: SpikeTime) -> SpikeTime { if a > b { a } else { INF } }
fn ref_neq(a: SpikeTime, b: SpikeTime) -> SpikeTime { if a != b { a } else { INF } }
fn ref_max(a: SpikeTime, b: SpikeTime) -> SpikeTime { a.max(b) }

fn stim_lit(t: SpikeTime) -> String {
    if t == INF { "inf".to_string() } else { t.to_string() }
}

/// Compile `y = op(a, b)` through the std-library-backed frontend and evaluate it with the event
/// engine for one operand pair, returning the produced spike time for `y`.
fn eval_op(op: &str, a: SpikeTime, b: SpikeTime) -> SpikeTime {
    let src = format!(
        "module M (input time a, input time b, output time y) {{ y = {op}(a, b); }}"
    );
    let design = load_with_std(&src);
    let checked = analyze(&design).unwrap_or_else(|e| {
        let r: Vec<String> = e.iter().map(|d| d.render()).collect();
        panic!("sema failed for {op}:\n{}", r.join("\n"))
    });
    let elab = quackc::elaborate::elaborate(&checked, "M").unwrap_or_else(|e| {
        let r: Vec<String> = e.iter().map(|d| d.render()).collect();
        panic!("elaborate failed for {op}:\n{}", r.join("\n"))
    });
    let netlist = ir::build(&elab);

    let mut inputs: HashMap<String, SpikeTime> = HashMap::new();
    inputs.insert("a".to_string(), a);
    inputs.insert("b".to_string(), b);
    let out = sim_event::run(&netlist, &inputs, TimeWidth::W32, DEFAULT_MAX_TICK);
    let _ = (stim_lit(a), stim_lit(b)); // keep the literal helper exercised / documented
    out.output("y")
}

/// Sweep every ordered pair in [`SAMPLE`] through `op` compiled against std.sta and assert the
/// produced value equals the operator's if/else `reference` for every pair, inf included.
fn assert_op_matches(op: &str, reference: fn(SpikeTime, SpikeTime) -> SpikeTime) {
    let mut failures = Vec::new();
    for &a in &SAMPLE {
        for &b in &SAMPLE {
            let got = eval_op(op, a, b);
            let want = reference(a, b);
            if got != want {
                failures.push(format!("{op}({a}, {b}) = {got} but if/else meaning = {want}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "std.sta operator `{op}` disagrees with its if/else meaning for {} pair(s):\n{}",
        failures.len(),
        failures.join("\n"),
    );
}

#[test]
fn std_le_matches_if_else_meaning() { assert_op_matches("le", ref_le); }

#[test]
fn std_ge_matches_if_else_meaning() { assert_op_matches("ge", ref_ge); }

#[test]
fn std_eq_matches_if_else_meaning() { assert_op_matches("eq", ref_eq); }

#[test]
fn std_gt_matches_if_else_meaning() { assert_op_matches("gt", ref_gt); }

#[test]
fn std_neq_matches_if_else_meaning() { assert_op_matches("neq", ref_neq); }

#[test]
fn std_max_matches_if_else_meaning() { assert_op_matches("max", ref_max); }

#[test]
fn std_min_matches_first_arrival() {
    // `min` is the primitive first-arrival; its library form is the variadic `min(...)` call.
    assert_op_matches("min", |a, b| a.min(b));
}
