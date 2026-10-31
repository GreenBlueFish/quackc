//! AST type definitions; every node carries its source `Span` (Requirement 1.25).
//!
//! The parser produces an abstract syntax tree in which every node records where it came from as
//! file, line, and column (Req 1.25). The shapes here follow the design's Data Models section; the
//! only deliberate departures are where the design wrote a bare enum that still needs provenance:
//!
//! * **`CtExpr`** — the design shows a bare `enum CtExpr { Int, Param, BinOp }`. A compile-time
//!   expression is a real node (it appears in sizes, bounds, indices, delay amounts and `assert`
//!   conditions, each of which can be reported against — e.g. Req 2.7, 2.13, 7.7, 7.8). So we
//!   split it into a span-carrying struct [`CtExpr`] wrapping a [`CtExprKind`]. Every compile-time
//!   subexpression therefore has its own span, consistently.
//! * **`LValue`** and **`StimVal`** — likewise split into a span-carrying struct wrapping a
//!   `*Kind` enum, because both are independent nodes the later stages locate diagnostics against
//!   (e.g. an undriven assignment target, or a stimulus value).
//! * **`Ty`** — carries a span: `time[N]` is a type expression with a source location.
//! * **`Dir`** — stays a plain marker enum. It is not an independent node; it is a property of the
//!   [`Port`] that owns it, and the port already carries the span.
//!
//! All types derive `Clone, Debug, PartialEq` so later tasks can write snapshot / golden parse
//! tests over whole trees.

use crate::diag::Span;

/// A whole parsed design: the top-level translation unit (Req 1.25, 1.24).
#[derive(Clone, Debug, PartialEq)]
pub struct Design {
    pub modules: Vec<Module>,
    pub funcs: Vec<Func>,
    pub tests: Vec<Test>,
    pub imports: Vec<Import>,
    /// `include "path";` statements: like imports, but resolved RELATIVE to the including file's
    /// own directory (not the library search path), for multi-file user programs.
    pub includes: Vec<Import>,
}

/// An `import` statement bringing in another source file such as the standard library (Req 1.24).
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub path: String,
    pub span: Span,
}

/// A `module` declaration with parameters, ports, and a body of statements (Req 1.8).
#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    pub name: String,
    pub params: Vec<Param>,
    pub ports: Vec<Port>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

/// A compile-time integer `param` with an optional default (Req 1.16, 1.17).
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub default: Option<CtExpr>,
    pub span: Span,
}

/// An `input` or `output` port typed `time` or `time[N]` (Req 1.8).
#[derive(Clone, Debug, PartialEq)]
pub struct Port {
    pub dir: Dir,
    pub ty: Ty,
    pub name: String,
    pub span: Span,
}

/// Port direction. A plain marker owned by its [`Port`]; it carries no independent span.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    Input,
    Output,
}

/// A port / wire type: a scalar `time` or a vector `time[N]` where `N` resolves to `>= 1`.
#[derive(Clone, Debug, PartialEq)]
pub struct Ty {
    pub kind: TyKind,
    pub span: Span,
}

/// The scalar-vs-vector shape of a [`Ty`].
#[derive(Clone, Debug, PartialEq)]
pub enum TyKind {
    /// `time` — a single spike time.
    Time,
    /// `time[N]` — a vector of `N` spike times; `N` resolves to a compile-time integer `>= 1`.
    TimeVec(CtExpr),
}

impl Ty {
    /// A scalar `time` type at `span`.
    pub fn time(span: Span) -> Self {
        Ty {
            kind: TyKind::Time,
            span,
        }
    }

    /// A `time[n]` vector type at `span`.
    pub fn time_vec(n: CtExpr, span: Span) -> Self {
        Ty {
            kind: TyKind::TimeVec(n),
            span,
        }
    }
}

/// A statement in a module body (Req 1.9, 1.14, 1.19, 1.20, 1.23).
#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    /// `wire t x;` or `wire t x = <expr>;` — a declaration with an optional inline driver.
    WireDecl {
        ty: Ty,
        name: String,
        driver: Option<Expr>,
        span: Span,
    },
    /// `target = value;` — a standalone assignment.
    Assign {
        target: LValue,
        value: Expr,
        span: Span,
    },
    /// `inst Name = Module(params...)(conns...);` — a sub-module instantiation.
    Inst {
        name: String,
        module: String,
        params: Vec<(String, CtExpr)>,
        conns: Vec<(String, LValue)>,
        span: Span,
    },
    /// `for v in lo..hi { body }` — a compile-time generate loop (Req 1.19).
    ForGen {
        var: String,
        lo: CtExpr,
        hi: CtExpr,
        body: Vec<Stmt>,
        span: Span,
    },
    /// `if cond { then } else { else }` — a compile-time generate conditional (Req 1.20).
    IfGen {
        cond: CtExpr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
        span: Span,
    },
    /// `assert <compile-time-expr>;` (Req 1.23).
    Assert { cond: CtExpr, span: Span },
}

/// The target of an assignment or instance connection: a whole wire or one vector element.
#[derive(Clone, Debug, PartialEq)]
pub struct LValue {
    pub kind: LValueKind,
    pub span: Span,
}

/// The whole-wire-vs-element shape of an [`LValue`].
#[derive(Clone, Debug, PartialEq)]
pub enum LValueKind {
    /// A whole wire or port by name.
    Wire(String),
    /// `name[i]` — a single element of a vector wire, `i` compile-time (Req 1.10).
    Index(String, CtExpr),
}

impl LValue {
    /// A whole-wire l-value at `span`.
    pub fn wire(name: impl Into<String>, span: Span) -> Self {
        LValue {
            kind: LValueKind::Wire(name.into()),
            span,
        }
    }

    /// An element l-value `name[index]` at `span`.
    pub fn index(name: impl Into<String>, index: CtExpr, span: Span) -> Self {
        LValue {
            kind: LValueKind::Index(name.into(), index),
            span,
        }
    }
}

/// A spike-time-valued expression (Req 1.10). Every variant carries its own `Span` so diagnostics
/// can point at the exact subexpression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// A bare name referring to a wire or port.
    Ident(String, Span),
    /// `base[i]` — vector indexing, `i` compile-time (Req 1.10).
    Index(Box<Expr>, CtExpr, Span),
    /// The spike-time literal `inf` (never fires) (Req 1.10).
    Inf(Span),
    /// `inc(a)` — the increment primitive (Req 1.10).
    Inc(Box<Expr>, Span),
    /// `a + n` — delay by a compile-time amount `n`; the RHS is never a wire (Req 2.8).
    Delay(Box<Expr>, CtExpr, Span),
    /// `min(...)` — variadic minimum, always with `>= 2` arguments (Req 1.10, 1.11).
    Min(Vec<Expr>, Span),
    /// A comparison operator written as a symbol or call: `lt`/`gt`/`eq`/`neq` (Req 1.10, 1.13).
    Op {
        func: String,
        args: Vec<Expr>,
        span: Span,
    },
    /// A `func` or derived-operator call by name (Req 1.10).
    Call {
        name: String,
        args: Vec<Expr>,
        span: Span,
    },
}

impl Expr {
    /// The source span of this expression, regardless of variant.
    pub fn span(&self) -> &Span {
        match self {
            Expr::Ident(_, span)
            | Expr::Index(_, _, span)
            | Expr::Inf(span)
            | Expr::Inc(_, span)
            | Expr::Delay(_, _, span)
            | Expr::Min(_, span)
            | Expr::Op { span, .. }
            | Expr::Call { span, .. } => span,
        }
    }
}

/// A compile-time integer expression: literals, params, loop vars, and `+ - * / %` over them
/// (Req 2.13). Wrapped in a span-carrying struct so every compile-time subexpression is locatable.
#[derive(Clone, Debug, PartialEq)]
pub struct CtExpr {
    pub kind: CtExprKind,
    pub span: Span,
}

/// The shape of a [`CtExpr`].
#[derive(Clone, Debug, PartialEq)]
pub enum CtExprKind {
    /// A non-negative integer literal.
    Int(u64),
    /// A reference to a `param` or a `for`-generate loop variable (both compile-time).
    Param(String),
    /// A binary arithmetic operation over two compile-time subexpressions.
    BinOp(CtOp, Box<CtExpr>, Box<CtExpr>),
}

impl CtExpr {
    /// An integer-literal compile-time expression at `span`.
    pub fn int(value: u64, span: Span) -> Self {
        CtExpr {
            kind: CtExprKind::Int(value),
            span,
        }
    }

    /// A param / loop-variable reference at `span`.
    pub fn param(name: impl Into<String>, span: Span) -> Self {
        CtExpr {
            kind: CtExprKind::Param(name.into()),
            span,
        }
    }

    /// A binary compile-time operation at `span`.
    pub fn binop(op: CtOp, lhs: CtExpr, rhs: CtExpr, span: Span) -> Self {
        CtExpr {
            kind: CtExprKind::BinOp(op, Box::new(lhs), Box::new(rhs)),
            span,
        }
    }
}

/// The compile-time integer operators usable in a [`CtExpr`] (Req 2.13).
///
/// Arithmetic operators (`Add`..`Mod`) combine two integers into an integer. Comparison operators
/// (`Eq`..`Ge`) combine two integers into `1` (true) or `0` (false), so a comparison can be used
/// directly as an `if`/`assert` condition (nonzero = true) or anywhere else an integer is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CtOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// `==` — 1 if the operands are equal, else 0.
    Eq,
    /// `!=` — 1 if the operands differ, else 0.
    Ne,
    /// `<` — 1 if the left operand is strictly less, else 0.
    Lt,
    /// `<=` — 1 if the left operand is less or equal, else 0.
    Le,
    /// `>` — 1 if the left operand is strictly greater, else 0.
    Gt,
    /// `>=` — 1 if the left operand is greater or equal, else 0.
    Ge,
}

/// A `func`: a pure, single-output macro expanded during elaboration (Req 1.21).
#[derive(Clone, Debug, PartialEq)]
pub struct Func {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
    pub span: Span,
}

/// A `test` block exercising a module with one or more waves (Req 1.14).
#[derive(Clone, Debug, PartialEq)]
pub struct Test {
    pub name: String,
    pub module: String,
    /// Per-test output defaults from an optional single `default { .. }` block, used only by a
    /// wave's `expect default;`. An output not named here falls back to `inf` (Feature: test defaults).
    pub defaults: Vec<(String, StimVal)>,
    pub waves: Vec<Wave>,
    pub span: Span,
}

/// A single `wave`: a set of input stimuli and expected outputs (Req 1.14).
#[derive(Clone, Debug, PartialEq)]
pub struct Wave {
    pub inputs: Vec<(String, StimVal)>,
    pub expects: Vec<(String, StimVal)>,
    /// `true` when the wave wrote `expect default;`: every output NOT explicitly expected is then
    /// checked against the test's default (or `inf`). When `false`, unmentioned outputs are ignored.
    pub expect_default: bool,
    pub span: Span,
}

/// A stimulus / expectation value. Numeric literals are allowed here (unlike in wire expressions)
/// because test vectors are stimulus, not time-invariant hardware (Req 1.14).
#[derive(Clone, Debug, PartialEq)]
pub struct StimVal {
    pub kind: StimValKind,
    pub span: Span,
}

/// The scalar / `inf` / vector shape of a [`StimVal`].
#[derive(Clone, Debug, PartialEq)]
pub enum StimValKind {
    /// A concrete spike time.
    Time(u64),
    /// The `inf` spike time (never fires).
    Inf,
    /// A vector of stimulus values for a `time[N]` port.
    Vec(Vec<StimVal>),
}

impl StimVal {
    /// A concrete spike-time stimulus at `span`.
    pub fn time(value: u64, span: Span) -> Self {
        StimVal {
            kind: StimValKind::Time(value),
            span,
        }
    }

    /// An `inf` stimulus at `span`.
    pub fn inf(span: Span) -> Self {
        StimVal {
            kind: StimValKind::Inf,
            span,
        }
    }

    /// A vector stimulus at `span`.
    pub fn vector(values: Vec<StimVal>, span: Span) -> Self {
        StimVal {
            kind: StimValKind::Vec(values),
            span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(line: u32, col: u32) -> Span {
        Span::new("sort2.sta", line, col)
    }

    /// Build a representative `Sort2`-like module and confirm the AST types compose and that each
    /// node carries a span (Req 1.25):
    ///
    /// ```text
    /// module Sort2(input time a, input time b, output time lo, output time hi) {
    ///     lo = min(a, b);
    ///     hi = inc(a);        // stand-in payload exercising inc + delay
    /// }
    /// ```
    #[test]
    fn builds_a_sort2_like_module_with_spans() {
        let module = Module {
            name: "Sort2".into(),
            params: vec![],
            ports: vec![
                Port {
                    dir: Dir::Input,
                    ty: Ty::time(sp(1, 14)),
                    name: "a".into(),
                    span: sp(1, 14),
                },
                Port {
                    dir: Dir::Input,
                    ty: Ty::time(sp(1, 28)),
                    name: "b".into(),
                    span: sp(1, 28),
                },
                Port {
                    dir: Dir::Output,
                    ty: Ty::time(sp(1, 43)),
                    name: "lo".into(),
                    span: sp(1, 43),
                },
                Port {
                    dir: Dir::Output,
                    ty: Ty::time(sp(1, 59)),
                    name: "hi".into(),
                    span: sp(1, 59),
                },
            ],
            body: vec![
                Stmt::Assign {
                    target: LValue::wire("lo", sp(2, 5)),
                    value: Expr::Min(
                        vec![
                            Expr::Ident("a".into(), sp(2, 14)),
                            Expr::Ident("b".into(), sp(2, 17)),
                        ],
                        sp(2, 10),
                    ),
                    span: sp(2, 5),
                },
                Stmt::Assign {
                    target: LValue::wire("hi", sp(3, 5)),
                    value: Expr::Inc(Box::new(Expr::Ident("a".into(), sp(3, 14))), sp(3, 10)),
                    span: sp(3, 5),
                },
            ],
            span: sp(1, 1),
        };

        let design = Design {
            modules: vec![module],
            funcs: vec![],
            tests: vec![],
            imports: vec![Import {
                path: "std.sta".into(),
                span: sp(1, 1),
            }],
            includes: vec![],
        };

        // Structure composes as expected.
        assert_eq!(design.modules.len(), 1);
        let m = &design.modules[0];
        assert_eq!(m.name, "Sort2");
        assert_eq!(m.ports.len(), 4);
        assert_eq!(m.body.len(), 2);

        // Every node carries a span (Req 1.25) — spot-check across node kinds.
        assert_eq!(m.span, sp(1, 1));
        assert_eq!(m.ports[0].span, sp(1, 14));
        assert_eq!(design.imports[0].span, sp(1, 1));

        match &m.body[0] {
            Stmt::Assign { value, target, .. } => {
                assert_eq!(target.span, sp(2, 5));
                // The min expression and its arguments each have a span.
                assert_eq!(value.span(), &sp(2, 10));
                if let Expr::Min(args, _) = value {
                    assert_eq!(args.len(), 2);
                    assert_eq!(args[0].span(), &sp(2, 14));
                    assert_eq!(args[1].span(), &sp(2, 17));
                } else {
                    panic!("expected min expression");
                }
            }
            other => panic!("expected assign, got {other:?}"),
        }
    }

    /// Confirm compile-time expressions, delay, indexing, and test waves compose and carry spans.
    #[test]
    fn builds_ctexpr_delay_index_and_waves_with_spans() {
        // time[4] type: the vector size is a compile-time expression with its own span.
        let ty = Ty::time_vec(CtExpr::int(4, sp(1, 20)), sp(1, 14));
        match &ty.kind {
            TyKind::TimeVec(n) => assert_eq!(n.span, sp(1, 20)),
            _ => panic!("expected time[N]"),
        }

        // Delay `x[i] + (p * 2)`: nested CtExpr plus an indexed base.
        let delay = Expr::Delay(
            Box::new(Expr::Index(
                Box::new(Expr::Ident("x".into(), sp(2, 5))),
                CtExpr::param("i", sp(2, 7)),
                sp(2, 5),
            )),
            CtExpr::binop(
                CtOp::Mul,
                CtExpr::param("p", sp(2, 12)),
                CtExpr::int(2, sp(2, 16)),
                sp(2, 12),
            ),
            sp(2, 10),
        );
        assert_eq!(delay.span(), &sp(2, 10));

        // A test wave with scalar, inf, and vector stimulus values.
        let test = Test {
            name: "t_sort2".into(),
            module: "Sort2".into(),
            defaults: vec![],
            waves: vec![Wave {
                inputs: vec![
                    ("a".into(), StimVal::time(3, sp(5, 20))),
                    ("b".into(), StimVal::inf(sp(5, 30))),
                ],
                expects: vec![(
                    "v".into(),
                    StimVal::vector(
                        vec![StimVal::time(1, sp(6, 20)), StimVal::time(2, sp(6, 23))],
                        sp(6, 19),
                    ),
                )],
                expect_default: false,
                span: sp(5, 5),
            }],
            span: sp(4, 1),
        };

        assert_eq!(test.waves.len(), 1);
        let wave = &test.waves[0];
        assert_eq!(wave.inputs.len(), 2);
        assert_eq!(wave.inputs[1].1.span, sp(5, 30));
        match &wave.expects[0].1.kind {
            StimValKind::Vec(vs) => assert_eq!(vs.len(), 2),
            _ => panic!("expected vector stimulus"),
        }

        // A func whose body is an expression, and the CtOp enum is usable.
        let func = Func {
            name: "double".into(),
            params: vec!["x".into()],
            body: Expr::Inc(Box::new(Expr::Ident("x".into(), sp(8, 10))), sp(8, 5)),
            span: sp(8, 1),
        };
        assert_eq!(func.params, vec!["x".to_string()]);
        assert_eq!(func.body.span(), &sp(8, 5));
    }
}
