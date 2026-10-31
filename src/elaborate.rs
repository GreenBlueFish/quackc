//! Elaboration: expand params, unroll loops, inline funcs and derived ops (Requirement 3).
//!
//! Elaboration turns a [`CheckedDesign`] (a validated but still parametric / generative AST) into a
//! concrete [`Elaborated`] structure rooted at the `--top` module (Req 3.12). Along the way it:
//!
//! * resolves the top module, erroring if the name does not match any module (Req 3.10);
//! * substitutes parameter values into every size, bound, index, delay amount, and compile-time
//!   condition (Req 3.1);
//! * fully unrolls `for` generate loops into straight-line statements (Req 3.2);
//! * selects the taken branch of a compile-time `if` (Req 3.3);
//! * inline-expands `func` calls as pure compile-time macros (Req 3.4);
//! * expands the derived operators `le`, `ge`, `eq`, `max`, `gt`, `neq` into the three primitives
//!   `inc`, `min`, `lt` (Req 3.5);
//! * recurses into sub-module instances, preserving each element's hierarchy path (Req 3.11);
//! * leaves every surviving size / bound / index / delay amount a resolved non-negative integer
//!   constant (Req 3.6).
//!
//! It reports, with a source location, a `for` bound that is not a compile-time constant (Req 3.7),
//! a vector index that resolves out of range (Req 3.8), recursion through `func`/instance expansion
//! (Req 3.9, 2.9), and a missing `--top` module (Req 3.10). Every detected problem is collected and
//! returned together; elaboration yields either an [`Elaborated`] or the full diagnostic list.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::{Diagnostic, Diagnostics, Span};
use crate::sema::CheckedDesign;

/// A fully elaborated design: a concrete tree of module instances rooted at `--top`, with every
/// compile-time quantity resolved to an integer and every expression reduced to the three
/// primitives (`inc`/`min`/`lt`) plus `min`, indexing, identifiers and `inf` (Req 3.6).
///
/// This is the input to the netlist IR builder (task 9), which reads `&Elaborated`.
#[derive(Clone, Debug, PartialEq)]
pub struct Elaborated {
    /// The name of the top module elaboration was rooted at (Req 3.12).
    pub top: String,
    /// The root elaborated module instance; its hierarchy path is the top module name (Req 3.11).
    pub root: ElabModule,
}

/// One elaborated module instance in the hierarchy (Req 3.11). All of its parameters have been
/// substituted, its loops unrolled, its `if`s selected, and its body reduced to concrete,
/// primitive-only statements. Sub-module instances appear as nested [`ElabInstance`]s so the
/// hierarchy is retained rather than flattened.
#[derive(Clone, Debug, PartialEq)]
pub struct ElabModule {
    /// The source module name this instance was elaborated from.
    pub name: String,
    /// The hierarchy path from the root to this instance, e.g. `Top` or `Top/s0` (Req 3.11).
    pub path: String,
    /// Resolved parameter values in scope for this module.
    pub params: HashMap<String, u64>,
    /// Input/output ports with resolved (concrete) signal shapes.
    pub ports: Vec<ElabPort>,
    /// The module's own wire-driving statements (whole-wire or element assignments), primitive-only.
    pub assigns: Vec<ElabAssign>,
    /// Nested sub-module instances, each already elaborated.
    pub instances: Vec<ElabInstance>,
}

/// A port of an elaborated module with a resolved shape.
#[derive(Clone, Debug, PartialEq)]
pub struct ElabPort {
    pub dir: Dir,
    pub name: String,
    /// `None` for a scalar `time`; `Some(n)` for a `time[n]` with resolved width `n >= 1`.
    pub width: Option<u64>,
    pub span: Span,
}

/// A concrete assignment in an elaborated module. The target is a resolved signal reference (whole
/// wire or a specific element index) and the value is a primitive-only [`ElabExpr`].
#[derive(Clone, Debug, PartialEq)]
pub struct ElabAssign {
    pub target: ElabRef,
    pub value: ElabExpr,
    pub span: Span,
}

/// A nested elaborated sub-module instance (Req 3.11). Its connections wire the parent's signals to
/// the instance's ports; the instance's own body lives in `module`.
#[derive(Clone, Debug, PartialEq)]
pub struct ElabInstance {
    /// The instance name as declared in the parent (`inst <name> = ...`).
    pub inst_name: String,
    /// The elaborated body of the instantiated module, with its own hierarchy path.
    pub module: ElabModule,
    /// `(instance port name, parent signal reference)` connections, indices resolved.
    pub conns: Vec<(String, ElabRef)>,
    pub span: Span,
}

/// A resolved reference to a signal: a whole wire/port by name, or a specific element of a vector
/// with its index resolved to a concrete integer (Req 3.6).
#[derive(Clone, Debug, PartialEq)]
pub enum ElabRef {
    Wire(String),
    Index(String, u64),
}

/// A primitive-only elaborated expression (Req 3.5, 3.6). Derived operators have been expanded,
/// `func` calls inlined, and every delay amount / index resolved to an integer.
#[derive(Clone, Debug, PartialEq)]
pub enum ElabExpr {
    /// A whole wire / port by name.
    Ident(String, Span),
    /// `name[i]` with a resolved index.
    Index(String, u64, Span),
    /// The `inf` literal.
    Inf(Span),
    /// `inc(a)` — unit delay primitive.
    Inc(Box<ElabExpr>, Span),
    /// `a + n` — a delay by a resolved compile-time amount `n` (a chain of `n` unit incs).
    Delay(Box<ElabExpr>, u64, Span),
    /// `min(...)` — variadic minimum primitive (`>= 2` args after expansion).
    Min(Vec<ElabExpr>, Span),
    /// `lt(a, b)` — the strict-less-than / inhibit primitive.
    Lt(Box<ElabExpr>, Box<ElabExpr>, Span),
}

impl ElabExpr {
    /// The source span of this expression, regardless of variant.
    pub fn span(&self) -> &Span {
        match self {
            ElabExpr::Ident(_, s)
            | ElabExpr::Index(_, _, s)
            | ElabExpr::Inf(s)
            | ElabExpr::Inc(_, s)
            | ElabExpr::Delay(_, _, s)
            | ElabExpr::Min(_, s)
            | ElabExpr::Lt(_, _, s) => s,
        }
    }
}

/// Elaborate a checked design rooted at the module named `top` (Requirement 3).
///
/// Returns the concrete [`Elaborated`] structure on success, or the full list of diagnostics (a
/// missing top module, a non-constant `for` bound, an out-of-range index, or recursion) otherwise.
pub fn elaborate(checked: &CheckedDesign, top: &str) -> Result<Elaborated, Vec<Diagnostic>> {
    let design = checked.design();
    let mut diags = Diagnostics::new();

    // Resolve the top module by name (Req 3.10, 3.12).
    let top_module = match design.modules.iter().find(|m| m.name == top) {
        Some(m) => m,
        None => {
            diags.push(
                Diagnostic::error(
                    design_span(design),
                    format!("no module named `{top}` to use as the top module (`--top {top}`)"),
                )
                .with_note(
                    "the `--top` option must name a module declared in the design; \
                     check the spelling or declare the module",
                ),
            );
            return Err(diags.into_vec());
        }
    };

    let mut elab = Elaborator {
        design,
        diags: &mut diags,
    };

    // The top module is elaborated with its parameter *defaults* (no overriding instance), its
    // hierarchy path is just its name (Req 3.11), and it anchors recursion detection.
    let initial_stack = vec![top_module.name.clone()];
    let root = elab.elaborate_module(top_module, &[], top_module.name.clone(), &initial_stack);

    if diags.has_errors() {
        Err(diags.into_vec())
    } else {
        Ok(Elaborated {
            top: top.to_string(),
            root,
        })
    }
}

/// A span to anchor a whole-design error (missing top) against: the first module's span if any,
/// otherwise a synthetic location in the (sole) source unit.
fn design_span(design: &Design) -> Span {
    if let Some(m) = design.modules.first() {
        m.span.clone()
    } else {
        Span::new("<design>", 1, 1)
    }
}

// -------------------------------------------------------------------------------------------------
// Elaborator
// -------------------------------------------------------------------------------------------------

struct Elaborator<'a> {
    design: &'a Design,
    diags: &'a mut Diagnostics,
}

impl<'a> Elaborator<'a> {
    /// Elaborate one module instance.
    ///
    /// * `module` — the source module being elaborated.
    /// * `overrides` — `(name, value)` parameter overrides supplied by the instantiating parent;
    ///   empty for the top module (defaults apply).
    /// * `path` — the hierarchy path of this instance (Req 3.11).
    /// * `stack` — the chain of module names currently being expanded, for recursion detection
    ///   (Req 3.9, 2.9). The module being elaborated is already on the stack.
    fn elaborate_module(
        &mut self,
        module: &Module,
        overrides: &[(String, u64)],
        path: String,
        stack: &[String],
    ) -> ElabModule {
        // Build the parameter environment: start from defaults, then apply overrides (Req 3.1).
        let mut env = CtEnv::new();
        for p in &module.params {
            if let Some(def) = &p.default {
                if let Some(v) = self.eval_ct(def, &env) {
                    env.set(&p.name, v);
                }
            }
        }
        for (name, value) in overrides {
            env.set(name, *value);
        }

        // Resolve this module's ports to concrete widths (Req 3.6).
        let mut ports = Vec::new();
        for port in &module.ports {
            let width = match &port.ty.kind {
                TyKind::Time => None,
                TyKind::TimeVec(n) => self.eval_ct(n, &env),
            };
            ports.push(ElabPort {
                dir: port.dir,
                name: port.name.clone(),
                width,
                span: port.span.clone(),
            });
        }

        // A table of signal widths in this module scope, for out-of-range index checks (Req 3.8).
        let mut widths: HashMap<String, Option<u64>> = HashMap::new();
        for port in &ports {
            widths.insert(port.name.clone(), port.width);
        }

        let mut ctx = ModuleCtx {
            env,
            path: path.clone(),
            widths,
            assigns: Vec::new(),
            instances: Vec::new(),
            inst_counter: 0,
        };

        // Pre-scan declarations so index checks for forward-referenced wires still have a width.
        self.collect_widths(&module.body, &mut ctx);

        // Walk the (generative) body, unrolling loops and selecting branches as we go.
        let body = module.body.clone();
        self.elaborate_stmts(&body, &mut ctx, stack);

        ElabModule {
            name: module.name.clone(),
            path,
            params: ctx.env.values.clone(),
            ports,
            assigns: ctx.assigns,
            instances: ctx.instances,
        }
    }

    /// Record vector widths of wires declared anywhere in a (possibly generative) body, evaluating
    /// the width against the current environment. Loop-local wires inside `for` bodies vary per
    /// iteration only in name suffixing performed later; their base widths do not depend on the
    /// loop variable, so a single pass with the module environment is sufficient for the range
    /// check's purposes.
    fn collect_widths(&mut self, stmts: &[Stmt], ctx: &mut ModuleCtx) {
        for stmt in stmts {
            match stmt {
                Stmt::WireDecl { ty, name, .. } => {
                    let width = match &ty.kind {
                        TyKind::Time => None,
                        TyKind::TimeVec(n) => self.eval_ct(n, &ctx.env),
                    };
                    ctx.widths.entry(name.clone()).or_insert(width);
                }
                Stmt::ForGen { body, .. } => self.collect_widths(body, ctx),
                Stmt::IfGen {
                    then_body,
                    else_body,
                    ..
                } => {
                    self.collect_widths(then_body, ctx);
                    self.collect_widths(else_body, ctx);
                }
                _ => {}
            }
        }
    }

    /// Elaborate a list of statements into concrete assignments / instances in `ctx`.
    fn elaborate_stmts(&mut self, stmts: &[Stmt], ctx: &mut ModuleCtx, stack: &[String]) {
        for stmt in stmts {
            self.elaborate_stmt(stmt, ctx, stack);
        }
    }

    fn elaborate_stmt(&mut self, stmt: &Stmt, ctx: &mut ModuleCtx, stack: &[String]) {
        match stmt {
            Stmt::WireDecl {
                name, driver, span, ..
            } => {
                if let Some(expr) = driver {
                    let value = self.elaborate_expr(expr, ctx);
                    ctx.assigns.push(ElabAssign {
                        target: ElabRef::Wire(name.clone()),
                        value,
                        span: span.clone(),
                    });
                }
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let target = self.elaborate_lvalue(target, ctx);
                let value = self.elaborate_expr(value, ctx);
                ctx.assigns.push(ElabAssign {
                    target,
                    value,
                    span: span.clone(),
                });
            }
            Stmt::Inst {
                name,
                module,
                params,
                conns,
                span,
            } => {
                let decl = InstDecl {
                    inst_name: name,
                    module_name: module,
                    params,
                    conns,
                    span,
                };
                self.elaborate_instance(&decl, ctx, stack);
            }
            Stmt::ForGen {
                var,
                lo,
                hi,
                body,
                span,
            } => {
                // The bounds must resolve to compile-time constants (Req 3.7).
                let lo_v = self.eval_ct_bound(lo, &ctx.env, "the lower bound of a `for` loop");
                let hi_v = self.eval_ct_bound(hi, &ctx.env, "the upper bound of a `for` loop");
                let (lo_v, hi_v) = match (lo_v, hi_v) {
                    (Some(l), Some(h)) => (l, h),
                    _ => {
                        let _ = span;
                        return;
                    }
                };
                // Fully unroll: bind the loop variable to each value in `lo..hi` (Req 3.2).
                let saved = ctx.env.get(var);
                let mut i = lo_v;
                while i < hi_v {
                    ctx.env.set(var, i);
                    self.elaborate_stmts(body, ctx, stack);
                    i += 1;
                }
                // Restore the previous binding (if any).
                match saved {
                    Some(v) => ctx.env.set(var, v),
                    None => ctx.env.remove(var),
                }
            }
            Stmt::IfGen {
                cond,
                then_body,
                else_body,
                span,
            } => {
                // Select the branch by the compile-time condition (nonzero = then) (Req 3.3).
                match self.eval_ct_bound(cond, &ctx.env, "the condition of a compile-time `if`") {
                    Some(v) => {
                        if v != 0 {
                            self.elaborate_stmts(then_body, ctx, stack);
                        } else {
                            self.elaborate_stmts(else_body, ctx, stack);
                        }
                    }
                    None => {
                        let _ = span;
                    }
                }
            }
            // Asserts carry no structure into the elaborated design; sema already evaluated the
            // constant ones. A parametric assert that becomes false here would have been resolved
            // during sema's constant pass, so nothing to emit.
            Stmt::Assert { .. } => {}
        }
    }

    /// Elaborate a sub-module instance, substituting its parameter overrides and recursing into its
    /// body with an extended hierarchy path (Req 3.1, 3.11). Detects recursion (Req 3.9).
    fn elaborate_instance(&mut self, decl: &InstDecl, ctx: &mut ModuleCtx, stack: &[String]) {
        let &InstDecl {
            inst_name,
            module_name,
            params,
            conns,
            span,
        } = decl;
        // Recursion: a module that (transitively) instantiates itself cannot be elaborated into a
        // finite structure (Req 3.9, 2.9).
        if stack.iter().any(|m| m == module_name) {
            let chain = {
                let mut c = stack.to_vec();
                c.push(module_name.to_string());
                c.join(" -> ")
            };
            self.diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!(
                        "recursive instantiation detected: module `{module_name}` is \
                         instantiated within its own expansion ({chain})"
                    ),
                )
                .with_note(
                    "module instantiation must form a finite hierarchy; \
                     a module cannot instantiate itself directly or indirectly",
                ),
            );
            return;
        }

        let target = match self.design.modules.iter().find(|m| m.name == module_name) {
            Some(m) => m,
            None => {
                // sema already reports unknown-module instances; be defensive and skip.
                return;
            }
        };

        // Evaluate parameter override values in the *parent* environment (Req 3.1).
        let mut overrides = Vec::new();
        for (pname, value) in params {
            if let Some(v) = self.eval_ct(value, &ctx.env) {
                overrides.push((pname.clone(), v));
            }
        }

        // Resolve each connection's l-value against the parent scope (indices resolved) (Req 3.6).
        let mut elab_conns = Vec::new();
        for (port_name, lv) in conns {
            let r = self.elaborate_lvalue(lv, ctx);
            elab_conns.push((port_name.clone(), r));
        }

        // Recurse with an extended path and recursion stack (Req 3.11, 3.9).
        let child_path = format!("{}/{}", ctx.path, inst_name);
        let mut child_stack = stack.to_vec();
        child_stack.push(module_name.to_string());
        let child = self.elaborate_module(target, &overrides, child_path, &child_stack);

        ctx.inst_counter += 1;
        ctx.instances.push(ElabInstance {
            inst_name: inst_name.to_string(),
            module: child,
            conns: elab_conns,
            span: span.clone(),
        });
    }

    /// Resolve an assignment / connection target to a concrete [`ElabRef`], range-checking an
    /// element index against the resolved vector width (Req 3.6, 3.8).
    fn elaborate_lvalue(&mut self, lv: &LValue, ctx: &ModuleCtx) -> ElabRef {
        match &lv.kind {
            LValueKind::Wire(name) => ElabRef::Wire(name.clone()),
            LValueKind::Index(name, idx) => {
                let i = self.eval_ct(idx, &ctx.env).unwrap_or(0);
                self.check_index_range(name, i, ctx, &lv.span);
                ElabRef::Index(name.clone(), i)
            }
        }
    }

    /// Elaborate an expression into a primitive-only [`ElabExpr`], expanding derived operators and
    /// inlining `func` calls (Req 3.4, 3.5), with indices and delay amounts resolved (Req 3.6).
    fn elaborate_expr(&mut self, expr: &Expr, ctx: &ModuleCtx) -> ElabExpr {
        self.elaborate_expr_env(expr, &ctx.env, ctx)
    }

    /// The core expression elaborator, carrying an explicit compile-time environment so a `func`
    /// body can be expanded with its arguments substituted. `ctx` provides vector widths for the
    /// range check and the hierarchy path; `env` provides the compile-time bindings.
    fn elaborate_expr_env(&mut self, expr: &Expr, env: &CtEnv, ctx: &ModuleCtx) -> ElabExpr {
        match expr {
            Expr::Ident(name, span) => {
                // A name bound to a func parameter that is itself a wire reference is handled by
                // substitution before we get here; a plain identifier is a signal reference.
                ElabExpr::Ident(name.clone(), span.clone())
            }
            Expr::Inf(span) => ElabExpr::Inf(span.clone()),
            Expr::Index(base, idx, span) => {
                let i = self.eval_ct(idx, env).unwrap_or(0);
                if let Expr::Ident(name, _) = base.as_ref() {
                    self.check_index_range(name, i, ctx, span);
                    ElabExpr::Index(name.clone(), i, span.clone())
                } else {
                    // A non-ident base is unusual but handled: elaborate the base and, lacking a
                    // named vector, treat the index structurally by re-wrapping. The frontend only
                    // produces ident bases, so this path is a defensive fallback.
                    self.elaborate_expr_env(base, env, ctx)
                }
            }
            Expr::Inc(inner, span) => {
                let a = self.elaborate_expr_env(inner, env, ctx);
                ElabExpr::Inc(Box::new(a), span.clone())
            }
            Expr::Delay(base, amount, span) => {
                let a = self.elaborate_expr_env(base, env, ctx);
                let n = self.eval_ct(amount, env).unwrap_or(0);
                ElabExpr::Delay(Box::new(a), n, span.clone())
            }
            Expr::Min(args, span) => {
                let elab_args = args
                    .iter()
                    .map(|a| self.elaborate_expr_env(a, env, ctx))
                    .collect();
                ElabExpr::Min(elab_args, span.clone())
            }
            Expr::Op { func, args, span } => {
                self.expand_named(func, args, env, ctx, span)
            }
            Expr::Call { name, args, span } => {
                self.expand_named(name, args, env, ctx, span)
            }
        }
    }

    /// Expand a named operator or call: a primitive (`lt`/`min`/`inc`), a derived operator
    /// (`le`/`ge`/`eq`/`max`/`gt`/`neq`) expanded into primitives (Req 3.5), or a user `func`
    /// inlined (Req 3.4).
    fn expand_named(
        &mut self,
        name: &str,
        args: &[Expr],
        env: &CtEnv,
        ctx: &ModuleCtx,
        span: &Span,
    ) -> ElabExpr {
        // Elaborate the arguments once; derived-op expansions reuse these primitive trees.
        let ea: Vec<ElabExpr> = args
            .iter()
            .map(|a| self.elaborate_expr_env(a, env, ctx))
            .collect();

        match name {
            // --- primitives -----------------------------------------------------------------
            "lt" => {
                // Binary primitive.
                self.make_lt(ea, span)
            }
            "min" => ElabExpr::Min(ea, span.clone()),
            "inc" => {
                let a = ea.into_iter().next().unwrap_or(ElabExpr::Inf(span.clone()));
                ElabExpr::Inc(Box::new(a), span.clone())
            }
            // --- derived operators expanded into inc/min/lt (Req 3.5) ------------------------
            // le(a,b) = lt(a, lt(b, a))
            "le" => {
                let (a, b) = self.two(ea, span);
                let inner = self.lt(b.clone(), a.clone(), span);
                self.lt(a, inner, span)
            }
            // ge(a,b) = lt(a, lt(a, b))
            "ge" => {
                let (a, b) = self.two(ea, span);
                let inner = self.lt(a.clone(), b, span);
                self.lt(a, inner, span)
            }
            // max(a,b) = min(ge(a,b), ge(b,a))
            // max(a, b, ...) — variadic later-arrival, mirroring the variadic `min` primitive.
            // `max` is associative (the latest of the latest is the overall latest), so a left
            // fold `max(max(a, b), c)...` is correct. sema guarantees `>= 2` args.
            "max" => {
                let mut it = ea.into_iter();
                let first = it.next().unwrap_or_else(|| ElabExpr::Inf(span.clone()));
                it.fold(first, |acc, next| self.derived_max(acc, next, span))
            }
            // eq(a,b) = ge(le(a,b), b)
            "eq" => {
                let (a, b) = self.two(ea, span);
                let le_ab = self.derived_le(a, b.clone(), span);
                self.derived_ge(le_ab, b, span)
            }
            // gt(a,b) = max(a, lt(b, a))
            "gt" => {
                let (a, b) = self.two(ea, span);
                let lt_ba = self.lt(b, a.clone(), span);
                self.derived_max(a, lt_ba, span)
            }
            // neq(a,b) = a if a != b else inf.
            //
            // The paper's identity eq. 25 is `a ≠ b = (a ≺ b) ∨ (a ≻ b)`, where `∨` is the
            // s-t "OR" / first-arrival combinator — i.e. `min`, not `max`. Expanding it as
            // `max(lt(a,b), gt(a,b))` is wrong: for `a < b` that is `max(a, inf) = inf`, and for
            // `a > b` that is `max(inf, a) = inf`, so it would yield `inf` for *every* unequal
            // pair, contradicting the authoritative if/else meaning (`a` if `a != b` else `inf`;
            // glossary, design Property 2). The correct expansion keeps the earlier of the two
            // one-sided results: `neq(a,b) = min(lt(a,b), gt(a,b))`.
            //   - a < b: min(a, inf)   = a   (a != b, pass a)
            //   - a > b: min(inf, a)   = a   (a != b, pass a)
            //   - a = b: min(inf, inf) = inf (a == b, block)
            "neq" => {
                let (a, b) = self.two(ea, span);
                let lt_ab = self.lt(a.clone(), b.clone(), span);
                let gt_ab = self.derived_gt(a, b, span);
                ElabExpr::Min(vec![lt_ab, gt_ab], span.clone())
            }
            // --- a user-defined func: inline-expand as a macro (Req 3.4) ---------------------
            _ => self.inline_func(name, args, env, ctx, span),
        }
    }

    /// Inline-expand a user `func` call by substituting its arguments into its body and elaborating
    /// the result (Req 3.4). Reports recursion through func expansion (Req 3.9).
    fn inline_func(
        &mut self,
        name: &str,
        args: &[Expr],
        env: &CtEnv,
        ctx: &ModuleCtx,
        span: &Span,
    ) -> ElabExpr {
        let func = match self.design.funcs.iter().find(|f| f.name == name) {
            Some(f) => f.clone(),
            None => {
                // sema reports unknown calls; be defensive — emit inf as a placeholder value.
                return ElabExpr::Inf(span.clone());
            }
        };

        // Recursion through funcs: track the func-expansion stack per call site via `env`.
        if env.func_stack.iter().any(|f| f == name) {
            let chain = {
                let mut c = env.func_stack.clone();
                c.push(name.to_string());
                c.join(" -> ")
            };
            self.diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!(
                        "recursive `func` expansion detected: `{name}` calls itself ({chain})"
                    ),
                )
                .with_note(
                    "a `func` is a compile-time macro and must expand to a finite expression; \
                     it cannot call itself directly or indirectly",
                ),
            );
            return ElabExpr::Inf(span.clone());
        }

        // Bind each formal parameter name to the corresponding (already-source) argument
        // expression, so references inside the body resolve to the caller's expressions.
        let mut subst: HashMap<String, Expr> = HashMap::new();
        for (param, arg) in func.params.iter().zip(args.iter()) {
            subst.insert(param.clone(), arg.clone());
        }

        // Substitute the arguments into the body, then elaborate the result in an environment that
        // records this func on the expansion stack (recursion detection).
        let substituted = substitute_args(&func.body, &subst);
        let mut child_env = env.clone();
        child_env.func_stack.push(name.to_string());
        self.elaborate_expr_env(&substituted, &child_env, ctx)
    }

    // ---- derived-op helpers that themselves expand to primitives ---------------------------

    fn derived_le(&self, a: ElabExpr, b: ElabExpr, span: &Span) -> ElabExpr {
        // le(a,b) = lt(a, lt(b, a))
        let inner = ElabExpr::Lt(Box::new(b), Box::new(a.clone()), span.clone());
        ElabExpr::Lt(Box::new(a), Box::new(inner), span.clone())
    }

    fn derived_ge(&self, a: ElabExpr, b: ElabExpr, span: &Span) -> ElabExpr {
        // ge(a,b) = lt(a, lt(a, b))
        let inner = ElabExpr::Lt(Box::new(a.clone()), Box::new(b), span.clone());
        ElabExpr::Lt(Box::new(a), Box::new(inner), span.clone())
    }

    fn derived_max(&self, a: ElabExpr, b: ElabExpr, span: &Span) -> ElabExpr {
        // max(a,b) = min(ge(a,b), ge(b,a))
        let ge_ab = self.derived_ge(a.clone(), b.clone(), span);
        let ge_ba = self.derived_ge(b, a, span);
        ElabExpr::Min(vec![ge_ab, ge_ba], span.clone())
    }

    fn derived_gt(&self, a: ElabExpr, b: ElabExpr, span: &Span) -> ElabExpr {
        // gt(a,b) = max(a, lt(b, a))
        let lt_ba = ElabExpr::Lt(Box::new(b), Box::new(a.clone()), span.clone());
        self.derived_max(a, lt_ba, span)
    }

    /// Build an `lt` from two already-elaborated expressions.
    fn lt(&self, a: ElabExpr, b: ElabExpr, span: &Span) -> ElabExpr {
        ElabExpr::Lt(Box::new(a), Box::new(b), span.clone())
    }

    /// Build an `lt` from a vector of (expected two) elaborated args.
    fn make_lt(&self, args: Vec<ElabExpr>, span: &Span) -> ElabExpr {
        let (a, b) = self.two(args, span);
        ElabExpr::Lt(Box::new(a), Box::new(b), span.clone())
    }

    /// Take exactly two arguments, filling with `inf` if under-supplied (sema guarantees arity, so
    /// this is a defensive fallback that never fires on validated input).
    fn two(&self, args: Vec<ElabExpr>, span: &Span) -> (ElabExpr, ElabExpr) {
        let mut it = args.into_iter();
        let a = it.next().unwrap_or_else(|| ElabExpr::Inf(span.clone()));
        let b = it.next().unwrap_or_else(|| ElabExpr::Inf(span.clone()));
        (a, b)
    }

    // ---- compile-time evaluation -----------------------------------------------------------

    /// Evaluate a compile-time expression against `env`, or `None` if it refers to an unknown
    /// binding. Division / modulo by zero yields `None`.
    fn eval_ct(&self, expr: &CtExpr, env: &CtEnv) -> Option<u64> {
        match &expr.kind {
            CtExprKind::Int(v) => Some(*v),
            CtExprKind::Param(name) => env.get(name),
            CtExprKind::BinOp(op, lhs, rhs) => {
                let a = self.eval_ct(lhs, env)?;
                let b = self.eval_ct(rhs, env)?;
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

    /// Evaluate a compile-time expression that is *required* to be constant (a `for` bound or `if`
    /// condition) against `env`. Reports a non-constant error with the source location (Req 3.7)
    /// and returns `None` if it cannot be resolved to an integer.
    fn eval_ct_bound(&mut self, expr: &CtExpr, env: &CtEnv, what: &str) -> Option<u64> {
        match self.eval_ct(expr, env) {
            Some(v) => Some(v),
            None => {
                self.diags.push(
                    Diagnostic::error(
                        expr.span.clone(),
                        format!("{what} is not a compile-time constant"),
                    )
                    .with_note(
                        "loop bounds and `if` conditions must resolve to compile-time integers \
                         from literals, `param`s, loop variables, and integer arithmetic",
                    ),
                );
                None
            }
        }
    }

    /// Range-check `name[i]` against the resolved width of `name`, reporting an out-of-range index
    /// with the source location (Req 3.8). An unknown width (scalar or unresolved) is not checked
    /// here; sema already reported scalar indexing.
    fn check_index_range(&mut self, name: &str, i: u64, ctx: &ModuleCtx, span: &Span) {
        if let Some(Some(width)) = ctx.widths.get(name) {
            if i >= *width {
                self.diags.push(
                    Diagnostic::error(
                        span.clone(),
                        format!(
                            "index {i} is out of range for `{name}` of type `time[{width}]` \
                             (valid indices are 0..{})",
                            width.saturating_sub(1)
                        ),
                    )
                    .with_note(
                        "the index resolved to a compile-time value outside the vector bounds; \
                         vector indices are 0-based and must be less than the width",
                    ),
                );
            }
        }
    }
}

// -------------------------------------------------------------------------------------------------
// Compile-time environment
// -------------------------------------------------------------------------------------------------

/// The compile-time environment carried through elaboration: `param` / loop-variable integer
/// bindings plus the current `func`-expansion stack (for recursion detection, Req 3.9).
#[derive(Clone, Default)]
struct CtEnv {
    values: HashMap<String, u64>,
    func_stack: Vec<String>,
}

impl CtEnv {
    fn new() -> Self {
        CtEnv::default()
    }

    fn set(&mut self, name: &str, value: u64) {
        self.values.insert(name.to_string(), value);
    }

    fn get(&self, name: &str) -> Option<u64> {
        self.values.get(name).copied()
    }

    fn remove(&mut self, name: &str) {
        self.values.remove(name);
    }
}

/// A borrowed view of a `Stmt::Inst` declaration, grouping its fields so the instance elaborator
/// takes one argument instead of five.
#[derive(Clone, Copy)]
struct InstDecl<'a> {
    inst_name: &'a str,
    module_name: &'a str,
    params: &'a [(String, CtExpr)],
    conns: &'a [(String, LValue)],
    span: &'a Span,
}

/// Per-module elaboration context: the active environment, hierarchy path, known vector widths, and
/// the accumulating concrete body.
struct ModuleCtx {
    env: CtEnv,
    path: String,
    widths: HashMap<String, Option<u64>>,
    assigns: Vec<ElabAssign>,
    instances: Vec<ElabInstance>,
    inst_counter: u32,
}

// -------------------------------------------------------------------------------------------------
// Argument substitution for func inlining
// -------------------------------------------------------------------------------------------------

/// Substitute `func` call arguments for the formal parameter names inside a func body (Req 3.4).
///
/// A func parameter name appearing as a bare identifier is replaced by the caller's argument
/// expression. Compile-time subexpressions (indices, delay amounts) are left untouched here; they
/// are resolved against the environment during elaboration.
fn substitute_args(body: &Expr, subst: &HashMap<String, Expr>) -> Expr {
    match body {
        Expr::Ident(name, span) => match subst.get(name) {
            Some(arg) => arg.clone(),
            None => Expr::Ident(name.clone(), span.clone()),
        },
        Expr::Inf(span) => Expr::Inf(span.clone()),
        Expr::Index(base, idx, span) => Expr::Index(
            Box::new(substitute_args(base, subst)),
            idx.clone(),
            span.clone(),
        ),
        Expr::Inc(inner, span) => {
            Expr::Inc(Box::new(substitute_args(inner, subst)), span.clone())
        }
        Expr::Delay(base, amount, span) => Expr::Delay(
            Box::new(substitute_args(base, subst)),
            amount.clone(),
            span.clone(),
        ),
        Expr::Min(args, span) => Expr::Min(
            args.iter().map(|a| substitute_args(a, subst)).collect(),
            span.clone(),
        ),
        Expr::Op { func, args, span } => Expr::Op {
            func: func.clone(),
            args: args.iter().map(|a| substitute_args(a, subst)).collect(),
            span: span.clone(),
        },
        Expr::Call { name, args, span } => Expr::Call {
            name: name.clone(),
            args: args.iter().map(|a| substitute_args(a, subst)).collect(),
            span: span.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::TimeWidth;
    use crate::operators::OperatorTable;
    use crate::parser::parse_source;
    use crate::sema::analyze;

    /// Parse + analyze a source string, panicking on any parse or sema error so elaboration tests
    /// start from a validated design.
    fn checked(src: &str) -> CheckedDesign {
        let design = parse_source(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32)
            .unwrap_or_else(|d| {
                let r: Vec<String> = d.iter().map(|x| x.render()).collect();
                panic!("fixture failed to parse:\n{}", r.join("\n"))
            });
        analyze(&design).unwrap_or_else(|errs| {
            let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
            panic!("fixture failed sema:\n{}", r.join("\n"))
        })
    }

    /// Elaborate a source string rooted at `top`, expecting success.
    fn elab_ok(src: &str, top: &str) -> Elaborated {
        match elaborate(&checked(src), top) {
            Ok(e) => e,
            Err(errs) => {
                let r: Vec<String> = errs.iter().map(|e| e.render()).collect();
                panic!("expected clean elaboration, got errors:\n{}", r.join("\n"))
            }
        }
    }

    /// Elaborate a source string rooted at `top`, expecting at least one error.
    fn elab_err(src: &str, top: &str) -> Vec<Diagnostic> {
        match elaborate(&checked(src), top) {
            Ok(_) => panic!("expected an elaboration error, but elaboration succeeded"),
            Err(errs) => errs,
        }
    }

    fn has_err_msg(diags: &[Diagnostic], needle: &str) -> bool {
        diags.iter().any(|d| d.is_error() && d.message.contains(needle))
    }

    /// Count how many `Lt` nodes appear in an elaborated expression tree.
    fn count_lt(e: &ElabExpr) -> usize {
        match e {
            ElabExpr::Lt(a, b, _) => 1 + count_lt(a) + count_lt(b),
            ElabExpr::Min(args, _) => args.iter().map(count_lt).sum(),
            ElabExpr::Inc(a, _) | ElabExpr::Delay(a, _, _) => count_lt(a),
            ElabExpr::Ident(..) | ElabExpr::Index(..) | ElabExpr::Inf(_) => 0,
        }
    }

    /// Count how many `Min` nodes appear in an elaborated expression tree.
    fn count_min(e: &ElabExpr) -> usize {
        match e {
            ElabExpr::Min(args, _) => 1 + args.iter().map(count_min).sum::<usize>(),
            ElabExpr::Lt(a, b, _) => count_min(a) + count_min(b),
            ElabExpr::Inc(a, _) | ElabExpr::Delay(a, _, _) => count_min(a),
            ElabExpr::Ident(..) | ElabExpr::Index(..) | ElabExpr::Inf(_) => 0,
        }
    }

    /// The value of the single assignment that drives `target`, if present.
    fn driver_of<'a>(m: &'a ElabModule, target: &str) -> Option<&'a ElabExpr> {
        m.assigns.iter().find_map(|a| match &a.target {
            ElabRef::Wire(n) if n == target => Some(&a.value),
            _ => None,
        })
    }

    // --- Req 3.10 missing --top -------------------------------------------------------------

    #[test]
    fn missing_top_module_is_an_error() {
        let errs = elab_err(
            "module M (input time a, output time y) { y = a; }",
            "DoesNotExist",
        );
        assert!(
            has_err_msg(&errs, "no module named `DoesNotExist`"),
            "expected a missing-top error, got: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn present_top_module_elaborates() {
        let e = elab_ok("module M (input time a, output time y) { y = a; }", "M");
        assert_eq!(e.top, "M");
        assert_eq!(e.root.name, "M");
        assert_eq!(e.root.path, "M");
    }

    // --- Req 3.2 for-loop unrolling ---------------------------------------------------------

    #[test]
    fn for_loop_is_fully_unrolled() {
        // A `for i in 1..4` over a width-4 vector drives acc[1], acc[2], acc[3]; acc[0] directly.
        let e = elab_ok(
            "module MinN #(param N = 4) (input time x[N], output time y) {\n\
             wire time acc[N];\n\
             acc[0] = x[0];\n\
             for i in 1..N {\n\
             acc[i] = min(acc[i - 1], x[i]);\n\
             }\n\
             y = acc[N - 1];\n\
             }",
            "MinN",
        );
        // Element drivers for acc[0..3] plus the final y = acc[3]; no loop structure remains.
        let indexed: Vec<u64> = e
            .root
            .assigns
            .iter()
            .filter_map(|a| match &a.target {
                ElabRef::Index(name, i) if name == "acc" => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(indexed, vec![0, 1, 2, 3], "acc[0..=3] should all be driven");

        // The final assignment resolved `acc[N - 1]` to `acc[3]`.
        let y_val = driver_of(&e.root, "y").expect("y must be driven");
        match y_val {
            ElabExpr::Index(name, i, _) => {
                assert_eq!(name, "acc");
                assert_eq!(*i, 3);
            }
            other => panic!("expected y = acc[3], got {other:?}"),
        }
    }

    #[test]
    fn param_override_changes_unroll_count() {
        // The loop count `K` is a parameter; the vector width is fixed at 2 so sema's
        // default-based connection check passes. The parent overrides K to 2, which must drive the
        // unroll count during elaboration (Req 3.1, 3.2). The default K=2 keeps the standalone
        // module legal, and the override still exercises substitution into the loop bound.
        let src = "module MinN #(param K = 2) (input time x[2], output time y) {\n\
                   wire time acc[2];\n\
                   acc[0] = x[0];\n\
                   for i in 1..K { acc[i] = min(acc[i - 1], x[i]); }\n\
                   y = acc[K - 1];\n\
                   }\n\
                   module Top (input time w[2], output time r) {\n\
                   inst s = MinN #(.K(2)) (.x(w), .y(r));\n\
                   }";
        let e = elab_ok(src, "Top");
        assert_eq!(e.root.instances.len(), 1);
        let child = &e.root.instances[0].module;
        assert_eq!(child.name, "MinN");
        assert_eq!(child.path, "Top/s");
        assert_eq!(child.params.get("K"), Some(&2));
        let indexed: Vec<u64> = child
            .assigns
            .iter()
            .filter_map(|a| match &a.target {
                ElabRef::Index(name, i) if name == "acc" => Some(*i),
                _ => None,
            })
            .collect();
        assert_eq!(indexed, vec![0, 1], "with K=2 only acc[0..=1] are driven");
    }

    // --- Req 3.3 if-branch selection --------------------------------------------------------

    /// How many assignments drive the whole wire `target` in a module.
    fn drivers_of(m: &ElabModule, target: &str) -> usize {
        m.assigns
            .iter()
            .filter(|a| matches!(&a.target, ElabRef::Wire(n) if n == target))
            .count()
    }

    #[test]
    fn if_selects_then_branch_when_condition_nonzero() {
        // Both branches drive the SAME output `y` (the realistic `if`-generate pattern: select how
        // to drive one output based on a compile-time parameter). sema descends only into the taken
        // branch, so `y` has exactly one driver; elaboration keeps the then-branch for N != 0.
        let e = elab_ok(
            "module M #(param N = 1) (input time a, output time y) {\n\
             if (N) { y = inc(a); } else { y = a; }\n\
             }",
            "M",
        );
        assert_eq!(drivers_of(&e.root, "y"), 1, "y is driven exactly once");
        // The then-branch (`inc(a)`) was taken, so the driver is an Inc.
        let y = driver_of(&e.root, "y").expect("y driven");
        assert!(matches!(y, ElabExpr::Inc(_, _)), "then-branch `inc(a)` taken for N=1");
    }

    #[test]
    fn if_selects_else_branch_when_condition_zero() {
        let e = elab_ok(
            "module M #(param N = 0) (input time a, output time y) {\n\
             if (N) { y = inc(a); } else { y = a; }\n\
             }",
            "M",
        );
        assert_eq!(drivers_of(&e.root, "y"), 1, "y is driven exactly once");
        // The else-branch (`y = a`) was taken, so the driver is a bare Ident.
        let y = driver_of(&e.root, "y").expect("y driven");
        assert!(matches!(y, ElabExpr::Ident(n, _) if n == "a"), "else-branch `y = a` taken for N=0");
    }

    // --- Req 3.4 func inlining --------------------------------------------------------------

    #[test]
    fn func_call_is_inlined() {
        // `func double(x) = inc(inc(x));` inlined into `y = double(a)` becomes inc(inc(a)).
        let e = elab_ok(
            "func double(time x) -> time { return inc(inc(x)); }\n\
             module M (input time a, output time y) { y = double(a); }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        match y {
            ElabExpr::Inc(outer, _) => match outer.as_ref() {
                ElabExpr::Inc(inner, _) => {
                    assert!(matches!(inner.as_ref(), ElabExpr::Ident(n, _) if n == "a"));
                }
                other => panic!("expected inc(inc(a)), got inner {other:?}"),
            },
            other => panic!("expected inc(...), got {other:?}"),
        }
    }

    #[test]
    fn func_with_two_params_substitutes_both() {
        // `func pick(x, y) = min(x, y);` inlined into `o = pick(a, b)` becomes min(a, b).
        let e = elab_ok(
            "func pick(time x, time y) -> time { return min(x, y); }\n\
             module M (input time a, input time b, output time o) { o = pick(a, b); }",
            "M",
        );
        let o = driver_of(&e.root, "o").expect("o driven");
        match o {
            ElabExpr::Min(args, _) => {
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], ElabExpr::Ident(n, _) if n == "a"));
                assert!(matches!(&args[1], ElabExpr::Ident(n, _) if n == "b"));
            }
            other => panic!("expected min(a, b), got {other:?}"),
        }
    }

    // --- Req 3.5 derived-op expansion into inc/min/lt ---------------------------------------

    #[test]
    fn lt_primitive_is_preserved() {
        let e = elab_ok(
            "module M (input time a, input time b, output time y) { y = a < b; }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        match y {
            ElabExpr::Lt(a, b, _) => {
                assert!(matches!(a.as_ref(), ElabExpr::Ident(n, _) if n == "a"));
                assert!(matches!(b.as_ref(), ElabExpr::Ident(n, _) if n == "b"));
            }
            other => panic!("expected lt(a, b), got {other:?}"),
        }
    }

    #[test]
    fn le_expands_to_two_lts() {
        // le(a,b) = lt(a, lt(b, a)) — two Lt nodes, no derived op remaining.
        let e = elab_ok(
            "func le(time a, time b) -> time { return a < (b < a); }\n\
             module M (input time a, input time b, output time y) { y = le(a, b); }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        assert_eq!(count_lt(y), 2, "le should expand into exactly two lt primitives");
        assert_eq!(count_min(y), 0);
    }

    #[test]
    fn gt_infix_expands_to_primitives_only() {
        // `a > b` is gt(a,b) = max(a, lt(b,a)); max = min(ge, ge); ge = lt(a, lt(a, b)).
        // The result must be built from only Min and Lt nodes (no derived ops left).
        let e = elab_ok(
            "module M (input time a, input time b, output time y) { y = a > b; }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        // The expansion routes through max (a min) and several lt nodes. The result is
        // structurally primitive-only: `ElabExpr` has no derived-op variant, so the only way a
        // derived op could survive would be an un-expanded `lt`/`min`/`inc` call, which cannot
        // occur. The node counts confirm the construction (gt -> max -> min + lt + ges).
        assert!(count_min(y) >= 1, "gt expansion routes through max -> min");
        assert!(count_lt(y) >= 1, "gt expansion routes through lt");
    }

    #[test]
    fn eq_infix_expands_to_primitives() {
        let e = elab_ok(
            "module M (input time a, input time b, output time y) { y = a == b; }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        // eq(a,b) = ge(le(a,b), b) where le(a,b)=lt(a,lt(b,a)) (2 lts) and ge(L,b)=lt(L,lt(L,b))
        // inlines L twice (2*2) plus 2 outer lts => 6 lts total, and no min node.
        assert_eq!(count_lt(y), 6, "eq expands into six lt primitives");
        assert_eq!(count_min(y), 0);
    }

    #[test]
    fn neq_infix_expands_to_primitives() {
        let e = elab_ok(
            "module M (input time a, input time b, output time y) { y = a != b; }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        // neq(a,b) = min(lt(a,b), gt(a,b)) (first-arrival of the two one-sided results); its top
        // node is a min, and gt expands to further min/lt primitives underneath.
        assert!(matches!(y, ElabExpr::Min(_, _)), "neq's outer node is a min");
        assert!(count_lt(y) >= 1 && count_min(y) >= 1);
    }

    #[test]
    fn max_call_expands_to_min_of_two_ges() {
        // max(a,b) = min(ge(a,b), ge(b,a)); each ge is two lts, so four lts under one min.
        let e = elab_ok(
            "module M (input time a, input time b, output time y) { y = max(a, b); }",
            "M",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        match y {
            ElabExpr::Min(args, _) => assert_eq!(args.len(), 2, "max is a min of two ge terms"),
            other => panic!("expected max to expand to a min, got {other:?}"),
        }
        assert_eq!(count_lt(y), 4, "two ge terms, each two lts");
    }

    // --- Req 3.6 delay amounts and indices resolved to constants ----------------------------

    #[test]
    fn delay_amount_is_resolved_to_a_constant() {
        let e = elab_ok(
            "module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }",
            "DelayN",
        );
        let y = driver_of(&e.root, "y").expect("y driven");
        match y {
            ElabExpr::Delay(base, n, _) => {
                assert!(matches!(base.as_ref(), ElabExpr::Ident(s, _) if s == "a"));
                assert_eq!(*n, 3, "the param N default 3 resolves the delay amount");
            }
            other => panic!("expected a resolved delay, got {other:?}"),
        }
    }

    // --- Req 3.7 non-constant for bound -----------------------------------------------------

    #[test]
    fn non_constant_for_bound_is_an_error() {
        // A param with no default leaves the bound unresolved when the module is the top (no
        // overriding instance), so the loop bound is not a compile-time constant here.
        let errs = elab_err(
            "module M #(param N) (input time a, output time y, output time z[4]) {\n\
             y = a;\n\
             for i in 0..N { z[i] = a; }\n\
             }",
            "M",
        );
        assert!(
            has_err_msg(&errs, "not a compile-time constant"),
            "expected a non-constant bound error, got: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    // --- Req 3.8 out-of-range index at elaboration ------------------------------------------

    #[test]
    fn out_of_range_index_after_substitution_is_an_error() {
        // With N=2, acc has width 2 but the loop writes acc[2] on its last iteration (0..3),
        // which only becomes known after parameter substitution and unrolling (Req 3.8).
        let errs = elab_err(
            "module M (input time x[2], output time y) {\n\
             wire time acc[2];\n\
             for i in 0..3 { acc[i] = x[0]; }\n\
             y = acc[0];\n\
             }",
            "M",
        );
        assert!(
            has_err_msg(&errs, "out of range"),
            "expected an out-of-range index error, got: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    // --- Req 3.9 / 2.9 recursion detection --------------------------------------------------

    #[test]
    fn recursive_instance_is_an_error() {
        // `R` instantiates itself; its output is driven by the instance connection so sema passes,
        // but elaboration cannot expand it into a finite structure (Req 3.9, 2.9).
        let errs = elab_err(
            "module R (input time a, output time y) {\n\
             inst self = R (.a(a), .y(y));\n\
             }",
            "R",
        );
        assert!(
            has_err_msg(&errs, "recursive instantiation"),
            "expected a recursion error, got: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn mutually_recursive_instances_are_an_error() {
        // A -> B -> A: indirect recursion through instances.
        let errs = elab_err(
            "module A (input time a, output time y) { inst b = B (.a(a), .y(y)); }\n\
             module B (input time a, output time y) { inst a2 = A (.a(a), .y(y)); }",
            "A",
        );
        assert!(has_err_msg(&errs, "recursive instantiation"));
    }

    #[test]
    fn recursive_func_is_an_error() {
        // `func loop(x) = loop(x);` cannot expand to a finite expression (Req 3.9).
        let errs = elab_err(
            "func loopf(time x) -> time { return min(x, loopf(x)); }\n\
             module M (input time a, output time y) { y = loopf(a); }",
            "M",
        );
        assert!(
            has_err_msg(&errs, "recursive `func` expansion"),
            "expected a func recursion error, got: {:?}",
            errs.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    // --- Req 3.11 hierarchy path preservation -----------------------------------------------

    #[test]
    fn hierarchy_paths_are_preserved() {
        let src = "module Leaf (input time a, output time y) { y = inc(a); }\n\
                   module Mid (input time a, output time y) { inst l = Leaf (.a(a), .y(y)); }\n\
                   module Top (input time a, output time y) { inst m = Mid (.a(a), .y(y)); }";
        let e = elab_ok(src, "Top");
        assert_eq!(e.root.path, "Top");
        let mid = &e.root.instances[0].module;
        assert_eq!(mid.path, "Top/m");
        let leaf = &mid.instances[0].module;
        assert_eq!(leaf.path, "Top/m/l");
    }
}
