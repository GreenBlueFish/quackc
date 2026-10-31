//! Golden / snapshot parse tests for the Section 4 examples (Req 1.27, 1.28).
//!
//! Requirement 1.28 says each example module in Section 4 of the project prompt (Sort2, Wta3,
//! DelayN, MinN, Top) must parse into an AST with no reported errors, and Requirement 1.27 says any
//! source conforming to the grammar produces an AST with no reported errors. These tests embed each
//! example verbatim from the prompt and assert `parse_source(..)` returns `Ok`.
//!
//! Beyond the "no errors" bar, each test pins a small set of stable structural properties of the
//! parsed tree — the port count and shape, the number of body statements, the presence of a `param`
//! with its default, a `for`-generate loop, an `inst` statement, and so on. This is the golden /
//! snapshot flavour: a full-tree dump would be brittle (every span would churn), but these
//! structural invariants give a stable fingerprint of each example's shape that would break if the
//! parser silently started producing a different tree.
//!
//! `max` and `ge` are library (derived) operators. The parser does not need them defined: they
//! parse as generic `Call` expressions, and elaboration (a later task) handles their expansion. All
//! that matters here is that parsing reports no errors.

use quackc::ast::{CtExprKind, Design, Dir, Expr, LValueKind, Stmt, TyKind};
use quackc::lexer::TimeWidth;
use quackc::operators::OperatorTable;
use quackc::parser::parse_source;

/// Parse a Section 4 example with the builtin operator table at the default 32-bit width, asserting
/// a clean parse (Req 1.27, 1.28). On any diagnostic the rendered errors are shown so a regression
/// is easy to read.
fn parse_clean(name: &str, src: &str) -> Design {
    match parse_source(src, "section4.sta", &OperatorTable::builtin(), TimeWidth::W32) {
        Ok(design) => design,
        Err(diags) => {
            let rendered: Vec<String> = diags.iter().map(|d| d.render()).collect();
            panic!(
                "Section 4 example `{name}` should parse with no errors (Req 1.28), but got:\n{}",
                rendered.join("\n")
            );
        }
    }
}

/// The single module of a single-module design.
fn only_module(design: &Design) -> &quackc::ast::Module {
    assert_eq!(
        design.modules.len(),
        1,
        "expected exactly one module in the design"
    );
    assert!(design.funcs.is_empty(), "no funcs expected");
    assert!(design.tests.is_empty(), "no tests expected");
    &design.modules[0]
}

// 4.1 Sort2 — verbatim from the project prompt (including the inline comments).
const SORT2: &str = r#"module Sort2 (
    input  time a, input time b,
    output time lo, output time hi
) {
    lo = min(a, b);
    hi = max(a, b);          // library function, expands to
                             // min(ge(a,b), ge(b,a))
}"#;

// 4.2 Wta3.
const WTA3: &str = r#"module Wta3 (
    input  time x[3],
    output time y[3]
) {
    y[0] = x[0] < min(x[1], x[2]);
    y[1] = x[1] < min(x[0], x[2]);
    y[2] = x[2] < min(x[0], x[1]);
}"#;

// 4.3 DelayN.
const DELAYN: &str = r#"module DelayN #(param N = 3) (
    input time a, output time y
) {
    y = a + N;
}"#;

// 4.4 MinN.
const MINN: &str = r#"module MinN #(param N = 4) (
    input time x[N], output time y
) {
    wire time acc[N];
    acc[0] = x[0];
    for i in 1..N {
        acc[i] = min(acc[i - 1], x[i]);
    }
    y = acc[N - 1];
}"#;

// 4.5 Top.
const TOP: &str = r#"module Top (input time p, input time q, output time r) {
    wire time lo; wire time hi;
    inst s = Sort2 (.a(p), .b(q), .lo(lo), .hi(hi));
    r = hi + 2;
}"#;

/// 4.1 Sort2: four scalar `time` ports (two in, two out) and two assignment statements. `max` parses
/// as a generic call that elaboration later expands (Req 1.27, 1.28).
#[test]
fn sort2_parses_with_no_errors() {
    let design = parse_clean("Sort2", SORT2);
    let m = only_module(&design);

    assert_eq!(m.name, "Sort2");
    assert!(m.params.is_empty(), "Sort2 has no params");

    // Four scalar ports: a, b (in); lo, hi (out).
    assert_eq!(m.ports.len(), 4, "Sort2 has 4 ports");
    assert_eq!(m.ports[0].dir, Dir::Input);
    assert_eq!(m.ports[1].dir, Dir::Input);
    assert_eq!(m.ports[2].dir, Dir::Output);
    assert_eq!(m.ports[3].dir, Dir::Output);
    for p in &m.ports {
        assert!(matches!(p.ty.kind, TyKind::Time), "all Sort2 ports are scalar `time`");
    }
    assert_eq!(
        m.ports.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["a", "b", "lo", "hi"]
    );

    // Two body statements: lo = min(a, b);  hi = max(a, b);
    assert_eq!(m.body.len(), 2, "Sort2 has 2 body statements");
    match &m.body[0] {
        Stmt::Assign { value: Expr::Min(args, _), .. } => assert_eq!(args.len(), 2),
        other => panic!("expected `lo = min(a, b)`, got {other:?}"),
    }
    match &m.body[1] {
        // `max` is a library function; it parses as a generic Call (expanded during elaboration).
        Stmt::Assign { value: Expr::Call { name, args, .. }, .. } => {
            assert_eq!(name, "max");
            assert_eq!(args.len(), 2);
        }
        other => panic!("expected `hi = max(a, b)`, got {other:?}"),
    }
}

/// 4.2 Wta3: two `time[3]` vector ports and three indexed assignments using the `<` relational and
/// `min` (Req 1.27, 1.28).
#[test]
fn wta3_parses_with_no_errors() {
    let design = parse_clean("Wta3", WTA3);
    let m = only_module(&design);

    assert_eq!(m.name, "Wta3");
    assert!(m.params.is_empty());

    // Both ports are time[3].
    assert_eq!(m.ports.len(), 2, "Wta3 has 2 ports");
    assert_eq!(m.ports[0].dir, Dir::Input);
    assert_eq!(m.ports[1].dir, Dir::Output);
    for p in &m.ports {
        match &p.ty.kind {
            TyKind::TimeVec(n) => assert!(
                matches!(n.kind, CtExprKind::Int(3)),
                "Wta3 ports are time[3]"
            ),
            other => panic!("expected time[3], got {other:?}"),
        }
    }

    // Three indexed assignments: y[0], y[1], y[2].
    assert_eq!(m.body.len(), 3, "Wta3 has 3 body statements");
    for (i, stmt) in m.body.iter().enumerate() {
        match stmt {
            Stmt::Assign { target, value: Expr::Op { func, args, .. }, .. } => {
                assert!(
                    matches!(&target.kind, LValueKind::Index(n, _) if n == "y"),
                    "statement {i} assigns to y[_]"
                );
                assert_eq!(func, "lt", "`<` maps to the lt relational");
                assert_eq!(args.len(), 2);
                // RHS of the lt is a min(...) call.
                assert!(matches!(args[1], Expr::Min(_, _)), "rhs is a min");
            }
            other => panic!("expected an indexed `<` assignment, got {other:?}"),
        }
    }
}

/// 4.3 DelayN: a `param N` with default `3`, two scalar ports, and a single `y = a + N` delay
/// assignment (Req 1.27, 1.28).
#[test]
fn delayn_parses_with_no_errors() {
    let design = parse_clean("DelayN", DELAYN);
    let m = only_module(&design);

    assert_eq!(m.name, "DelayN");

    // One param N with default 3.
    assert_eq!(m.params.len(), 1, "DelayN has one param");
    assert_eq!(m.params[0].name, "N");
    match &m.params[0].default {
        Some(ce) => assert!(
            matches!(ce.kind, CtExprKind::Int(3)),
            "param N defaults to 3"
        ),
        None => panic!("expected param N to have a default of 3"),
    }

    // Two scalar ports: a (in), y (out).
    assert_eq!(m.ports.len(), 2);
    assert_eq!(m.ports[0].dir, Dir::Input);
    assert_eq!(m.ports[1].dir, Dir::Output);
    for p in &m.ports {
        assert!(matches!(p.ty.kind, TyKind::Time));
    }

    // Single delay assignment: y = a + N.
    assert_eq!(m.body.len(), 1, "DelayN has one body statement");
    match &m.body[0] {
        Stmt::Assign { value: Expr::Delay(_, amount, _), .. } => {
            assert!(
                matches!(&amount.kind, CtExprKind::Param(p) if p == "N"),
                "delay amount is the param N"
            );
        }
        other => panic!("expected `y = a + N` delay, got {other:?}"),
    }
}

/// 4.4 MinN: a `param N` (default 4), a `time[N]` input, a `wire time acc[N]`, and a `for`-generate
/// loop building the running minimum (Req 1.27, 1.28).
#[test]
fn minn_parses_with_no_errors() {
    let design = parse_clean("MinN", MINN);
    let m = only_module(&design);

    assert_eq!(m.name, "MinN");

    // Param N default 4.
    assert_eq!(m.params.len(), 1);
    assert_eq!(m.params[0].name, "N");
    assert!(matches!(
        m.params[0].default.as_ref().map(|c| &c.kind),
        Some(CtExprKind::Int(4))
    ));

    // Input x is time[N] (a param-sized vector); output y is scalar time.
    assert_eq!(m.ports.len(), 2);
    match &m.ports[0].ty.kind {
        TyKind::TimeVec(n) => assert!(matches!(&n.kind, CtExprKind::Param(p) if p == "N")),
        other => panic!("expected x: time[N], got {other:?}"),
    }
    assert!(matches!(m.ports[1].ty.kind, TyKind::Time));

    // Body: wire decl, acc[0] assign, a ForGen loop, and the final y assign.
    assert_eq!(m.body.len(), 4, "MinN has 4 body statements");
    assert!(
        matches!(&m.body[0], Stmt::WireDecl { ty, name, .. }
            if name == "acc" && matches!(ty.kind, TyKind::TimeVec(_))),
        "first statement is `wire time acc[N]`"
    );
    assert!(
        matches!(&m.body[1], Stmt::Assign { target, .. }
            if matches!(&target.kind, LValueKind::Index(n, _) if n == "acc")),
        "second statement is `acc[0] = x[0]`"
    );

    // The for-generate loop over 1..N with one body statement (Req 1.19).
    let forgen = m
        .body
        .iter()
        .find_map(|s| match s {
            Stmt::ForGen { var, lo, hi, body, .. } => Some((var, lo, hi, body)),
            _ => None,
        })
        .expect("MinN has a ForGen statement");
    assert_eq!(forgen.0, "i", "loop variable is i");
    assert!(matches!(forgen.1.kind, CtExprKind::Int(1)), "lo bound is 1");
    assert!(matches!(&forgen.2.kind, CtExprKind::Param(p) if p == "N"), "hi bound is N");
    assert_eq!(forgen.3.len(), 1, "loop body has one statement");

    // The final assignment targets y.
    assert!(
        matches!(m.body.last(), Some(Stmt::Assign { target, .. })
            if matches!(&target.kind, LValueKind::Wire(n) if n == "y")),
        "last statement is `y = acc[N - 1]`"
    );
}

/// 4.5 Top: three scalar ports, two scalar `wire` decls, a sub-module `inst` of Sort2 with four port
/// connections, and a final delay assignment (Req 1.27, 1.28).
#[test]
fn top_parses_with_no_errors() {
    let design = parse_clean("Top", TOP);
    let m = only_module(&design);

    assert_eq!(m.name, "Top");
    assert!(m.params.is_empty());

    // Three scalar ports: p, q (in); r (out).
    assert_eq!(m.ports.len(), 3);
    assert_eq!(m.ports[0].dir, Dir::Input);
    assert_eq!(m.ports[1].dir, Dir::Input);
    assert_eq!(m.ports[2].dir, Dir::Output);
    for p in &m.ports {
        assert!(matches!(p.ty.kind, TyKind::Time));
    }

    // Body: two wire decls, an inst, and a delay assign.
    assert_eq!(m.body.len(), 4, "Top has 4 body statements");

    // The inst statement wires Sort2 with four named connections (Req 1.20).
    let inst = m
        .body
        .iter()
        .find_map(|s| match s {
            Stmt::Inst { name, module, params, conns, .. } => Some((name, module, params, conns)),
            _ => None,
        })
        .expect("Top has an Inst statement");
    assert_eq!(inst.0, "s", "instance name is s");
    assert_eq!(inst.1, "Sort2", "instantiated module is Sort2");
    assert!(inst.2.is_empty(), "no param overrides");
    assert_eq!(inst.3.len(), 4, "four port connections");
    assert_eq!(
        inst.3.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        ["a", "b", "lo", "hi"]
    );

    // Final statement: r = hi + 2 (a delay).
    assert!(
        matches!(m.body.last(), Some(Stmt::Assign { value: Expr::Delay(_, amt, _), .. })
            if matches!(amt.kind, CtExprKind::Int(2))),
        "last statement is `r = hi + 2`"
    );
}

/// A combined smoke check: all five Section 4 examples parse with no reported errors (Req 1.28).
/// This mirrors the milestone bar ("parse all Section 4 examples") in one place.
#[test]
fn all_section4_examples_parse_without_error() {
    for (name, src) in [
        ("Sort2", SORT2),
        ("Wta3", WTA3),
        ("DelayN", DELAYN),
        ("MinN", MINN),
        ("Top", TOP),
    ] {
        let _ = parse_clean(name, src);
    }
}
