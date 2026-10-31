//! Semantic analysis: name/type/driver legality checks in a single pass (Requirement 2).
//!
//! `sema` walks a parsed [`ast::Design`] and collects **every** diagnostic it can find in one pass
//! (Req 2.17) rather than stopping at the first error. On a clean run it returns a
//! [`CheckedDesign`] that later stages (`elaborate`) consume; if *any* error was collected it
//! returns the full list of diagnostics and no checked design (Req 2.16). Warnings never stop
//! compilation, so a design with only warnings still produces a [`CheckedDesign`] (Req 2.14, 2.15).
//!
//! Checks implemented here (per module, then design-wide):
//!
//! * **Undeclared / duplicate names** — a name used but not declared is an error (Req 2.1); a name
//!   declared twice in the same scope is an error (Req 2.2).
//! * **Type / vector-size mismatch** — mixing `time` and `time[N]` incompatibly, or connecting
//!   mismatched vector widths, is an error (Req 2.3).
//! * **Driver legality** — every wire and output port must be driven exactly once: more than one
//!   driver is a multiple-driver error (Req 2.4), none is an undriven-signal error (Req 2.5, 2.11).
//! * **Numeric literal / compile-time constant as a wire value** — using a compile-time integer
//!   (for example a `param`) where a spike-time wire value is expected is an error whose note
//!   explains that a numeric constant would violate time-invariance (Req 2.6, 13.4).
//! * **Non-compile-time delay / bound / index** — a delay amount, loop bound, vector size, or index
//!   that is actually a wire rather than a compile-time integer is an error (Req 2.7, 2.8, 2.13).
//! * **Out-of-range index** — a compile-time index outside `0..N-1` is an error (Req 2.10).
//! * **Compile-time `assert`** — a `assert` whose condition evaluates to a constant zero is a
//!   failed-assertion error (Req 2.12).
//! * **Warnings** — a wire that is declared but never read emits an unused-wire warning (Req 2.14,
//!   13.6); an instance output port left unconnected emits an unconnected-output warning (Req 2.15,
//!   13.7). Neither stops compilation.
//!
//! Recursive instantiation (Req 2.9) and index ranges that only become known after parameter
//! substitution (Req 3.8) are left to `elaborate`, as recorded in the design's error-class table.
//! Here we range-check only indices whose vector size and index both resolve to a concrete
//! compile-time constant from literals and `param` defaults.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::{Diagnostic, Diagnostics, Span};

/// A design that has passed semantic analysis with no errors (Req 2.16).
///
/// It owns the validated AST so later stages (`elaborate`) consume a `&CheckedDesign` as their
/// input (see the design's module table). Warnings may still have been emitted during analysis;
/// they are carried alongside so a driver can render them without re-running analysis.
#[derive(Clone, Debug, PartialEq)]
pub struct CheckedDesign {
    /// The validated design.
    pub design: Design,
    /// Warnings collected during analysis (unused wires, unconnected outputs). Never errors.
    pub warnings: Vec<Diagnostic>,
}

impl CheckedDesign {
    /// Borrow the validated design.
    pub fn design(&self) -> &Design {
        &self.design
    }

    /// The warnings collected during analysis (Req 2.14, 2.15).
    pub fn warnings(&self) -> &[Diagnostic] {
        &self.warnings
    }
}

/// Run semantic analysis over a parsed design (Requirement 2).
///
/// Collects every diagnostic in one pass (Req 2.17). Returns the [`CheckedDesign`] when no *error*
/// was found (warnings are allowed and carried along), otherwise the full diagnostic list with no
/// checked design (Req 2.16).
pub fn analyze(design: &Design) -> Result<CheckedDesign, Vec<Diagnostic>> {
    let mut diags = Diagnostics::new();

    // A design-wide view of module names, so instance connections can be checked against the
    // instantiated module's ports (Req 2.15) and duplicate module names reported (Req 2.2).
    let module_table = ModuleTable::build(design, &mut diags);

    // Expected argument counts for callable names: the built-in operators/primitives plus every
    // user `func`. `min` is variadic (>= 2) and is represented by `None`; everything else has a
    // fixed arity. Used to catch arity-mismatch calls like `max(a, b, c)` (max is 2-ary).
    let arity = build_arity_table(design);

    for module in &design.modules {
        let mut analyzer = ModuleAnalyzer::new(module, &module_table, &arity, &mut diags);
        analyzer.run();
    }

    // Validate testbench (`test` / `cases`) stimulus and expectation vector shapes against the
    // bound module's ports (at its default parameters). A length/shape mismatch is a static error.
    check_testbench_shapes(design, &module_table, &mut diags);

    if diags.has_errors() {
        Err(diags.into_vec())
    } else {
        // Only warnings (if any) remain.
        let warnings = diags.into_vec();
        Ok(CheckedDesign {
            design: design.clone(),
            warnings,
        })
    }
}

/// Validate every `test` / `cases` block's stimulus and expectation vectors against the bound
/// module's port shapes (resolved at the module's DEFAULT parameters, since a testbench binds
/// defaults). Reports, with the binding's source span:
///   * a binding to a name that is not a port of the module (or wrong direction),
///   * a scalar value given for a vector port (or vice versa),
///   * a vector whose length does not match the port width.
/// These are static testbench errors — the runner would otherwise silently pad/truncate.
fn check_testbench_shapes(design: &Design, table: &ModuleTable, diags: &mut Diagnostics) {
    for test in &design.tests {
        let ports = match table.get(&test.module) {
            Some(p) => p,
            None => {
                diags.push(Diagnostic::error(
                    test.span.clone(),
                    format!("test `{}` binds unknown module `{}`", test.name, test.module),
                ));
                continue;
            }
        };
        let resolved = ports.ports_at_defaults();

        // `default { .. }` bindings target outputs.
        for (name, val) in &test.defaults {
            check_stim_shape(&test.module, name, val, Dir::Output, &resolved, diags);
        }
        for (wi, wave) in test.waves.iter().enumerate() {
            let _ = wi;
            for (name, val) in &wave.inputs {
                check_stim_shape(&test.module, name, val, Dir::Input, &resolved, diags);
            }
            for (name, val) in &wave.expects {
                check_stim_shape(&test.module, name, val, Dir::Output, &resolved, diags);
            }
        }
    }
}

/// Check one `name = value` stimulus/expect binding against the bound module's ports. `want_dir`
/// is the direction the binding implies (inputs are `Input`; `expect`/`default` are `Output`).
fn check_stim_shape(
    module: &str,
    name: &str,
    val: &StimVal,
    want_dir: Dir,
    ports: &[(String, Dir, SigTy)],
    diags: &mut Diagnostics,
) {
    let port = ports.iter().find(|(n, _, _)| n == name);
    let (dir, ty) = match port {
        Some((_, d, t)) => (*d, *t),
        None => {
            diags.push(Diagnostic::error(
                val.span.clone(),
                format!("module `{module}` has no port named `{name}`"),
            ));
            return;
        }
    };
    let role = match want_dir {
        Dir::Input => "input",
        Dir::Output => "output",
    };
    if dir != want_dir {
        diags.push(Diagnostic::error(
            val.span.clone(),
            format!("`{name}` is not an {role} port of `{module}`"),
        ));
        return;
    }
    // Shape: scalar port wants a scalar value; a time[W] port wants a length-W vector.
    match ty {
        SigTy::Scalar => {
            if let StimValKind::Vec(_) = &val.kind {
                diags.push(
                    Diagnostic::error(
                        val.span.clone(),
                        format!("port `{name}` of `{module}` is scalar `time` but a vector value was given"),
                    )
                    .with_note("a scalar port takes a single `time`/`inf` value, not a `[...]` vector"),
                );
            }
        }
        SigTy::Vector(w) => match &val.kind {
            StimValKind::Vec(elems) => {
                if elems.len() as u64 != w {
                    diags.push(
                        Diagnostic::error(
                            val.span.clone(),
                            format!(
                                "port `{name}` of `{module}` is `time[{w}]` but the value has {} element(s)",
                                elems.len()
                            ),
                        )
                        .with_note("a vector stimulus/expectation must have exactly one value per element"),
                    );
                }
            }
            _ => {
                diags.push(
                    Diagnostic::error(
                        val.span.clone(),
                        format!("port `{name}` of `{module}` is `time[{w}]` but a scalar value was given"),
                    )
                    .with_note("a vector port takes a `[...]` vector of the matching length"),
                );
            }
        },
        SigTy::VectorUnknown => {
            // Width not statically known (parameterized with no concrete default); skip the length
            // check and leave it to runtime, but still reject an obvious scalar-vs-vector mismatch.
            if !matches!(&val.kind, StimValKind::Vec(_)) {
                diags.push(Diagnostic::error(
                    val.span.clone(),
                    format!("port `{name}` of `{module}` is a vector but a scalar value was given"),
                ));
            }
        }
    }
}

/// Expected argument count for a callable name. `Fixed(n)` means exactly `n`; `Variadic` means
/// two or more (only `min`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arity {
    Fixed(usize),
    Variadic,
}

/// Build the name -> [`Arity`] table: built-in primitives/operators plus every user `func`.
/// A user `func` with the same name as a built-in shadows it (the user definition wins), matching
/// the elaborator, which resolves user funcs in `inline_func` only for names it does not special-case.
fn build_arity_table(design: &Design) -> HashMap<String, Arity> {
    let mut m: HashMap<String, Arity> = HashMap::new();

    // User funcs first. Note the elaborator (`expand_named`) special-cases the built-in operator
    // names BEFORE it ever looks up a user `func`, so a user definition that happens to share a
    // built-in name (e.g. the library's `func max`) never actually runs. The arity table must match
    // that resolution order, so built-ins are inserted AFTER user funcs and therefore win.
    for f in &design.funcs {
        m.insert(f.name.clone(), Arity::Fixed(f.params.len()));
    }

    // Built-ins (see elaborate::expand_named), inserted last so they override any same-named func:
    // all binary except `inc` (unary) and `min`/`max` (variadic, >= 2).
    m.insert("inc".into(), Arity::Fixed(1));
    m.insert("min".into(), Arity::Variadic);
    m.insert("max".into(), Arity::Variadic);
    for binop in ["lt", "gt", "le", "ge", "eq", "neq"] {
        m.insert(binop.into(), Arity::Fixed(2));
    }
    m
}

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

/// The resolved shape of a signal: scalar `time` or a vector `time[N]`. `None` width means the size
/// is a compile-time expression that is not a concrete constant yet (resolved during elaboration).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SigTy {
    /// `time` — a single spike time.
    Scalar,
    /// `time[N]` with a concrete, known width.
    Vector(u64),
    /// `time[expr]` whose width is compile-time but not a concrete constant here.
    VectorUnknown,
}

impl SigTy {
    fn describe(self) -> String {
        match self {
            SigTy::Scalar => "time".to_string(),
            SigTy::Vector(n) => format!("time[{n}]"),
            SigTy::VectorUnknown => "time[N]".to_string(),
        }
    }

    /// Whether two signal types are compatible for assignment / connection (Req 2.3). A concrete
    /// width must match exactly; an unknown width is compatible with any vector (checked later).
    fn compatible_with(self, other: SigTy) -> bool {
        match (self, other) {
            (SigTy::Scalar, SigTy::Scalar) => true,
            (SigTy::Vector(a), SigTy::Vector(b)) => a == b,
            (SigTy::Vector(_), SigTy::VectorUnknown)
            | (SigTy::VectorUnknown, SigTy::Vector(_))
            | (SigTy::VectorUnknown, SigTy::VectorUnknown) => true,
            _ => false,
        }
    }

    fn is_vector(self) -> bool {
        matches!(self, SigTy::Vector(_) | SigTy::VectorUnknown)
    }
}

/// What kind of name a declared symbol is, for name resolution and value-vs-constant checks.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SymKind {
    /// An `input` port (readable; drives itself).
    InputPort(SigTy),
    /// An `output` port (must be driven exactly once).
    OutputPort(SigTy),
    /// An internal `wire`.
    Wire(SigTy),
    /// A compile-time integer `param` (never a wire value, Req 2.6).
    Param,
    /// A sub-module instance name.
    Instance,
}

impl SymKind {
    /// The signal type if this symbol carries spike-time values (ports and wires).
    fn sig_ty(&self) -> Option<SigTy> {
        match self {
            SymKind::InputPort(t) | SymKind::OutputPort(t) | SymKind::Wire(t) => Some(*t),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Module table (design-wide)
// ---------------------------------------------------------------------------------------------

/// A design-wide map of module name -> its ports, used to validate instance connections (Req 2.15)
/// and to report duplicate module declarations (Req 2.2).
struct ModuleTable {
    modules: HashMap<String, ModulePorts>,
}

/// The ports and parameters of one module, kept in their RAW (unresolved) form so an instance can
/// resolve each port's vector size against that instance's own parameter overrides rather than the
/// module's defaults (fixes per-instance parameterized port typing).
struct ModulePorts {
    /// Each port's `(name, direction, raw type)`; the type's vector size is an unresolved `CtExpr`.
    ports: Vec<(String, Dir, Ty)>,
    /// The module's own `param` declarations (name + optional default), used to build the parameter
    /// environment for an instance (defaults overlaid with the instance's overrides).
    params: Vec<Param>,
}

impl ModulePorts {
    /// The module's default parameter environment (defaults only, no overrides).
    fn default_env(&self) -> ParamEnv {
        self.instance_env(&HashMap::new())
    }

    /// Every port resolved at the module's DEFAULT parameters, as `(name, dir, SigTy)`. Used to
    /// validate testbench stimulus/expectation shapes, since a `test`/`cases` binds a module at its
    /// defaults.
    fn ports_at_defaults(&self) -> Vec<(String, Dir, SigTy)> {
        let env = self.default_env();
        self.ports
            .iter()
            .map(|(n, d, ty)| (n.clone(), *d, resolve_port_ty(ty, &env)))
            .collect()
    }

    /// Build the parameter environment for one instantiation: start from this module's own defaults
    /// and overlay the `overrides` (param name -> already-evaluated value). An override value that
    /// could not be resolved to a constant is simply omitted (its param stays unknown).
    fn instance_env(&self, overrides: &HashMap<String, u64>) -> ParamEnv {
        let mut values = HashMap::new();
        // Defaults first, each resolved against the values accumulated so far (defaults may refer
        // to earlier params).
        for p in &self.params {
            if let Some(def) = &p.default {
                if let Some(v) = eval_const(def, &ParamEnv { values: values.clone() }) {
                    values.insert(p.name.clone(), v);
                }
            }
        }
        // Overrides win over defaults.
        for (name, v) in overrides {
            values.insert(name.clone(), *v);
        }
        ParamEnv { values }
    }

}

impl ModuleTable {
    fn build(design: &Design, diags: &mut Diagnostics) -> Self {
        let mut modules: HashMap<String, ModulePorts> = HashMap::new();
        for module in &design.modules {
            if modules.contains_key(&module.name) {
                diags.push(
                    Diagnostic::error(
                        module.span.clone(),
                        format!("duplicate declaration: module `{}` is already declared", module.name),
                    )
                    .with_note("each module must be declared once in the design"),
                );
                continue;
            }
            let ports = module
                .ports
                .iter()
                .map(|p| (p.name.clone(), p.dir, p.ty.clone()))
                .collect();
            modules.insert(
                module.name.clone(),
                ModulePorts {
                    ports,
                    params: module.params.clone(),
                },
            );
        }
        ModuleTable { modules }
    }

    fn get(&self, name: &str) -> Option<&ModulePorts> {
        self.modules.get(name)
    }
}

// ---------------------------------------------------------------------------------------------
// Compile-time evaluation of CtExpr using param defaults / loop variables
// ---------------------------------------------------------------------------------------------

/// A compile-time integer environment: `param` defaults and in-scope `for`-loop variables, used to
/// range-check sizes and indices where they resolve to concrete constants (Req 2.10, 2.13).
#[derive(Clone, Default)]
struct ParamEnv {
    values: HashMap<String, u64>,
}

impl ParamEnv {
    /// Build an environment seeded with the module's `param` *defaults* (where present). Params
    /// without a default stay unknown, so sizes/indices depending on them are range-checked at
    /// elaboration instead.
    fn for_module(module: &Module) -> Self {
        let mut values = HashMap::new();
        for p in &module.params {
            if let Some(def) = &p.default {
                if let Some(v) = eval_const(def, &ParamEnv { values: values.clone() }) {
                    values.insert(p.name.clone(), v);
                }
            }
        }
        ParamEnv { values }
    }

    fn with(&self, name: &str, value: u64) -> Self {
        let mut values = self.values.clone();
        values.insert(name.to_string(), value);
        ParamEnv { values }
    }

    fn get(&self, name: &str) -> Option<u64> {
        self.values.get(name).copied()
    }
}

/// Evaluate a [`CtExpr`] to a concrete constant using the given environment, or `None` if it refers
/// to an unknown param / loop variable. Division or modulo by zero yields `None` (left to the
/// deeper elaboration pass to diagnose with full context).
fn eval_const(expr: &CtExpr, env: &ParamEnv) -> Option<u64> {
    match &expr.kind {
        CtExprKind::Int(v) => Some(*v),
        CtExprKind::Param(name) => env.get(name),
        CtExprKind::BinOp(op, lhs, rhs) => {
            let a = eval_const(lhs, env)?;
            let b = eval_const(rhs, env)?;
            match op {
                CtOp::Add => a.checked_add(b),
                CtOp::Sub => a.checked_sub(b),
                CtOp::Mul => a.checked_mul(b),
                CtOp::Div => a.checked_div(b),
                CtOp::Mod => a.checked_rem(b),
                // Comparisons yield 1 (true) / 0 (false).
                CtOp::Eq => Some((a == b) as u64),
                CtOp::Ne => Some((a != b) as u64),
                CtOp::Lt => Some((a < b) as u64),
                CtOp::Le => Some((a <= b) as u64),
                CtOp::Gt => Some((a > b) as u64),
                CtOp::Ge => Some((a >= b) as u64),
            }
        }
    }
}

/// Resolve a declared [`Ty`] to a [`SigTy`], evaluating a vector size against the param env.
fn resolve_port_ty(ty: &Ty, env: &ParamEnv) -> SigTy {
    match &ty.kind {
        TyKind::Time => SigTy::Scalar,
        TyKind::TimeVec(n) => match eval_const(n, env) {
            Some(w) => SigTy::Vector(w),
            None => SigTy::VectorUnknown,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Per-module analysis
// ---------------------------------------------------------------------------------------------

struct ModuleAnalyzer<'a> {
    module: &'a Module,
    module_table: &'a ModuleTable,
    arity: &'a HashMap<String, Arity>,
    diags: &'a mut Diagnostics,
    /// Name -> declaration kind for this module's scope.
    symbols: HashMap<String, SymKind>,
    /// Declaration spans, for duplicate / unused reporting.
    decl_spans: HashMap<String, Span>,
    /// Compile-time environment (param defaults; loop vars pushed while walking generate loops).
    param_env: ParamEnv,
    /// Whole-signal driver count per driven signal (non-indexed assignments / connections), by
    /// name. A vector driven as a whole counts here; element drivers are tracked separately.
    driver_count: HashMap<String, u32>,
    /// Count of element (`name[i] = ...`) drivers per signal. For a vector these are legal and may
    /// be many; the exact per-element exactly-once check is deferred to elaboration once loops are
    /// unrolled and indices resolved.
    element_driver_count: HashMap<String, u32>,
    /// Names of wires/ports that have been read somewhere (for unused-wire, Req 2.14).
    read_names: std::collections::HashSet<String>,
    /// Declaration order of wires, so unused-wire warnings are emitted deterministically.
    wire_order: Vec<String>,
}

impl<'a> ModuleAnalyzer<'a> {
    fn new(
        module: &'a Module,
        module_table: &'a ModuleTable,
        arity: &'a HashMap<String, Arity>,
        diags: &'a mut Diagnostics,
    ) -> Self {
        ModuleAnalyzer {
            module,
            module_table,
            arity,
            diags,
            symbols: HashMap::new(),
            decl_spans: HashMap::new(),
            param_env: ParamEnv::for_module(module),
            driver_count: HashMap::new(),
            element_driver_count: HashMap::new(),
            read_names: std::collections::HashSet::new(),
            wire_order: Vec::new(),
        }
    }

    fn run(&mut self) {
        self.collect_params();
        self.collect_ports();
        self.collect_wires_and_instances(&self.module.body.clone());

        // Walk the body for value-level checks (reads, drivers, delays, indices, asserts).
        let body = self.module.body.clone();
        self.walk_stmts(&body);

        self.check_drivers();
        self.check_unused_wires();
    }

    // ---- declaration collection -------------------------------------------------------------

    fn declare(&mut self, name: &str, kind: SymKind, span: &Span) {
        if self.symbols.contains_key(name) {
            self.diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!("duplicate declaration: `{name}` is already declared in this module"),
                )
                .with_note("each name may be declared only once in the same scope"),
            );
            return;
        }
        self.symbols.insert(name.to_string(), kind);
        self.decl_spans.insert(name.to_string(), span.clone());
    }

    fn collect_params(&mut self) {
        for p in &self.module.params.clone() {
            self.declare(&p.name, SymKind::Param, &p.span);
        }
    }

    fn collect_ports(&mut self) {
        for port in &self.module.ports.clone() {
            let ty = resolve_port_ty(&port.ty, &self.param_env);
            let kind = match port.dir {
                Dir::Input => SymKind::InputPort(ty),
                Dir::Output => SymKind::OutputPort(ty),
            };
            self.declare(&port.name, kind, &port.span);
        }
    }

    /// Collect wire and instance declarations, including those nested inside generate blocks (they
    /// share the module scope since the loops are compile-time unrolled). Duplicate names across
    /// nested blocks are still duplicates in the same module scope (Req 2.2).
    fn collect_wires_and_instances(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::WireDecl { ty, name, span, .. } => {
                    let sig = resolve_port_ty(ty, &self.param_env);
                    self.declare(name, SymKind::Wire(sig), span);
                    self.wire_order.push(name.clone());
                }
                Stmt::Inst { name, span, .. } => {
                    self.declare(name, SymKind::Instance, span);
                }
                Stmt::ForGen { body, .. } => {
                    self.collect_wires_and_instances(body);
                }
                Stmt::IfGen {
                    cond,
                    then_body,
                    else_body,
                    ..
                } => {
                    // Mirror elaboration: when the condition is a known compile-time constant, only
                    // the taken branch contributes structure. This lets an `if`-generate with an
                    // `else` have both branches drive the same output (only one is actually taken).
                    match eval_const(cond, &self.param_env) {
                        Some(v) if v != 0 => self.collect_wires_and_instances(then_body),
                        Some(_) => self.collect_wires_and_instances(else_body),
                        None => {
                            // Condition not resolvable here (depends on an unbound param): be
                            // conservative and collect both so names still resolve.
                            self.collect_wires_and_instances(then_body);
                            self.collect_wires_and_instances(else_body);
                        }
                    }
                }
                Stmt::Assign { .. } | Stmt::Assert { .. } => {}
            }
        }
    }

    // ---- statement walk (value-level checks) ------------------------------------------------

    fn walk_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.walk_stmt(stmt);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::WireDecl { name, driver, .. } => {
                if let Some(expr) = driver {
                    let decl_ty = self
                        .symbols
                        .get(name)
                        .and_then(SymKind::sig_ty)
                        .unwrap_or(SigTy::Scalar);
                    let span = expr.span().clone();
                    let value_ty = self.check_expr(expr);
                    self.record_driver(name, 1);
                    self.check_assign_types(decl_ty, value_ty, &span);
                }
            }
            Stmt::Assign { target, value, .. } => {
                let target_ty = self.check_lvalue(target);
                let span = value.span().clone();
                let value_ty = self.check_expr(value);
                self.check_assign_types(target_ty, value_ty, &span);
            }
            Stmt::Inst {
                module,
                params,
                conns,
                span,
                ..
            } => {
                self.check_instance(module, params, conns, span);
            }
            Stmt::ForGen {
                var,
                lo,
                hi,
                body,
                span,
            } => {
                self.check_ct_bound(lo, "the lower bound of a `for` loop");
                self.check_ct_bound(hi, "the upper bound of a `for` loop");
                // Introduce the loop variable as a compile-time binding while walking the body. If
                // the bounds are concrete constants, bind the variable to the low bound so indices
                // inside the loop have *some* concrete value to range-check against; otherwise mark
                // it unknown.
                let saved = self.param_env.clone();
                if let Some(lo_v) = eval_const(lo, &self.param_env) {
                    self.param_env = self.param_env.with(var, lo_v);
                }
                // Declaring the loop var as a param so name resolution sees it (Req 2.1). It is a
                // compile-time integer, so using it as a wire value is caught like any param.
                let had = self.symbols.insert(var.clone(), SymKind::Param);
                self.decl_spans.entry(var.clone()).or_insert(span.clone());
                self.walk_stmts(body);
                // Restore scope.
                match had {
                    Some(k) => {
                        self.symbols.insert(var.clone(), k);
                    }
                    None => {
                        self.symbols.remove(var);
                    }
                }
                self.param_env = saved;
            }
            Stmt::IfGen {
                cond,
                then_body,
                else_body,
                ..
            } => {
                self.check_ct_bound(cond, "the condition of a compile-time `if`");
                // Only walk the taken branch when the condition is a known constant, so the untaken
                // branch's assignments do not count as extra drivers (matching elaboration). An
                // `if`-generate is true when its integer condition is nonzero.
                match eval_const(cond, &self.param_env) {
                    Some(v) if v != 0 => self.walk_stmts(then_body),
                    Some(_) => self.walk_stmts(else_body),
                    None => {
                        // Unresolved condition: walk both so reads/indices are still checked.
                        self.walk_stmts(then_body);
                        self.walk_stmts(else_body);
                    }
                }
            }
            Stmt::Assert { cond, span } => {
                self.check_assert(cond, span);
            }
        }
    }

    /// Record a whole-signal driver (a non-indexed assignment or connection) for `name`.
    fn record_driver(&mut self, name: &str, count: u32) {
        *self.driver_count.entry(name.to_string()).or_insert(0) += count;
    }

    /// Record an element driver (`name[i] = ...`) for `name`. Element drivers of a vector are
    /// legal and may be many; the per-element exactly-once check happens during elaboration.
    fn record_element_driver(&mut self, name: &str) {
        *self
            .element_driver_count
            .entry(name.to_string())
            .or_insert(0) += 1;
    }

    // ---- l-values ---------------------------------------------------------------------------

    /// Check an assignment target: resolve the name, verify it is a drivable signal, range-check an
    /// element index, and record the driver. Returns the l-value's signal type for compatibility
    /// checking.
    fn check_lvalue(&mut self, lv: &LValue) -> SigTy {
        match &lv.kind {
            LValueKind::Wire(name) => {
                match self.symbols.get(name).cloned() {
                    None => {
                        self.undeclared(name, &lv.span);
                        SigTy::Scalar
                    }
                    Some(kind) => {
                        match &kind {
                            SymKind::Wire(t) | SymKind::OutputPort(t) => {
                                self.record_driver(name, 1);
                                *t
                            }
                            SymKind::InputPort(_) => {
                                self.diags.push(Diagnostic::error(
                                    lv.span.clone(),
                                    format!("cannot assign to input port `{name}`"),
                                ));
                                SigTy::Scalar
                            }
                            SymKind::Param => {
                                self.diags.push(Diagnostic::error(
                                    lv.span.clone(),
                                    format!("cannot assign to compile-time parameter `{name}`"),
                                ));
                                SigTy::Scalar
                            }
                            SymKind::Instance => {
                                self.diags.push(Diagnostic::error(
                                    lv.span.clone(),
                                    format!("cannot assign to instance `{name}`"),
                                ));
                                SigTy::Scalar
                            }
                        }
                    }
                }
            }
            LValueKind::Index(name, idx) => {
                let base = self.symbols.get(name).cloned();
                match base {
                    None => {
                        self.undeclared(name, &lv.span);
                    }
                    Some(SymKind::Wire(t)) | Some(SymKind::OutputPort(t)) => {
                        if !t.is_vector() {
                            self.diags.push(Diagnostic::error(
                                lv.span.clone(),
                                format!("`{name}` is a scalar `time` and cannot be indexed"),
                            ));
                        } else {
                            self.check_index_range(name, t, idx, &lv.span);
                        }
                        // Driving a single element records an element driver, not a whole-signal
                        // driver: a vector may legally be driven element by element.
                        self.record_element_driver(name);
                    }
                    Some(SymKind::InputPort(_)) => {
                        self.diags.push(Diagnostic::error(
                            lv.span.clone(),
                            format!("cannot assign to input port `{name}`"),
                        ));
                    }
                    Some(_) => {
                        self.diags.push(Diagnostic::error(
                            lv.span.clone(),
                            format!("`{name}` cannot be used as an assignment target"),
                        ));
                    }
                }
                // An indexed l-value is scalar-valued.
                SigTy::Scalar
            }
        }
    }

    // ---- expressions ------------------------------------------------------------------------

    /// Type-check an expression used as a spike-time *value*, reporting undeclared names, numeric /
    /// compile-time constants used as wires, non-compile-time delay amounts, and out-of-range
    /// indices along the way. Returns the inferred [`SigTy`].
    fn check_expr(&mut self, expr: &Expr) -> SigTy {
        match expr {
            Expr::Ident(name, span) => {
                self.read_names.insert(name.clone());
                match self.symbols.get(name).cloned() {
                    None => {
                        self.undeclared(name, span);
                        SigTy::Scalar
                    }
                    Some(SymKind::InputPort(t))
                    | Some(SymKind::OutputPort(t))
                    | Some(SymKind::Wire(t)) => t,
                    Some(SymKind::Param) => {
                        // Using a compile-time integer where a wire value is expected would break
                        // time-invariance (Req 2.6, 13.4).
                        self.numeric_as_wire(name, span);
                        SigTy::Scalar
                    }
                    Some(SymKind::Instance) => {
                        self.diags.push(Diagnostic::error(
                            span.clone(),
                            format!("`{name}` is an instance, not a readable value"),
                        ));
                        SigTy::Scalar
                    }
                }
            }
            Expr::Inf(_) => SigTy::Scalar,
            Expr::Index(base, idx, span) => {
                // Indexing a named vector: the base must be a vector; the result is scalar.
                if let Expr::Ident(name, _) = base.as_ref() {
                    self.read_names.insert(name.clone());
                    match self.symbols.get(name).cloned() {
                        None => self.undeclared(name, span),
                        Some(SymKind::InputPort(t))
                        | Some(SymKind::OutputPort(t))
                        | Some(SymKind::Wire(t)) => {
                            if !t.is_vector() {
                                self.diags.push(Diagnostic::error(
                                    span.clone(),
                                    format!("`{name}` is a scalar `time` and cannot be indexed"),
                                ));
                            } else {
                                self.check_index_range(name, t, idx, span);
                            }
                        }
                        Some(SymKind::Param) => {
                            self.numeric_as_wire(name, span);
                        }
                        Some(SymKind::Instance) => {
                            self.diags.push(Diagnostic::error(
                                span.clone(),
                                format!("`{name}` is an instance and cannot be indexed"),
                            ));
                        }
                    }
                } else {
                    // A non-ident base still type-checks for undeclared names etc.
                    let _ = self.check_expr(base);
                    self.check_ct_index(idx);
                }
                SigTy::Scalar
            }
            Expr::Inc(inner, _) => {
                let t = self.check_expr(inner);
                self.require_scalar(t, inner.span(), "inc");
                SigTy::Scalar
            }
            Expr::Delay(base, amount, span) => {
                let t = self.check_expr(base);
                self.require_scalar(t, base.span(), "a delay `a + n`");
                // The RHS of `a + n` must be a compile-time integer, never a wire (Req 2.8).
                self.check_delay_amount(amount, span);
                SigTy::Scalar
            }
            Expr::Min(args, span) => {
                for a in args {
                    let t = self.check_expr(a);
                    self.require_scalar(t, a.span(), "min");
                }
                let _ = span;
                SigTy::Scalar
            }
            Expr::Op { func, args, span } => {
                for a in args {
                    let t = self.check_expr(a);
                    self.require_scalar(t, a.span(), func);
                }
                self.check_arity(func, args.len(), span);
                SigTy::Scalar
            }
            Expr::Call { name, args, span } => {
                for a in args {
                    let t = self.check_expr(a);
                    self.require_scalar(t, a.span(), "a call argument");
                }
                self.check_arity(name, args.len(), span);
                SigTy::Scalar
            }
        }
    }

    /// Check that a call to `name` with `got` arguments matches the callable's expected arity.
    /// Unknown names are left to the undeclared-name check; a variadic `min` requires at least two.
    fn check_arity(&mut self, name: &str, got: usize, span: &Span) {
        let expected = match self.arity.get(name) {
            Some(a) => *a,
            None => return, // not a known callable here; reported elsewhere if truly unknown
        };
        let bad = match expected {
            Arity::Fixed(n) => got != n,
            Arity::Variadic => got < 2,
        };
        if bad {
            let want = match expected {
                Arity::Fixed(n) => format!("exactly {n} argument(s)"),
                Arity::Variadic => "at least 2 arguments".to_string(),
            };
            self.diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!("`{name}` takes {want}, but {got} were given"),
                )
                .with_note(
                    "check the call's argument count; note that `max`/`min` as library functions \
                     are binary — only the `min(...)` primitive is variadic",
                ),
            );
        }
    }

    /// The RHS of `a + n` must be a compile-time integer. If it is a bare identifier naming a wire
    /// or port, that is the "a + n RHS is a wire" error (Req 2.8); otherwise it is checked as a
    /// general compile-time expression (Req 2.7, 2.13).
    fn check_delay_amount(&mut self, amount: &CtExpr, _span: &Span) {
        self.check_ct_expr_values(amount, "a delay amount `a + n`");
    }

    /// A vector index must be a compile-time integer (Req 2.7). Shares the compile-time-value walk.
    fn check_ct_index(&mut self, idx: &CtExpr) {
        self.check_ct_expr_values(idx, "a vector index");
    }

    /// A loop bound / `if` condition must be a compile-time integer (Req 2.7).
    fn check_ct_bound(&mut self, expr: &CtExpr, what: &str) {
        self.check_ct_expr_values(expr, what);
    }

    /// Walk a compile-time expression and report any `param`-position name that actually refers to
    /// a *wire* or *port* (a runtime value) rather than a compile-time integer (Req 2.7, 2.8,
    /// 2.13). Unknown names are undeclared-name errors (Req 2.1).
    fn check_ct_expr_values(&mut self, expr: &CtExpr, what: &str) {
        match &expr.kind {
            CtExprKind::Int(_) => {}
            CtExprKind::Param(name) => match self.symbols.get(name) {
                None => self.undeclared(name, &expr.span),
                Some(SymKind::Param) => {}
                Some(SymKind::InputPort(_))
                | Some(SymKind::OutputPort(_))
                | Some(SymKind::Wire(_)) => {
                    self.diags.push(
                        Diagnostic::error(
                            expr.span.clone(),
                            format!(
                                "`{name}` is a wire value, but {what} must be a compile-time integer"
                            ),
                        )
                        .with_note(
                            "delay amounts, sizes, bounds, and indices are resolved at compile \
                             time; only `param`s, loop variables, and integer arithmetic are allowed",
                        ),
                    );
                }
                Some(SymKind::Instance) => {
                    self.diags.push(Diagnostic::error(
                        expr.span.clone(),
                        format!("`{name}` is an instance, not a compile-time integer"),
                    ));
                }
            },
            CtExprKind::BinOp(_, lhs, rhs) => {
                self.check_ct_expr_values(lhs, what);
                self.check_ct_expr_values(rhs, what);
            }
        }
    }

    /// Range-check `name[idx]` against a concrete vector width where both resolve to constants
    /// (Req 2.10). When either is not a concrete compile-time constant here, the check is deferred
    /// to elaboration (Req 3.8).
    fn check_index_range(&mut self, name: &str, ty: SigTy, idx: &CtExpr, span: &Span) {
        self.check_ct_index(idx);
        if let (SigTy::Vector(width), Some(i)) = (ty, eval_const(idx, &self.param_env)) {
            if i >= width {
                self.diags.push(
                    Diagnostic::error(
                        span.clone(),
                        format!(
                            "index {i} is out of range for `{name}` of type `time[{width}]` \
                             (valid indices are 0..{})",
                            width.saturating_sub(1)
                        ),
                    )
                    .with_note("vector indices are 0-based and must be less than the width"),
                );
            }
        }
    }

    // ---- instances --------------------------------------------------------------------------

    /// Check an instance: resolve the instantiated module, validate connected ports exist and match
    /// types, record output-port drivers against the connected wires, and warn about any output
    /// port left unconnected (Req 2.15).
    fn check_instance(
        &mut self,
        module: &str,
        params: &[(String, CtExpr)],
        conns: &[(String, LValue)],
        span: &Span,
    ) {
        // Parameter override values must be compile-time integers (Req 2.7).
        for (_pname, value) in params {
            self.check_ct_bound(value, "an instance parameter value");
        }

        if self.module_table.get(module).is_none() {
            self.diags.push(Diagnostic::error(
                span.clone(),
                format!("instance refers to unknown module `{module}`"),
            ));
            // Still check the connection wires exist so we report their issues too.
            for (_p, lv) in conns {
                self.check_connection_lvalue(lv);
            }
            return;
        }

        // Evaluate the instance's parameter overrides in the PARENT's environment (so a value like
        // `#(.N(N))` resolves `N` to the parent's param value), then build the submodule's own
        // parameter environment (its defaults overlaid with these overrides). Port vector sizes are
        // resolved against THIS env, so a parameterized port like `x[N]` is typed per instance
        // rather than at the submodule's defaults.
        let mut overrides: HashMap<String, u64> = HashMap::new();
        for (pname, value) in params {
            if let Some(v) = eval_const(value, &self.param_env) {
                overrides.insert(pname.clone(), v);
            }
        }
        let ports = self.module_table.get(module).expect("module presence checked above");
        let inst_env = ports.instance_env(&overrides);

        // Collect each port's resolved type and direction up front (immutable borrow of `ports`),
        // so the mutable `self` calls below do not conflict with the borrow.
        let mut resolved: Vec<(String, SigTy, Dir)> = Vec::new();
        for (name, dir, ty) in &ports.ports {
            resolved.push((name.clone(), resolve_port_ty(ty, &inst_env), *dir));
        }
        let output_port_names: Vec<String> = ports
            .ports
            .iter()
            .filter(|(_, d, _)| *d == Dir::Output)
            .map(|(n, _, _)| n.clone())
            .collect();

        let mut connected: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (port_name, lv) in conns {
            connected.insert(port_name.as_str());
            let port = resolved.iter().find(|(n, _, _)| n == port_name);
            let actual = self.check_connection_lvalue(lv);

            match port {
                None => {
                    self.diags.push(Diagnostic::error(
                        lv.span.clone(),
                        format!("module `{module}` has no port named `{port_name}`"),
                    ));
                }
                Some((_, expected, dir)) => {
                    if !expected.compatible_with(actual) {
                        self.diags.push(
                            Diagnostic::error(
                                lv.span.clone(),
                                format!(
                                    "connection to port `{port_name}` of `{module}` has type `{}` \
                                     but the port is `{}`",
                                    actual.describe(),
                                    expected.describe()
                                ),
                            )
                            .with_note("ports and wires must have matching scalar/vector types"),
                        );
                    }
                    // Connecting an *output* port of the instance drives the target. A whole-wire
                    // target is a whole-signal driver; an indexed target is an element driver.
                    if *dir == Dir::Output {
                        match &lv.kind {
                            LValueKind::Wire(target) => self.record_driver(target, 1),
                            LValueKind::Index(target, _) => self.record_element_driver(target),
                        }
                    }
                }
            }
        }

        // Warn about any output port of the instance left unconnected (Req 2.15).
        for out_name in &output_port_names {
            if !connected.contains(out_name.as_str()) {
                self.diags.push(
                    Diagnostic::warning(
                        span.clone(),
                        format!(
                            "output port `{out_name}` of instance of `{module}` is left unconnected"
                        ),
                    )
                    .with_note("an unconnected output is discarded; connect it or remove it"),
                );
            }
        }
    }

    /// Resolve a connection l-value (a wire or element the instance port is wired to), reporting an
    /// undeclared name, and return its signal type. Also records the name as read (an input-port
    /// connection reads the wire).
    fn check_connection_lvalue(&mut self, lv: &LValue) -> SigTy {
        match &lv.kind {
            LValueKind::Wire(name) => {
                self.read_names.insert(name.clone());
                match self.symbols.get(name).cloned() {
                    None => {
                        self.undeclared(name, &lv.span);
                        SigTy::Scalar
                    }
                    Some(kind) => kind.sig_ty().unwrap_or_else(|| {
                        self.diags.push(Diagnostic::error(
                            lv.span.clone(),
                            format!("`{name}` cannot be connected to a port"),
                        ));
                        SigTy::Scalar
                    }),
                }
            }
            LValueKind::Index(name, idx) => {
                self.read_names.insert(name.clone());
                match self.symbols.get(name).cloned() {
                    None => {
                        self.undeclared(name, &lv.span);
                    }
                    Some(kind) => {
                        if let Some(t) = kind.sig_ty() {
                            if !t.is_vector() {
                                self.diags.push(Diagnostic::error(
                                    lv.span.clone(),
                                    format!("`{name}` is a scalar `time` and cannot be indexed"),
                                ));
                            } else {
                                self.check_index_range(name, t, idx, &lv.span);
                            }
                        } else {
                            self.diags.push(Diagnostic::error(
                                lv.span.clone(),
                                format!("`{name}` cannot be connected to a port"),
                            ));
                        }
                    }
                }
                SigTy::Scalar
            }
        }
    }

    // ---- assert -----------------------------------------------------------------------------

    /// Evaluate a compile-time `assert`. The condition must be a compile-time integer (Req 2.7); if
    /// it evaluates to a concrete zero it is a failed assertion (Req 2.12). A non-constant
    /// condition (depends on a param without a default) is deferred to elaboration.
    fn check_assert(&mut self, cond: &CtExpr, span: &Span) {
        self.check_ct_bound(cond, "an `assert` condition");
        if let Some(v) = eval_const(cond, &self.param_env) {
            if v == 0 {
                self.diags.push(
                    Diagnostic::error(
                        span.clone(),
                        "assertion failed: the compile-time condition evaluated to 0 (false)",
                    )
                    .with_note("a compile-time `assert` must evaluate to a nonzero (true) value"),
                );
            }
        }
    }

    // ---- driver / unused checks -------------------------------------------------------------

    /// Verify every wire and output port is driven exactly once: zero drivers is undriven
    /// (Req 2.5, 2.11), more than one is a multiple-driver error (Req 2.4).
    fn check_drivers(&mut self) {
        // Deterministic order: ports in declaration order, then wires in declaration order.
        let mut driven_signals: Vec<(String, SigTy, bool)> = Vec::new(); // (name, ty, is_output)
        for port in &self.module.ports {
            if port.dir == Dir::Output {
                let ty = resolve_port_ty(&port.ty, &self.param_env);
                driven_signals.push((port.name.clone(), ty, true));
            }
        }
        for name in &self.wire_order {
            if let Some(SymKind::Wire(t)) = self.symbols.get(name) {
                driven_signals.push((name.clone(), *t, false));
            }
        }

        for (name, ty, is_output) in driven_signals {
            let whole = self.driver_count.get(&name).copied().unwrap_or(0);
            let elems = self.element_driver_count.get(&name).copied().unwrap_or(0);
            let span = self
                .decl_spans
                .get(&name)
                .cloned()
                .unwrap_or_else(|| self.module.span.clone());
            let what = if is_output { "output port" } else { "wire" };

            // More than one whole-signal driver is always a multiple-driver error (Req 2.4).
            if whole > 1 {
                self.diags.push(
                    Diagnostic::error(
                        span.clone(),
                        format!("{what} `{name}` has {whole} drivers but must have exactly one"),
                    )
                    .with_note(
                        "a wire or output port may be driven by only one assignment or connection",
                    ),
                );
                continue;
            }

            // Mixing a whole-signal driver with element drivers also over-drives the signal.
            if whole == 1 && elems > 0 {
                self.diags.push(
                    Diagnostic::error(
                        span.clone(),
                        format!(
                            "{what} `{name}` is driven both as a whole and element by element"
                        ),
                    )
                    .with_note("drive a vector either as a whole or by elements, not both"),
                );
                continue;
            }

            let driven = whole > 0 || elems > 0;
            if !driven {
                // A scalar with no driver, or a vector with no driver at all, is undriven (Req 2.5,
                // 2.11). Per-element coverage of a vector is checked during elaboration once the
                // loops that fill it are unrolled.
                self.diags.push(
                    Diagnostic::error(span, format!("{what} `{name}` has no driver"))
                        .with_note("every wire and output port must be driven exactly once"),
                );
                continue;
            }

            // A scalar driven only by element writes would be an indexing error already reported
            // in check_lvalue; nothing more to do here. Vectors driven element-wise are fine.
            let _ = ty;
        }
    }

    /// Emit an unused-wire warning for each wire that is never read (Req 2.14). Warnings never stop
    /// compilation.
    fn check_unused_wires(&mut self) {
        for name in &self.wire_order.clone() {
            if !self.read_names.contains(name) {
                let span = self
                    .decl_spans
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| self.module.span.clone());
                self.diags.push(
                    Diagnostic::warning(span, format!("wire `{name}` is declared but never read"))
                        .with_note("remove the wire or use its value to silence this warning"),
                );
            }
        }
    }

    // ---- small helpers ----------------------------------------------------------------------

    fn undeclared(&mut self, name: &str, span: &Span) {
        self.diags.push(Diagnostic::error(
            span.clone(),
            format!("use of undeclared name `{name}`"),
        ));
    }

    fn numeric_as_wire(&mut self, name: &str, span: &Span) {
        self.diags.push(
            Diagnostic::error(
                span.clone(),
                format!(
                    "`{name}` is a compile-time integer and cannot be used as a wire value"
                ),
            )
            .with_note(
                "a numeric constant used as a wire value would violate time-invariance: shifting \
                 every input by a constant must shift every output by the same constant, which a \
                 fixed number does not do",
            ),
        );
    }

    fn require_scalar(&mut self, ty: SigTy, span: &Span, context: &str) {
        if ty.is_vector() {
            self.diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!(
                        "{context} expects a scalar `time` value but got `{}`",
                        ty.describe()
                    ),
                )
                .with_note("index the vector (`x[i]`) to use a single element as a scalar value"),
            );
        }
    }

    /// Check assignment / driver type compatibility between a target and a value (Req 2.3).
    fn check_assign_types(&mut self, target: SigTy, value: SigTy, span: &Span) {
        if !target.compatible_with(value) {
            self.diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!(
                        "type mismatch: cannot drive a `{}` target with a `{}` value",
                        target.describe(),
                        value.describe()
                    ),
                )
                .with_note("the driven signal and its driver must have matching scalar/vector types"),
            );
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::operators::OperatorTable;
    use crate::parser::parse_source;
    use crate::lexer::TimeWidth;

    /// Parse a source string into a design, panicking on any parse error so tests exercise sema.
    fn design(src: &str) -> Design {
        parse_source(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32).unwrap_or_else(
            |d| {
                let rendered: Vec<String> = d.iter().map(|x| x.render()).collect();
                panic!("test fixture failed to parse:\n{}", rendered.join("\n"))
            },
        )
    }

    /// Analyze a source string, expecting a clean (error-free) result; returns the CheckedDesign.
    fn analyze_ok(src: &str) -> CheckedDesign {
        match analyze(&design(src)) {
            Ok(c) => c,
            Err(errs) => {
                let rendered: Vec<String> = errs.iter().map(|e| e.render()).collect();
                panic!("expected a clean analysis, got errors:\n{}", rendered.join("\n"))
            }
        }
    }

    /// Analyze a source string, expecting at least one error; returns the diagnostics.
    fn analyze_err(src: &str) -> Vec<Diagnostic> {
        match analyze(&design(src)) {
            Ok(_) => panic!("expected an analysis error, but analysis succeeded"),
            Err(errs) => errs,
        }
    }

    /// True if some error diagnostic's message contains `needle`.
    fn has_err_msg(diags: &[Diagnostic], needle: &str) -> bool {
        diags
            .iter()
            .any(|d| d.is_error() && d.message.contains(needle))
    }

    // --- a legal baseline module analyzes cleanly -------------------------------------------

    #[test]
    fn clean_module_has_no_errors() {
        let c = analyze_ok(
            "module Sort2 (input time a, input time b, output time lo, output time hi) {\n\
             lo = min(a, b);\n\
             hi = a < b;\n\
             }",
        );
        assert!(c.warnings().is_empty());
    }

    // --- Req 2.1 undeclared name ------------------------------------------------------------

    #[test]
    fn undeclared_name_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) {\n\
             y = min(a, b);\n\
             }",
        );
        assert!(has_err_msg(&errs, "undeclared name `b`"));
    }

    // --- Req 2.2 duplicate declaration ------------------------------------------------------

    #[test]
    fn duplicate_wire_declaration_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) {\n\
             wire time w = a;\n\
             wire time w = a;\n\
             y = w;\n\
             }",
        );
        assert!(has_err_msg(&errs, "duplicate declaration"));
    }

    #[test]
    fn duplicate_port_and_wire_name_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) {\n\
             wire time a = y;\n\
             y = a;\n\
             }",
        );
        assert!(has_err_msg(&errs, "duplicate declaration"));
    }

    #[test]
    fn duplicate_module_declaration_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) { y = a; }\n\
             module M (input time a, output time y) { y = a; }",
        );
        assert!(has_err_msg(&errs, "module `M` is already declared"));
    }

    // --- Req 2.3 type / vector-size mismatch ------------------------------------------------

    #[test]
    fn assigning_vector_to_scalar_is_a_type_mismatch() {
        let errs = analyze_err(
            "module M (input time x[2], output time y) {\n\
             y = x;\n\
             }",
        );
        assert!(has_err_msg(&errs, "type mismatch"));
    }

    #[test]
    fn mismatched_vector_widths_on_connection_is_an_error() {
        let errs = analyze_err(
            "module Sub (input time x[2], output time y) { y = x[0]; }\n\
             module Top (input time w[3], output time r) {\n\
             inst s = Sub (.x(w), .y(r));\n\
             }",
        );
        assert!(has_err_msg(&errs, "but the port is"));
    }

    #[test]
    fn indexing_a_scalar_is_a_type_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) {\n\
             y = a[0];\n\
             }",
        );
        assert!(has_err_msg(&errs, "cannot be indexed"));
    }

    // --- Req 2.4 multiple drivers -----------------------------------------------------------

    #[test]
    fn multiple_drivers_on_an_output_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, input time b, output time y) {\n\
             y = a;\n\
             y = b;\n\
             }",
        );
        assert!(has_err_msg(&errs, "has 2 drivers"));
    }

    #[test]
    fn multiple_drivers_on_a_wire_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, input time b, output time y) {\n\
             wire time w = a;\n\
             w = b;\n\
             y = w;\n\
             }",
        );
        assert!(has_err_msg(&errs, "has 2 drivers"));
    }

    // --- Req 2.5 / 2.11 undriven signal -----------------------------------------------------

    #[test]
    fn undriven_output_is_an_error() {
        let errs = analyze_err("module M (input time a, output time y) { wire time t = a; }");
        assert!(has_err_msg(&errs, "output port `y` has no driver"));
    }

    #[test]
    fn undriven_wire_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) {\n\
             wire time w;\n\
             y = a;\n\
             }",
        );
        // `w` is declared, never driven, and (because it is never read) also warned as unused.
        assert!(has_err_msg(&errs, "wire `w` has no driver"));
    }

    // --- Req 2.6 / 13.4 numeric literal / compile-time constant as a wire -------------------

    #[test]
    fn param_used_as_a_wire_value_is_an_invariance_error() {
        let errs = analyze_err(
            "module M #(param N = 3) (input time a, output time y) {\n\
             y = min(a, N);\n\
             }",
        );
        let diag = errs
            .iter()
            .find(|d| d.is_error() && d.message.contains("cannot be used as a wire value"))
            .expect("expected a numeric-as-wire error");
        let note = diag.note.as_deref().unwrap_or("");
        assert!(
            note.contains("time-invariance"),
            "the note must explain the invariance violation, got: {note}"
        );
    }

    // --- Req 2.7 non-compile-time delay / bound / index -------------------------------------

    #[test]
    fn wire_as_a_loop_bound_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) {\n\
             wire time k = a;\n\
             for i in 0..k { }\n\
             y = a;\n\
             }",
        );
        assert!(has_err_msg(&errs, "must be a compile-time integer"));
    }

    #[test]
    fn wire_as_a_vector_index_is_an_error() {
        let errs = analyze_err(
            "module M (input time x[3], input time k, output time y) {\n\
             y = x[k];\n\
             }",
        );
        assert!(has_err_msg(&errs, "must be a compile-time integer"));
    }

    // --- Req 2.8 the RHS of `a + n` is a wire -----------------------------------------------

    #[test]
    fn delay_amount_as_a_wire_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, input time d, output time y) {\n\
             y = a + d;\n\
             }",
        );
        assert!(has_err_msg(&errs, "must be a compile-time integer"));
    }

    #[test]
    fn delay_by_a_param_is_legal() {
        analyze_ok(
            "module DelayN #(param N = 3) (input time a, output time y) {\n\
             y = a + N;\n\
             }",
        );
    }

    // --- Req 2.10 out-of-range index --------------------------------------------------------

    #[test]
    fn out_of_range_constant_index_is_an_error() {
        let errs = analyze_err(
            "module M (input time x[3], output time y) {\n\
             y = x[3];\n\
             }",
        );
        assert!(has_err_msg(&errs, "out of range"));
    }

    #[test]
    fn in_range_index_is_legal() {
        analyze_ok(
            "module M (input time x[3], output time y) {\n\
             y = x[2];\n\
             }",
        );
    }

    #[test]
    fn out_of_range_index_via_param_default_is_an_error() {
        let errs = analyze_err(
            "module M #(param N = 2) (input time x[N], output time y) {\n\
             y = x[5];\n\
             }",
        );
        assert!(has_err_msg(&errs, "out of range"));
    }

    // --- Req 2.12 compile-time assert -------------------------------------------------------

    #[test]
    fn failed_compile_time_assert_is_an_error() {
        let errs = analyze_err(
            "module M #(param N = 0) (input time a, output time y) {\n\
             assert N;\n\
             y = a;\n\
             }",
        );
        assert!(has_err_msg(&errs, "assertion failed"));
    }

    // --- testbench vector-shape validation --------------------------------------------------

    #[test]
    fn wave_input_vector_length_must_match_port_width() {
        let errs = analyze_err(
            "module M (input time x[3], output time y) { y = min(x[0], min(x[1], x[2])); }\n\
             test t for M { wave { x = [1, 2]; expect y = 1; } }",
        );
        assert!(
            has_err_msg(&errs, "is `time[3]` but the value has 2 element(s)"),
            "a too-short input vector must error: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn wave_expect_vector_length_must_match_port_width() {
        let errs = analyze_err(
            "module M (input time a, output time y[2]) { y[0] = a; y[1] = a; }\n\
             test t for M { wave { a = 1; expect y = [1]; } }",
        );
        assert!(has_err_msg(&errs, "is `time[2]` but the value has 1 element(s)"));
    }

    #[test]
    fn scalar_port_rejects_a_vector_stimulus() {
        let errs = analyze_err(
            "module M (input time a, output time y) { y = a; }\n\
             test t for M { wave { a = [1, 2]; expect y = 1; } }",
        );
        assert!(has_err_msg(&errs, "is scalar `time` but a vector value was given"));
    }

    #[test]
    fn vector_port_rejects_a_scalar_stimulus() {
        let errs = analyze_err(
            "module M (input time x[2], output time y) { y = min(x[0], x[1]); }\n\
             test t for M { wave { x = 1; expect y = 1; } }",
        );
        assert!(has_err_msg(&errs, "is `time[2]` but a scalar value was given"));
    }

    #[test]
    fn testbench_binding_to_unknown_port_is_an_error() {
        let errs = analyze_err(
            "module M (input time a, output time y) { y = a; }\n\
             test t for M { wave { a = 1; expect z = 1; } }",
        );
        assert!(has_err_msg(&errs, "has no port named `z`"));
    }

    #[test]
    fn correctly_shaped_testbench_is_accepted() {
        analyze_ok(
            "module M (input time x[3], output time y[2]) { y[0] = min(x[0], x[1]); y[1] = max(x[1], x[2]); }\n\
             test t for M {\n\
             default { y = [inf, inf]; }\n\
             wave { x = [1, 2, 3]; expect y = [1, 3]; }\n\
             }",
        );
    }

    #[test]
    fn cases_vector_shapes_are_checked_too() {
        // A `cases` block expands to waves; each column must still match the port widths.
        let errs = analyze_err(
            "module M (input time x[2], output time y) { y = min(x[0], x[1]); }\n\
             cases c for M { x = [[1,2], [3]]; expect y = [1, 3]; }",
        );
        // The second column `[3]` is length 1 for a time[2] port.
        assert!(has_err_msg(&errs, "is `time[2]` but the value has 1 element(s)"));
    }

    #[test]
    fn instance_port_type_uses_the_override_not_the_default() {
        // Sub has a param N defaulting to 2; an instance overrides N=3 and connects a time[3] wire.
        // The connection must type-check against the OVERRIDE (time[3]), not the default (time[2]).
        analyze_ok(
            "module Sub #(param N = 2) (input time x[N], output time y) { y = x[0]; }\n\
             module Top (input time d[3], output time o) {\n\
             inst s = Sub #(.N(3)) (.x(d), .y(o));\n\
             }",
        );
    }

    #[test]
    fn instance_override_can_reference_the_parent_param() {
        // `#(.N(N))` passes the PARENT's N into the child; typing must resolve N in the parent env.
        analyze_ok(
            "module Sub #(param N = 2) (input time x[N], output time y) { y = x[0]; }\n\
             module Top #(param N = 4) (input time d[N], output time o) {\n\
             inst s = Sub #(.N(N)) (.x(d), .y(o));\n\
             }",
        );
    }

    #[test]
    fn instance_port_mismatch_is_still_caught_with_overrides() {
        // Override N=3 but connect a time[2] wire: a genuine mismatch must still error.
        let errs = analyze_err(
            "module Sub #(param N = 2) (input time x[N], output time y) { y = x[0]; }\n\
             module Top (input time d[2], output time o) {\n\
             inst s = Sub #(.N(3)) (.x(d), .y(o));\n\
             }",
        );
        assert!(
            has_err_msg(&errs, "but the port is"),
            "a real width mismatch under an override must still be reported: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn compile_time_comparison_conditions_work() {
        // `N == 3` is true at the default, so the then-branch (driving y) is taken; valid design.
        analyze_ok(
            "module M #(param N = 3) (input time a, output time y) {\n\
             if (N == 3) { y = a; } else { y = a + 1; }\n\
             }",
        );
        // A nested arithmetic comparison `(N - 2) == 4` also parses and evaluates.
        analyze_ok(
            "module M #(param N = 6) (input time a, output time y) {\n\
             if ((N - 2) == 4) { y = a; } else { y = a + 1; }\n\
             }",
        );
        // `assert N >= 1;` now works directly (comparison yields 1 = true).
        analyze_ok(
            "module M #(param N = 3) (input time a, output time y) { assert N >= 1; y = a; }",
        );
        // A false assert via comparison is still caught.
        let errs = analyze_err(
            "module M #(param N = 0) (input time a, output time y) { assert N >= 1; y = a; }",
        );
        assert!(has_err_msg(&errs, "assertion failed"), "N >= 1 is false at N=0");
    }

    #[test]
    fn if_generate_both_branches_may_drive_the_same_output() {
        // A compile-time `if` whose two branches each drive the SAME output is NOT a double-driver
        // error: only the taken branch counts. This is the standard pattern for a degenerate param.
        analyze_ok(
            "module M #(param N = 3) (input time a, output time y) {\n\
             if (N - 1) { y = inc(a); } else { y = a; }\n\
             }",
        );
        // And at the degenerate value too (else-branch taken).
        analyze_ok(
            "module M #(param N = 1) (input time a, output time y) {\n\
             if (N - 1) { y = inc(a); } else { y = a; }\n\
             }",
        );
    }

    #[test]
    fn passing_assert_is_legal() {
        analyze_ok(
            "module M #(param N = 4) (input time a, output time y) {\n\
             assert N;\n\
             assert N - 4 + 1;\n\
             y = a;\n\
             }",
        );
    }

    // --- Req 2.14 unused-wire warning (does not stop compilation) ---------------------------

    #[test]
    fn unused_wire_emits_a_warning_and_still_succeeds() {
        let c = analyze_ok(
            "module M (input time a, output time y) {\n\
             wire time t = a;\n\
             y = a;\n\
             }",
        );
        assert_eq!(c.warnings().len(), 1);
        let w = &c.warnings()[0];
        assert_eq!(w.severity, crate::diag::Severity::Warning);
        assert!(w.message.contains("`t` is declared but never read"));
    }

    // --- Req 2.15 unconnected-output warning (does not stop compilation) --------------------

    #[test]
    fn unconnected_instance_output_emits_a_warning_and_still_succeeds() {
        let c = analyze_ok(
            "module Sub (input time a, output time p, output time q) {\n\
             p = a;\n\
             q = a;\n\
             }\n\
             module Top (input time a, output time y) {\n\
             inst s = Sub (.a(a), .p(y));\n\
             }",
        );
        // `q` is left unconnected -> a warning, but analysis still succeeds.
        assert!(
            c.warnings()
                .iter()
                .any(|w| w.message.contains("`q`") && w.message.contains("unconnected")),
            "expected an unconnected-output warning for `q`, got: {:?}",
            c.warnings().iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // --- Req 2.16 / 2.17 all diagnostics collected in one pass ------------------------------

    #[test]
    fn multiple_errors_are_all_reported_in_one_pass() {
        let errs = analyze_err(
            "module M (input time a, output time y, output time z) {\n\
             y = min(a, b);\n\
             }",
        );
        // Three independent problems: undeclared `b`, undriven `z`, undriven... actually y is
        // driven. We expect at least: undeclared `b` and undriven output `z`.
        assert!(has_err_msg(&errs, "undeclared name `b`"));
        assert!(has_err_msg(&errs, "output port `z` has no driver"));
        assert!(errs.len() >= 2, "expected multiple errors, got {}", errs.len());
    }

    #[test]
    fn any_error_means_no_checked_design() {
        let result = analyze(&design(
            "module M (input time a, output time y) { }",
        ));
        assert!(result.is_err(), "an undriven output must fail the analysis");
    }

    // --- a vector-wide module with a generate loop analyzes cleanly -------------------------

    #[test]
    fn generate_loop_module_analyzes_cleanly() {
        analyze_ok(
            "module MinN #(param N = 4) (input time x[N], output time y) {\n\
             wire time acc[N];\n\
             acc[0] = x[0];\n\
             for i in 1..N {\n\
             acc[i] = min(acc[i - 1], x[i]);\n\
             }\n\
             y = acc[N - 1];\n\
             }",
        );
    }
}
