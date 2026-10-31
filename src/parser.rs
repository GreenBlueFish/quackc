//! Parser: tokens to AST, precedence-climbing driven by the operator table (Requirement 1).
//!
//! Converts a `&[Token]` (produced by [`crate::lexer`]) into an [`ast::Design`]. The expression
//! grammar is a precedence-climbing parser whose infix operators and their precedence /
//! associativity come entirely from the centralized [`OperatorTable`] (Req 1.15), so adding a new
//! infix operator is a table entry plus a `std.sta` definition — no parser change (Req 14.3). The
//! relational operators are non-associative, so chaining `a < b < c` without parentheses is a
//! parse error (Req 1.16).
//!
//! Every parse error carries a `file:line:col` location and a plain-language "expected ..."
//! message (Req 1.4, 1.26, 13.1, 13.2). The parser recovers at statement / declaration boundaries
//! where it reasonably can so a single run reports more than one error, but a single clear error is
//! acceptable where recovery is hard.
//!
//! Grammar overview (surface forms, from the project prompt Section 3-4):
//!
//! ```text
//! design      := (import | module | func | test)*
//! import      := 'import' <string-ident-or-path> ';'
//! module      := 'module' Ident paramlist? '(' ports? ')' '{' stmt* '}'
//! paramlist   := '#' '(' 'param' Ident ('=' ctexpr)? (',' 'param' Ident ('=' ctexpr)?)* ')'
//! ports       := port (',' port)*
//! port        := ('input'|'output') 'time' Ident ('[' ctexpr ']')?
//! stmt        := wiredecl | assign | inst | forgen | ifgen | assert
//! wiredecl    := 'wire' 'time' Ident ('[' ctexpr ']')? ('=' expr)? ';'
//! assign      := lvalue '=' expr ';'
//! inst        := 'inst' Ident '=' Ident ('#' '(' instparam* ')')? '(' instconn* ')' ';'
//! forgen      := 'for' Ident 'in' ctexpr '..' ctexpr '{' stmt* '}'
//! ifgen       := 'if' '(' ctexpr ')' '{' stmt* '}' ('else' '{' stmt* '}')?
//! assert      := 'assert' ctexpr ';'
//! func        := 'func' Ident '(' funcparams? ')' '->' 'time' '{' 'return' expr ';' '}'
//! test        := 'test' Ident 'for' Ident '{' wave* '}'
//! wave        := 'wave' '{' (assignstim)* ('expect' assignstim)* '}'   (interleaved)
//! ```
//!
//! Note the `if`/`else` compile-time conditional (Req 1.19) reuses the lexer's existing tokens; the
//! lexer does not reserve `if`/`else` as keywords, so they arrive as `Ident("if")` / `Ident("else")`
//! and the parser treats them specially at statement position.

use crate::ast::*;
use crate::diag::{Diagnostic, Span};
use crate::lexer::{self, Token, TokenKind, TimeWidth};
use crate::operators::{Assoc, OperatorTable};

/// Parse a token stream into an [`ast::Design`].
///
/// On success returns the design; otherwise returns every parse error collected in one pass. Each
/// diagnostic carries a `file:line:col` location and a plain-language expected-construct message
/// (Req 1.26).
pub fn parse(
    tokens: &[Token],
    file: &str,
    ops: &OperatorTable,
) -> Result<Design, Vec<Diagnostic>> {
    let mut parser = Parser::new(tokens, file, ops);
    let design = parser.parse_design();
    if parser.diags.is_empty() {
        Ok(design)
    } else {
        Err(parser.diags)
    }
}

/// Convenience: lex then parse a source string with the builtin operator table and the given
/// `SpikeTime` width. Lexer errors and parser errors are both returned as `Vec<Diagnostic>`.
pub fn parse_source(
    src: &str,
    file: &str,
    ops: &OperatorTable,
    width: TimeWidth,
) -> Result<Design, Vec<Diagnostic>> {
    let tokens = lexer::tokenize(src, file, ops, width)?;
    parse(&tokens, file, ops)
}

/// A recoverable parse error carrying its diagnostic. Thrown internally with `?` and caught at
/// recovery points (statement / declaration boundaries) so the parser can keep going.
struct ParseError(Diagnostic);

type PResult<T> = Result<T, ParseError>;

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
    file: &'a str,
    ops: &'a OperatorTable,
    diags: Vec<Diagnostic>,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [Token], file: &'a str, ops: &'a OperatorTable) -> Self {
        Parser {
            tokens,
            pos: 0,
            file,
            ops,
            diags: Vec::new(),
        }
    }

    // ---- Token cursor helpers -------------------------------------------------------------

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn peek_kind(&self) -> Option<&TokenKind> {
        self.peek().map(|t| &t.kind)
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    /// Advance one token, returning it.
    fn bump(&mut self) -> Option<&Token> {
        let tok = self.tokens.get(self.pos);
        if tok.is_some() {
            self.pos += 1;
        }
        tok
    }

    /// The span at the current token, or a synthetic end-of-input span derived from the last token
    /// (or `file:1:1` for an empty input) so every error has a location (Req 1.26).
    fn cur_span(&self) -> Span {
        if let Some(tok) = self.peek() {
            tok.span.clone()
        } else if let Some(last) = self.tokens.last() {
            // Point just past the last token's starting column.
            Span::new(self.file, last.span.line, last.span.col.saturating_add(1).max(1))
        } else {
            Span::new(self.file, 1, 1)
        }
    }

    /// Build an "expected X, found Y" parse error at the current position (Req 1.26).
    fn err_expected(&self, what: &str) -> ParseError {
        let found = self.describe_current();
        ParseError(Diagnostic::error(
            self.cur_span(),
            format!("expected {what}, found {found}"),
        ))
    }

    /// Build a parse error carrying an explicit message at a given span (for semantic parse checks
    /// like a `cases` length mismatch that are not "expected X, found Y").
    fn err_at(&self, span: &Span, message: &str) -> ParseError {
        ParseError(Diagnostic::error(span.clone(), message.to_string()))
    }

    /// A plain-language description of the current token for error messages.
    fn describe_current(&self) -> String {
        match self.peek_kind() {
            None => "end of input".to_string(),
            Some(k) => describe_kind(k),
        }
    }

    /// Record a diagnostic without stopping (used after catching a recoverable error).
    fn push_err(&mut self, err: ParseError) {
        self.diags.push(err.0);
    }

    // ---- Token matching -------------------------------------------------------------------

    /// Consume the next token if its kind equals `kind`; otherwise error with `what`.
    fn expect(&mut self, kind: &TokenKind, what: &str) -> PResult<&Token> {
        if self.peek_kind() == Some(kind) {
            Ok(self.bump().expect("peek matched"))
        } else {
            Err(self.err_expected(what))
        }
    }

    /// If the next token has `kind`, consume it and return true.
    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.peek_kind() == Some(kind) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Expect and consume an identifier, returning its text and span.
    fn expect_ident(&mut self, what: &str) -> PResult<(String, Span)> {
        match self.peek_kind() {
            Some(TokenKind::Ident(name)) => {
                let name = name.clone();
                let span = self.peek().unwrap().span.clone();
                self.pos += 1;
                Ok((name, span))
            }
            _ => Err(self.err_expected(what)),
        }
    }

    /// True if the current `Ident` token has exactly this text (for soft keywords `if`/`else`).
    fn is_ident_kw(&self, kw: &str) -> bool {
        matches!(self.peek_kind(), Some(TokenKind::Ident(s)) if s == kw)
    }

    // ---- Top level ------------------------------------------------------------------------

    fn parse_design(&mut self) -> Design {
        let mut design = Design {
            modules: Vec::new(),
            funcs: Vec::new(),
            tests: Vec::new(),
            imports: Vec::new(),
            includes: Vec::new(),
        };

        while !self.is_eof() {
            let start = self.pos;
            let result = match self.peek_kind() {
                Some(TokenKind::KwImport) => self.parse_import().map(Item::Import),
                Some(TokenKind::KwInclude) => self.parse_include().map(Item::Include),
                Some(TokenKind::KwModule) => self.parse_module().map(Item::Module),
                Some(_) if self.is_ident_kw("func") => self.parse_func().map(Item::Func),
                Some(TokenKind::KwTest) => self.parse_test().map(Item::Test),
                Some(TokenKind::KwCases) => self.parse_cases().map(Item::Test),
                _ => Err(self.err_expected(
                    "a top-level `import`, `include`, `module`, `func`, `test`, or `cases` declaration",
                )),
            };

            match result {
                Ok(Item::Import(i)) => design.imports.push(i),
                Ok(Item::Include(i)) => design.includes.push(i),
                Ok(Item::Module(m)) => design.modules.push(m),
                Ok(Item::Func(f)) => design.funcs.push(f),
                Ok(Item::Test(t)) => design.tests.push(t),
                Err(e) => {
                    self.push_err(e);
                    self.recover_to_top_level();
                }
            }

            // Guard against zero-progress loops: if nothing was consumed, skip one token.
            if self.pos == start && !self.is_eof() {
                self.pos += 1;
            }
        }

        design
    }

    /// Skip tokens until the start of the next top-level declaration so one bad declaration does
    /// not poison the rest of the file.
    fn recover_to_top_level(&mut self) {
        while let Some(k) = self.peek_kind() {
            match k {
                TokenKind::KwImport | TokenKind::KwInclude | TokenKind::KwModule | TokenKind::KwTest | TokenKind::KwCases => return,
                _ if self.is_ident_kw("func") => return,
                _ => {
                    self.pos += 1;
                }
            }
        }
    }

    // ---- import ---------------------------------------------------------------------------

    fn parse_import(&mut self) -> PResult<Import> {
        let span = self.expect(&TokenKind::KwImport, "`import`")?.span.clone();
        // The lexer has no string literal; an import target is written as a bare identifier path
        // such as `std` or a dotted name `std.sta`. Accept an identifier followed by optional
        // `.ident` segments.
        let (first, _) = self.expect_ident("an import target name after `import`")?;
        let mut path = first;
        while self.eat(&TokenKind::Dot) {
            let (seg, _) = self.expect_ident("a path segment after `.` in an import")?;
            path.push('.');
            path.push_str(&seg);
        }
        self.expect(&TokenKind::Semi, "`;` to end the import statement")?;
        Ok(Import { path, span })
    }

    /// `include helpers/math.sta;` — a user-file include, resolved relative to the including file's
    /// directory (unlike `import`, which uses the library search path). The path is a bare token
    /// sequence allowing identifiers, `.`, `/`, and `-` so subfolders and extensions are writable
    /// without string literals.
    fn parse_include(&mut self) -> PResult<Import> {
        let span = self.expect(&TokenKind::KwInclude, "`include`")?.span.clone();
        let (first, _) = self.expect_ident("an include path after `include`")?;
        let mut path = first;
        // Accumulate following `.seg`, `/seg`, and `-` runs until the terminating `;`.
        loop {
            match self.peek_kind() {
                Some(TokenKind::Dot) => {
                    self.bump();
                    let (seg, _) = self.expect_ident("a path segment after `.` in an include")?;
                    path.push('.');
                    path.push_str(&seg);
                }
                Some(TokenKind::Slash) => {
                    self.bump();
                    let (seg, _) = self.expect_ident("a path segment after `/` in an include")?;
                    path.push('/');
                    path.push_str(&seg);
                }
                Some(TokenKind::Minus) => {
                    self.bump();
                    let (seg, _) = self.expect_ident("a path segment after `-` in an include")?;
                    path.push('-');
                    path.push_str(&seg);
                }
                _ => break,
            }
        }
        self.expect(&TokenKind::Semi, "`;` to end the include statement")?;
        Ok(Import { path, span })
    }

    // ---- module ---------------------------------------------------------------------------

    fn parse_module(&mut self) -> PResult<Module> {
        let span = self.expect(&TokenKind::KwModule, "`module`")?.span.clone();
        let (name, _) = self.expect_ident("a module name after `module`")?;

        let params = if self.peek_kind() == Some(&TokenKind::Hash) {
            self.parse_param_list()?
        } else {
            Vec::new()
        };

        self.expect(&TokenKind::LParen, "`(` to begin the module's port list")?;
        let ports = self.parse_ports()?;
        self.expect(&TokenKind::RParen, "`)` to end the module's port list")?;

        self.expect(&TokenKind::LBrace, "`{` to begin the module body")?;
        let body = self.parse_stmt_block()?;

        Ok(Module {
            name,
            params,
            ports,
            body,
            span,
        })
    }

    /// `#(param N = default, param M, ...)` — a module parameter list (Req 1.17).
    fn parse_param_list(&mut self) -> PResult<Vec<Param>> {
        self.expect(&TokenKind::Hash, "`#` to begin a parameter list")?;
        self.expect(&TokenKind::LParen, "`(` after `#` in a parameter list")?;
        let mut params = Vec::new();
        if self.peek_kind() != Some(&TokenKind::RParen) {
            loop {
                let span = self
                    .expect(&TokenKind::KwParam, "`param` in the parameter list")?
                    .span
                    .clone();
                let (name, _) = self.expect_ident("a parameter name after `param`")?;
                let default = if self.eat(&TokenKind::Eq) {
                    Some(self.parse_ctexpr()?)
                } else {
                    None
                };
                params.push(Param {
                    name,
                    default,
                    span,
                });
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)` to end the parameter list")?;
        Ok(params)
    }

    /// Parse zero or more comma-separated ports.
    fn parse_ports(&mut self) -> PResult<Vec<Port>> {
        let mut ports = Vec::new();
        if self.peek_kind() == Some(&TokenKind::RParen) {
            return Ok(ports);
        }
        // Grouped port declarations (Req: multiple ports per direction/type): the first entry must
        // begin with `input`/`output` and a type; a following entry that is a bare name reuses the
        // current direction and type (`input time a, b` == `input time a, input time b`). A new
        // `input`/`output` starts a fresh group with its own type.
        let mut cur_dir: Option<Dir> = None;
        let mut cur_ty: Option<Ty> = None;
        loop {
            match self.peek_kind() {
                Some(TokenKind::KwInput) => {
                    let span = self.bump().unwrap().span.clone();
                    // The group's BASE type is the scalar `time` (or `time` before any per-name
                    // `[N]`), so a per-name suffix like `a[4]` does not leak to later bare names.
                    let base = self.parse_type_with_name()?;
                    cur_dir = Some(Dir::Input);
                    cur_ty = Some(base.clone());
                    ports.push(self.finish_port(Dir::Input, base, span)?);
                }
                Some(TokenKind::KwOutput) => {
                    let span = self.bump().unwrap().span.clone();
                    let base = self.parse_type_with_name()?;
                    cur_dir = Some(Dir::Output);
                    cur_ty = Some(base.clone());
                    ports.push(self.finish_port(Dir::Output, base, span)?);
                }
                Some(TokenKind::Ident(_)) => {
                    // A bare name continues the current group, reusing its direction and type.
                    let (dir, ty) = match (cur_dir, &cur_ty) {
                        (Some(d), Some(t)) => (d, t.clone()),
                        _ => {
                            return Err(self.err_expected(
                                "`input` or `output` to begin the port list",
                            ))
                        }
                    };
                    ports.push(self.finish_port(dir, ty, self.cur_span())?);
                }
                _ => return Err(self.err_expected("`input` or `output` to begin a port declaration")),
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        Ok(ports)
    }

    /// Finish one port given its (already determined) direction and base type: read the port name
    /// and an optional per-name `[N]` vector suffix. The suffix attaches to THIS name only, so
    /// `input time a[4], b` gives `a : time[4]` and `b : time[4]` is NOT implied — `b` takes the
    /// group's base type. (A per-name suffix overrides the base for that one name.)
    fn finish_port(&mut self, dir: Dir, base_ty: Ty, span: Span) -> PResult<Port> {
        let (name, name_span) = self.expect_ident("a port name")?;
        let ty = self.maybe_vector_suffix(base_ty, &name_span)?;
        Ok(Port {
            dir,
            ty,
            name,
            span,
        })
    }

    /// Expect the `time` keyword (written as an identifier `time`) and return a scalar `Ty`. The
    /// vector suffix `[N]` is attached afterward once the name is read.
    fn parse_type_with_name(&mut self) -> PResult<Ty> {
        match self.peek_kind() {
            Some(TokenKind::Ident(s)) if s == "time" => {
                let span = self.bump().unwrap().span.clone();
                Ok(Ty::time(span))
            }
            _ => Err(self.err_expected("the type `time`")),
        }
    }

    /// If a `[ctexpr]` follows, turn a scalar `time` type into a `time[N]` vector type whose span is
    /// the type keyword's span.
    fn maybe_vector_suffix(&mut self, ty: Ty, _name_span: &Span) -> PResult<Ty> {
        if self.peek_kind() == Some(&TokenKind::LBrack) {
            let ty_span = ty.span.clone();
            self.bump(); // '['
            let n = self.parse_ctexpr()?;
            self.expect(&TokenKind::RBrack, "`]` to close the vector size")?;
            Ok(Ty::time_vec(n, ty_span))
        } else {
            Ok(ty)
        }
    }

    // ---- statements -----------------------------------------------------------------------

    /// Parse statements until the closing `}` of a block, consuming the `}`.
    fn parse_stmt_block(&mut self) -> PResult<Vec<Stmt>> {
        let mut stmts = Vec::new();
        loop {
            match self.peek_kind() {
                Some(TokenKind::RBrace) => {
                    self.bump();
                    break;
                }
                None => {
                    return Err(self.err_expected("`}` to close the block"));
                }
                _ => {}
            }
            let start = self.pos;
            match self.parse_stmt() {
                // A single source statement may expand to several AST statements (a grouped
                // `wire time a, b;` declaration becomes one `WireDecl` per name).
                Ok(mut s) => stmts.append(&mut s),
                Err(e) => {
                    self.push_err(e);
                    self.recover_to_stmt_end();
                }
            }
            if self.pos == start && !self.is_eof() {
                self.pos += 1;
            }
        }
        Ok(stmts)
    }

    /// Skip to just past the next `;`, or stop at a `}` so block recovery still works.
    fn recover_to_stmt_end(&mut self) {
        while let Some(k) = self.peek_kind() {
            match k {
                TokenKind::Semi => {
                    self.pos += 1;
                    return;
                }
                TokenKind::RBrace => return,
                _ => self.pos += 1,
            }
        }
    }

    fn parse_stmt(&mut self) -> PResult<Vec<Stmt>> {
        match self.peek_kind() {
            // Grouped wire declarations may yield more than one statement.
            Some(TokenKind::KwWire) => self.parse_wire_decl(),
            Some(TokenKind::KwInst) => self.parse_inst().map(|s| vec![s]),
            Some(TokenKind::KwFor) => self.parse_for_gen().map(|s| vec![s]),
            Some(TokenKind::KwAssert) => self.parse_assert().map(|s| vec![s]),
            _ if self.is_ident_kw("if") => self.parse_if_gen().map(|s| vec![s]),
            Some(TokenKind::Ident(_)) => self.parse_assign().map(|s| vec![s]),
            _ => Err(self.err_expected(
                "a statement (`wire`, assignment, `inst`, `for`, `if`, or `assert`)",
            )),
        }
    }

    /// `wire time x;`, `wire time x = <expr>;`, `wire time v[N];`, or a grouped
    /// `wire time a, b, c;` (each name becomes its own undriven wire of the group's base type;
    /// a per-name `[N]` suffix is local to that name). An inline `= <expr>` driver is only allowed
    /// for a SINGLE wire; a comma list must be undriven (drive the others on their own lines).
    /// Returns one [`Stmt::WireDecl`] per declared name (Req 1.9).
    fn parse_wire_decl(&mut self) -> PResult<Vec<Stmt>> {
        let span = self.expect(&TokenKind::KwWire, "`wire`")?.span.clone();
        // The base type (`time`) applies to every name in the group; a per-name `[N]` suffix below
        // overrides it for that one name only.
        let base_ty = self.parse_type_with_name()?;

        // First name.
        let (first_name, first_span) = self.expect_ident("a wire name after `wire time`")?;
        let first_ty = self.maybe_vector_suffix(base_ty.clone(), &first_span)?;

        // Case 1: single wire with an optional inline driver.
        if self.eat(&TokenKind::Eq) {
            let driver = self.parse_expr()?;
            self.expect(&TokenKind::Semi, "`;` to end the wire declaration")?;
            return Ok(vec![Stmt::WireDecl {
                ty: first_ty,
                name: first_name,
                driver: Some(driver),
                span,
            }]);
        }

        // Case 2: one or more names (grouped), all undriven.
        let mut decls = vec![Stmt::WireDecl {
            ty: first_ty,
            name: first_name,
            driver: None,
            span: span.clone(),
        }];
        while self.eat(&TokenKind::Comma) {
            let (name, name_span) = self.expect_ident("a wire name after `,`")?;
            let ty = self.maybe_vector_suffix(base_ty.clone(), &name_span)?;
            // A driver is not permitted inside a grouped declaration (ambiguous); only the
            // single-wire form above may be driven inline.
            if self.peek_kind() == Some(&TokenKind::Eq) {
                return Err(self.err_expected(
                    "`,` or `;` (an inline `= driver` is only allowed for a single `wire`, not a \
                     comma-separated group; drive the extra wires on their own lines)",
                ));
            }
            decls.push(Stmt::WireDecl {
                ty,
                name,
                driver: None,
                span: span.clone(),
            });
        }
        self.expect(&TokenKind::Semi, "`;` to end the wire declaration")?;
        Ok(decls)
    }

    /// `target = value;` where `target` is a wire or `wire[i]` l-value (Req 1.9, 1.10).
    fn parse_assign(&mut self) -> PResult<Stmt> {
        let target = self.parse_lvalue()?;
        let span = target.span.clone();
        self.expect(&TokenKind::Eq, "`=` in the assignment")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "`;` to end the assignment")?;
        Ok(Stmt::Assign {
            target,
            value,
            span,
        })
    }

    /// An l-value: a whole wire `name` or an element `name[i]` (Req 1.10).
    fn parse_lvalue(&mut self) -> PResult<LValue> {
        let (name, span) = self.expect_ident("a wire name on the left of `=`")?;
        if self.peek_kind() == Some(&TokenKind::LBrack) {
            self.bump(); // '['
            let idx = self.parse_ctexpr()?;
            self.expect(&TokenKind::RBrack, "`]` to close the index")?;
            Ok(LValue::index(name, idx, span))
        } else {
            Ok(LValue::wire(name, span))
        }
    }

    /// `inst u0 = Name #(.N(v))? (.port(wire), ...);` — a sub-module instantiation (Req 1.20).
    fn parse_inst(&mut self) -> PResult<Stmt> {
        let span = self.expect(&TokenKind::KwInst, "`inst`")?.span.clone();
        let (name, _) = self.expect_ident("an instance name after `inst`")?;
        self.expect(&TokenKind::Eq, "`=` after the instance name")?;
        let (module, _) = self.expect_ident("the name of the module being instantiated")?;

        // Optional parameter overrides: `#(.N(value), ...)`.
        let mut params = Vec::new();
        if self.peek_kind() == Some(&TokenKind::Hash) {
            self.bump(); // '#'
            self.expect(&TokenKind::LParen, "`(` after `#` in a parameter override")?;
            if self.peek_kind() != Some(&TokenKind::RParen) {
                loop {
                    self.expect(&TokenKind::Dot, "`.` to begin a parameter override")?;
                    let (pname, _) = self.expect_ident("a parameter name after `.`")?;
                    self.expect(&TokenKind::LParen, "`(` after the parameter name")?;
                    let value = self.parse_ctexpr()?;
                    self.expect(&TokenKind::RParen, "`)` to close the parameter value")?;
                    params.push((pname, value));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            self.expect(&TokenKind::RParen, "`)` to end the parameter overrides")?;
        }

        // Port connections: `(.port(wire), ...)`.
        self.expect(&TokenKind::LParen, "`(` to begin the port connections")?;
        let mut conns = Vec::new();
        if self.peek_kind() != Some(&TokenKind::RParen) {
            loop {
                self.expect(&TokenKind::Dot, "`.` to begin a port connection")?;
                let (pname, _) = self.expect_ident("a port name after `.`")?;
                self.expect(&TokenKind::LParen, "`(` after the port name")?;
                let lv = self.parse_lvalue()?;
                self.expect(&TokenKind::RParen, "`)` to close the connection")?;
                conns.push((pname, lv));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)` to end the port connections")?;
        self.expect(&TokenKind::Semi, "`;` to end the instantiation")?;

        Ok(Stmt::Inst {
            name,
            module,
            params,
            conns,
            span,
        })
    }

    /// `for i in lo..hi { ... }` — a compile-time generate loop (Req 1.18).
    fn parse_for_gen(&mut self) -> PResult<Stmt> {
        let span = self.expect(&TokenKind::KwFor, "`for`")?.span.clone();
        let (var, _) = self.expect_ident("a loop variable after `for`")?;
        self.expect(&TokenKind::KwIn, "`in` after the loop variable")?;
        let lo = self.parse_ctexpr()?;
        self.expect(&TokenKind::DotDot, "`..` between the loop bounds")?;
        let hi = self.parse_ctexpr()?;
        self.expect(&TokenKind::LBrace, "`{` to begin the loop body")?;
        let body = self.parse_stmt_block()?;
        Ok(Stmt::ForGen {
            var,
            lo,
            hi,
            body,
            span,
        })
    }

    /// `if (cond) { ... } else { ... }` — a compile-time conditional (Req 1.19). `if`/`else` are
    /// soft keywords (identifiers to the lexer).
    fn parse_if_gen(&mut self) -> PResult<Stmt> {
        // Consume the `if` identifier.
        let span = self.peek().expect("if present").span.clone();
        self.pos += 1;
        self.expect(&TokenKind::LParen, "`(` after `if`")?;
        let cond = self.parse_ctexpr()?;
        self.expect(&TokenKind::RParen, "`)` after the `if` condition")?;
        self.expect(&TokenKind::LBrace, "`{` to begin the `if` body")?;
        let then_body = self.parse_stmt_block()?;
        let else_body = if self.is_ident_kw("else") {
            self.pos += 1; // 'else'
            self.expect(&TokenKind::LBrace, "`{` to begin the `else` body")?;
            self.parse_stmt_block()?
        } else {
            Vec::new()
        };
        Ok(Stmt::IfGen {
            cond,
            then_body,
            else_body,
            span,
        })
    }

    /// `assert <ct-expr>;` (Req 1.23).
    fn parse_assert(&mut self) -> PResult<Stmt> {
        let span = self.expect(&TokenKind::KwAssert, "`assert`")?.span.clone();
        let cond = self.parse_ctexpr()?;
        self.expect(&TokenKind::Semi, "`;` to end the assertion")?;
        Ok(Stmt::Assert { cond, span })
    }

    // ---- func -----------------------------------------------------------------------------

    /// `func name(time a, time b) -> time { return <expr>; }` (Req 1.21).
    fn parse_func(&mut self) -> PResult<Func> {
        // Consume the `func` soft keyword.
        let span = self.peek().expect("func present").span.clone();
        self.pos += 1;
        let (name, _) = self.expect_ident("a function name after `func`")?;
        self.expect(&TokenKind::LParen, "`(` to begin the function parameters")?;
        let mut params = Vec::new();
        if self.peek_kind() != Some(&TokenKind::RParen) {
            loop {
                // Each parameter is `time <name>`.
                self.parse_type_with_name()?; // the `time` keyword
                let (pname, _) = self.expect_ident("a parameter name")?;
                params.push(pname);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)` to end the function parameters")?;
        self.expect(&TokenKind::Arrow, "`->` before the return type")?;
        // Return type is `time`.
        self.parse_type_with_name()?;
        self.expect(&TokenKind::LBrace, "`{` to begin the function body")?;
        // Body: `return <expr>;`.
        if !self.is_ident_kw("return") {
            return Err(self.err_expected("`return` in the function body"));
        }
        self.pos += 1; // 'return'
        let body = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "`;` after the return expression")?;
        self.expect(&TokenKind::RBrace, "`}` to close the function body")?;
        Ok(Func {
            name,
            params,
            body,
            span,
        })
    }

    // ---- test / wave / expect -------------------------------------------------------------

    /// `test Name for Module { wave { ... } ... }` (Req 1.22).
    fn parse_test(&mut self) -> PResult<Test> {
        let span = self.expect(&TokenKind::KwTest, "`test`")?.span.clone();
        let (name, _) = self.expect_ident("a test name after `test`")?;
        // `for` is a reserved keyword in the lexer.
        self.expect(&TokenKind::KwFor, "`for` after the test name")?;
        let (module, _) = self.expect_ident("the module under test after `for`")?;
        self.expect(&TokenKind::LBrace, "`{` to begin the test body")?;
        let mut waves = Vec::new();
        let mut defaults: Option<Vec<(String, StimVal)>> = None;
        loop {
            match self.peek_kind() {
                Some(TokenKind::RBrace) => {
                    self.bump();
                    break;
                }
                Some(TokenKind::KwWave) => waves.push(self.parse_wave()?),
                Some(TokenKind::KwDefault) => {
                    // An optional, at-most-once `default { y = v; .. }` block holding per-test
                    // output defaults used by a wave's `expect default;`.
                    if defaults.is_some() {
                        return Err(self.err_expected(
                            "at most one `default` block per test (a second was found)",
                        ));
                    }
                    defaults = Some(self.parse_default_block()?);
                }
                None => return Err(self.err_expected("`}` to close the test body")),
                _ => return Err(self.err_expected("`wave`, `default`, or `}` in the test body")),
            }
        }
        Ok(Test {
            name,
            module,
            defaults: defaults.unwrap_or_default(),
            waves,
            span,
        })
    }

    /// `default { y = v; z = inf; }` — the optional per-test defaults block. Reuses the same
    /// `name = <stimulus>;` binding form as a wave's input assignments.
    fn parse_default_block(&mut self) -> PResult<Vec<(String, StimVal)>> {
        self.expect(&TokenKind::KwDefault, "`default`")?;
        self.expect(&TokenKind::LBrace, "`{` to begin the default block")?;
        let mut bindings = Vec::new();
        loop {
            match self.peek_kind() {
                Some(TokenKind::RBrace) => {
                    self.bump();
                    break;
                }
                Some(TokenKind::Ident(_)) => bindings.push(self.parse_stim_binding()?),
                None => return Err(self.err_expected("`}` to close the default block")),
                _ => return Err(self.err_expected("an output default `name = value;` or `}`")),
            }
        }
        Ok(bindings)
    }

    /// `wave { a = 3; b = inf; expect lo = 3; expect v = [1,2]; }` (Req 1.22).
    ///
    /// Input assignments and `expect` outputs may be interleaved; each entry ends with `;`.
    fn parse_wave(&mut self) -> PResult<Wave> {
        let span = self.expect(&TokenKind::KwWave, "`wave`")?.span.clone();
        self.expect(&TokenKind::LBrace, "`{` to begin the wave body")?;
        let mut inputs = Vec::new();
        let mut expects = Vec::new();
        let mut expect_default = false;
        loop {
            match self.peek_kind() {
                Some(TokenKind::RBrace) => {
                    self.bump();
                    break;
                }
                Some(TokenKind::KwExpect) => {
                    self.bump();
                    // `expect default;` — every output not explicitly expected is checked against
                    // the test's default (or `inf`). Otherwise `expect name = value;`.
                    if self.peek_kind() == Some(&TokenKind::KwDefault) {
                        self.bump();
                        self.expect(&TokenKind::Semi, "`;` after `expect default`")?;
                        expect_default = true;
                    } else {
                        let (name, val) = self.parse_stim_binding()?;
                        expects.push((name, val));
                    }
                }
                Some(TokenKind::Ident(_)) => {
                    let (name, val) = self.parse_stim_binding()?;
                    inputs.push((name, val));
                }
                None => return Err(self.err_expected("`}` to close the wave body")),
                _ => {
                    return Err(self.err_expected(
                        "an input assignment, `expect`, `expect default`, or `}` in the wave body",
                    ))
                }
            }
        }
        Ok(Wave {
            inputs,
            expects,
            expect_default,
            span,
        })
    }

    /// `cases Name for Module { a = [..]; b = [..]; expect y = [..]; [expect default;] }` — a
    /// table-form test. Each binding is a vector of the SAME length N; column `i` becomes wave `i`.
    /// Expanded into a plain [`Test`] of N waves here, so the runner needs no special handling.
    /// A per-case entry may itself be a `[..]` vector for a vector-typed port.
    fn parse_cases(&mut self) -> PResult<Test> {
        let span = self.expect(&TokenKind::KwCases, "`cases`")?.span.clone();
        let (name, _) = self.expect_ident("a cases name after `cases`")?;
        self.expect(&TokenKind::KwFor, "`for` after the cases name")?;
        let (module, _) = self.expect_ident("the module under test after `for`")?;
        self.expect(&TokenKind::LBrace, "`{` to begin the cases body")?;

        // Collect column vectors: (is_expect, name, elements), preserving source order.
        let mut cols: Vec<(bool, String, Vec<StimVal>)> = Vec::new();
        let mut expect_default = false;
        let mut defaults: Option<Vec<(String, StimVal)>> = None;
        loop {
            match self.peek_kind() {
                Some(TokenKind::RBrace) => {
                    self.bump();
                    break;
                }
                Some(TokenKind::KwDefault) => {
                    // An optional, at-most-once `default { .. }` block applied to every expanded
                    // wave (so "this output is always inf" is one line plus the input vectors).
                    if defaults.is_some() {
                        return Err(self.err_expected(
                            "at most one `default` block per cases (a second was found)",
                        ));
                    }
                    defaults = Some(self.parse_default_block()?);
                }
                Some(TokenKind::KwExpect) => {
                    self.bump();
                    if self.peek_kind() == Some(&TokenKind::KwDefault) {
                        self.bump();
                        self.expect(&TokenKind::Semi, "`;` after `expect default`")?;
                        expect_default = true;
                    } else {
                        let (nm, elems) = self.parse_cases_vector_binding()?;
                        cols.push((true, nm, elems));
                    }
                }
                Some(TokenKind::Ident(_)) => {
                    let (nm, elems) = self.parse_cases_vector_binding()?;
                    cols.push((false, nm, elems));
                }
                None => return Err(self.err_expected("`}` to close the cases body")),
                _ => return Err(self.err_expected(
                    "an input/expect vector binding, `default`, `expect default`, or `}` in the cases body",
                )),
            }
        }
        let defaults = defaults.unwrap_or_default();

        // Determine N from the first column; every column must match.
        let n = cols.first().map(|c| c.2.len()).unwrap_or(0);
        if n == 0 {
            return Err(self.err_at(&span, "a `cases` block needs at least one non-empty vector"));
        }
        for (_, nm, elems) in &cols {
            if elems.len() != n {
                return Err(self.err_at(
                    &span,
                    &format!(
                        "all `cases` vectors must have the same length; `{nm}` has {} but expected {n}",
                        elems.len()
                    ),
                ));
            }
        }

        // Expand column-major into N waves.
        let mut waves = Vec::with_capacity(n);
        for i in 0..n {
            let mut inputs = Vec::new();
            let mut expects = Vec::new();
            for (is_expect, nm, elems) in &cols {
                let v = elems[i].clone();
                if *is_expect {
                    expects.push((nm.clone(), v));
                } else {
                    inputs.push((nm.clone(), v));
                }
            }
            waves.push(Wave {
                inputs,
                expects,
                expect_default,
                span: span.clone(),
            });
        }

        Ok(Test {
            name,
            module,
            defaults,
            waves,
            span,
        })
    }

    /// `name = [v, v, ...];` — one column of a `cases` block: a name bound to a vector of per-case
    /// stimulus values. The outer `[...]` is the case list; each element may itself be a scalar,
    /// `inf`, or (for a vector port) a nested `[...]`.
    fn parse_cases_vector_binding(&mut self) -> PResult<(String, Vec<StimVal>)> {
        let (name, _) = self.expect_ident("a signal name in the cases body")?;
        self.expect(&TokenKind::Eq, "`=` in the cases binding")?;
        self.expect(&TokenKind::LBrack, "`[` to begin the cases vector")?;
        let mut elems = Vec::new();
        if self.peek_kind() != Some(&TokenKind::RBrack) {
            loop {
                elems.push(self.parse_stim_val()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RBrack, "`]` to close the cases vector")?;
        self.expect(&TokenKind::Semi, "`;` to end the cases binding")?;
        Ok((name, elems))
    }

    /// `name = <stimulus>;` — one input or expect binding inside a wave.
    fn parse_stim_binding(&mut self) -> PResult<(String, StimVal)> {
        let (name, _) = self.expect_ident("a signal name in the wave body")?;
        self.expect(&TokenKind::Eq, "`=` in the wave binding")?;
        let val = self.parse_stim_val()?;
        self.expect(&TokenKind::Semi, "`;` to end the wave binding")?;
        Ok((name, val))
    }

    /// A stimulus / expect value: an integer, `inf`, or a vector literal `[v, v, ...]` (Req 1.22).
    fn parse_stim_val(&mut self) -> PResult<StimVal> {
        match self.peek_kind() {
            Some(TokenKind::Int(v)) => {
                let v = *v;
                let span = self.bump().unwrap().span.clone();
                Ok(StimVal::time(v, span))
            }
            Some(TokenKind::Inf) => {
                let span = self.bump().unwrap().span.clone();
                Ok(StimVal::inf(span))
            }
            Some(TokenKind::LBrack) => {
                let span = self.bump().unwrap().span.clone(); // '['
                let mut vals = Vec::new();
                if self.peek_kind() != Some(&TokenKind::RBrack) {
                    loop {
                        vals.push(self.parse_stim_val()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::RBrack, "`]` to close the vector literal")?;
                Ok(StimVal::vector(vals, span))
            }
            _ => Err(self.err_expected("a stimulus value (an integer, `inf`, or a `[...]` vector)")),
        }
    }

    // ---- expressions (precedence climbing) ------------------------------------------------

    /// Parse a full spike-time expression (Req 1.10, 1.15, 1.16).
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_bin_expr(0)
    }

    /// Precedence-climbing driven by the operator table. `min_prec` is the lowest precedence this
    /// call will accept; higher-precedence operators bind tighter (Req 1.15). The relational
    /// operators are non-associative: a second operator at the same level without parentheses is a
    /// parse error (Req 1.16).
    fn parse_bin_expr(&mut self, min_prec: u8) -> PResult<Expr> {
        let mut lhs = self.parse_delay_expr()?;

        loop {
            // The only table-driven infix operators are `Op(sym)` tokens.
            let (sym, entry_prec, entry_assoc, func_name) = match self.peek_kind() {
                Some(TokenKind::Op(sym)) => match self.ops.lookup_symbol(sym) {
                    Some(e) => (sym.clone(), e.precedence, e.assoc, e.func_name.clone()),
                    None => {
                        // An operator symbol the table does not know is a parse error.
                        return Err(self.err_expected("a known operator from the operator table"));
                    }
                },
                _ => break,
            };

            if entry_prec < min_prec {
                break;
            }

            let op_span = self.peek().unwrap().span.clone();
            self.bump(); // consume the operator

            // Parse the right operand. For a non-associative operator, the RHS must not itself
            // contain another operator at the same precedence level, so we require strictly
            // higher precedence (entry_prec + 1) and then explicitly reject a trailing operator
            // at the same level (Req 1.16).
            let next_min = match entry_assoc {
                Assoc::Left => entry_prec + 1,
                Assoc::Right => entry_prec,
                Assoc::NonAssoc => entry_prec + 1,
            };
            let rhs = self.parse_bin_expr(next_min)?;

            if entry_assoc == Assoc::NonAssoc {
                // If another same-level relational operator immediately follows, the user chained
                // without parentheses (`a < b < c`): a parse error (Req 1.16).
                if let Some(TokenKind::Op(next_sym)) = self.peek_kind() {
                    if let Some(next_entry) = self.ops.lookup_symbol(next_sym) {
                        if next_entry.precedence == entry_prec {
                            return Err(ParseError(
                                Diagnostic::error(
                                    self.cur_span(),
                                    format!(
                                        "the relational operator `{next_sym}` cannot be chained \
                                         after `{sym}` without parentheses"
                                    ),
                                )
                                .with_note(
                                    "relational operators are non-associative; parenthesize one \
                                     side, e.g. `(a {sym} b) {next} c`"
                                        .replace("{sym}", &sym)
                                        .replace("{next}", next_sym),
                                ),
                            ));
                        }
                    }
                }
            }

            let args = vec![lhs, rhs];
            lhs = Expr::Op {
                func: func_name,
                args,
                span: op_span,
            };
        }

        Ok(lhs)
    }

    /// The delay level `a + n`: a postfix compile-time delay binding tighter than relationals and
    /// looser than primaries/calls (Req 1.10, 1.15). The RHS is a compile-time expression, never a
    /// wire.
    fn parse_delay_expr(&mut self) -> PResult<Expr> {
        let mut base = self.parse_postfix_expr()?;
        while self.peek_kind() == Some(&TokenKind::Plus) {
            let span = base.span().clone();
            self.bump(); // '+'
            // The delay amount is an ARITHMETIC compile-time expression only (parse_ct_add), not the
            // full comparison-capable ct-expression. Otherwise `a + 1 < b` would greedily parse the
            // amount as the comparison `(1 < b)` instead of letting the spike-time `<` apply to
            // `(a + 1)`. A delay by a boolean is meaningless anyway. Comparisons remain available in
            // bracketed/keyword ct-contexts (`if`, `for`, `assert`, `[N]`), which have terminators.
            let amount = self.parse_ct_add()?;
            base = Expr::Delay(Box::new(base), amount, span);
        }
        Ok(base)
    }

    /// A primary optionally followed by vector-index suffixes `x[i]` (Req 1.10).
    fn parse_postfix_expr(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary_expr()?;
        while self.peek_kind() == Some(&TokenKind::LBrack) {
            let span = expr.span().clone();
            self.bump(); // '['
            let idx = self.parse_ctexpr()?;
            self.expect(&TokenKind::RBrack, "`]` to close the index")?;
            expr = Expr::Index(Box::new(expr), idx, span);
        }
        Ok(expr)
    }

    /// A primary expression: `inf`, `min(...)`, a call `name(...)`, an identifier, or a
    /// parenthesized expression (Req 1.10, 1.11, 1.13).
    fn parse_primary_expr(&mut self) -> PResult<Expr> {
        match self.peek_kind() {
            Some(TokenKind::Inf) => {
                let span = self.bump().unwrap().span.clone();
                Ok(Expr::Inf(span))
            }
            Some(TokenKind::LParen) => {
                self.bump(); // '('
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)` to close the parenthesized expression")?;
                Ok(inner)
            }
            Some(TokenKind::Int(_)) => {
                // A bare numeric literal is never a legal wire expression (invariance, Req 2.6).
                // The parser accepts the grammar but a number here cannot form an expression, so
                // report a clear parse error pointing at the literal.
                Err(self.err_expected(
                    "an expression; a numeric literal cannot be used directly as a wire value",
                ))
            }
            Some(TokenKind::Ident(name)) => {
                let name = name.clone();
                let span = self.peek().unwrap().span.clone();
                self.pos += 1;
                if self.peek_kind() == Some(&TokenKind::LParen) {
                    self.parse_call(name, span)
                } else {
                    Ok(Expr::Ident(name, span))
                }
            }
            _ => Err(self.err_expected("an expression")),
        }
    }

    /// A function-style call `name(arg, ...)`. `min` is special: the variadic minimum, which must
    /// have at least two arguments (Req 1.11). `inc` maps to the increment primitive. `lt`/`gt`/
    /// `eq`/`neq` map to relational ops; any other name is a generic call (Req 1.13).
    fn parse_call(&mut self, name: String, span: Span) -> PResult<Expr> {
        self.expect(&TokenKind::LParen, "`(` to begin the call arguments")?;
        let mut args = Vec::new();
        if self.peek_kind() != Some(&TokenKind::RParen) {
            loop {
                args.push(self.parse_expr()?);
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.expect(&TokenKind::RParen, "`)` to close the call arguments")?;

        match name.as_str() {
            "min" => {
                if args.len() < 2 {
                    return Err(ParseError(
                        Diagnostic::error(
                            span,
                            format!(
                                "`min(...)` needs at least two arguments but got {}",
                                args.len()
                            ),
                        )
                        .with_note(
                            "`min` is the variadic first-arrival operator; call it as \
                             `min(a, b, ...)`",
                        ),
                    ));
                }
                Ok(Expr::Min(args, span))
            }
            "inc" => {
                if args.len() != 1 {
                    return Err(ParseError(Diagnostic::error(
                        span,
                        format!("`inc(...)` takes exactly one argument but got {}", args.len()),
                    )));
                }
                let arg = args.into_iter().next().unwrap();
                Ok(Expr::Inc(Box::new(arg), span))
            }
            "lt" | "gt" | "eq" | "neq" => {
                if args.len() != 2 {
                    return Err(ParseError(Diagnostic::error(
                        span,
                        format!(
                            "`{name}(...)` is a binary relational and takes two arguments but got {}",
                            args.len()
                        ),
                    )));
                }
                Ok(Expr::Op {
                    func: name,
                    args,
                    span,
                })
            }
            _ => Ok(Expr::Call { name, args, span }),
        }
    }

    // ---- compile-time expressions ---------------------------------------------------------

    /// Parse a compile-time integer expression. Precedence, loosest to tightest: comparisons
    /// (`== != < <= > >=`), then `+ -`, then `* / %`, over integer literals, params, and loop
    /// variables (Req 1.17, 1.18). A comparison yields `1` (true) or `0` (false), so it can be used
    /// directly as an `if`/`assert` condition or anywhere else an integer is expected.
    fn parse_ctexpr(&mut self) -> PResult<CtExpr> {
        self.parse_ct_cmp()
    }

    /// The comparison level: an additive expression optionally followed by ONE comparison operator.
    /// Comparisons are non-associative (no `a < b < c` without parentheses), matching the
    /// spike-time relationals.
    fn parse_ct_cmp(&mut self) -> PResult<CtExpr> {
        let lhs = self.parse_ct_add()?;
        let op = match self.peek_kind() {
            Some(TokenKind::Op(s)) => match s.as_str() {
                "==" => Some(CtOp::Eq),
                "!=" => Some(CtOp::Ne),
                "<" => Some(CtOp::Lt),
                "<=" => Some(CtOp::Le),
                ">" => Some(CtOp::Gt),
                ">=" => Some(CtOp::Ge),
                _ => None,
            },
            _ => None,
        };
        let op = match op {
            Some(o) => o,
            None => return Ok(lhs),
        };
        let span = lhs.span.clone();
        self.bump(); // the comparison operator
        let rhs = self.parse_ct_add()?;
        // Non-associative: a second comparison without parentheses is an error.
        if let Some(TokenKind::Op(s)) = self.peek_kind() {
            if matches!(s.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") {
                return Err(self.err_expected(
                    "a single comparison (compile-time comparisons do not chain; parenthesize)",
                ));
            }
        }
        Ok(CtExpr::binop(op, lhs, rhs, span))
    }

    fn parse_ct_add(&mut self) -> PResult<CtExpr> {
        let mut lhs = self.parse_ct_mul()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Plus) => CtOp::Add,
                Some(TokenKind::Minus) => CtOp::Sub,
                _ => break,
            };
            let span = lhs.span.clone();
            self.bump();
            let rhs = self.parse_ct_mul()?;
            lhs = CtExpr::binop(op, lhs, rhs, span);
        }
        Ok(lhs)
    }

    fn parse_ct_mul(&mut self) -> PResult<CtExpr> {
        let mut lhs = self.parse_ct_primary()?;
        loop {
            let op = match self.peek_kind() {
                Some(TokenKind::Star) => CtOp::Mul,
                Some(TokenKind::Slash) => CtOp::Div,
                Some(TokenKind::Percent) => CtOp::Mod,
                _ => break,
            };
            let span = lhs.span.clone();
            self.bump();
            let rhs = self.parse_ct_primary()?;
            lhs = CtExpr::binop(op, lhs, rhs, span);
        }
        Ok(lhs)
    }

    fn parse_ct_primary(&mut self) -> PResult<CtExpr> {
        match self.peek_kind() {
            Some(TokenKind::Int(v)) => {
                let v = *v;
                let span = self.bump().unwrap().span.clone();
                Ok(CtExpr::int(v, span))
            }
            Some(TokenKind::Ident(name)) => {
                let name = name.clone();
                let span = self.bump().unwrap().span.clone();
                Ok(CtExpr::param(name, span))
            }
            Some(TokenKind::LParen) => {
                self.bump(); // '('
                let inner = self.parse_ctexpr()?;
                self.expect(
                    &TokenKind::RParen,
                    "`)` to close the parenthesized compile-time expression",
                )?;
                Ok(inner)
            }
            _ => Err(self.err_expected(
                "a compile-time integer (a literal, a parameter, or arithmetic on them)",
            )),
        }
    }
}

/// A parsed top-level item, used internally to route into the right `Design` collection.
enum Item {
    Import(Import),
    Include(Import),
    Module(Module),
    Func(Func),
    Test(Test),
}

/// A plain-language description of a token kind for "found ..." error messages.
fn describe_kind(k: &TokenKind) -> String {
    match k {
        TokenKind::Ident(s) => format!("the identifier `{s}`"),
        TokenKind::Int(v) => format!("the integer literal `{v}`"),
        TokenKind::Inf => "`inf`".to_string(),
        TokenKind::KwModule => "`module`".to_string(),
        TokenKind::KwInput => "`input`".to_string(),
        TokenKind::KwOutput => "`output`".to_string(),
        TokenKind::KwWire => "`wire`".to_string(),
        TokenKind::KwInst => "`inst`".to_string(),
        TokenKind::KwTest => "`test`".to_string(),
        TokenKind::KwWave => "`wave`".to_string(),
        TokenKind::KwExpect => "`expect`".to_string(),
        TokenKind::KwFor => "`for`".to_string(),
        TokenKind::KwIn => "`in`".to_string(),
        TokenKind::KwParam => "`param`".to_string(),
        TokenKind::KwAssert => "`assert`".to_string(),
        TokenKind::KwImport => "`import`".to_string(),
        TokenKind::KwInclude => "`include`".to_string(),
        TokenKind::KwCases => "`cases`".to_string(),
        TokenKind::KwDefault => "`default`".to_string(),
        TokenKind::KwStage => "`stage`".to_string(),
        TokenKind::KwReset => "`reset`".to_string(),
        TokenKind::Op(s) => format!("the operator `{s}`"),
        TokenKind::Plus => "`+`".to_string(),
        TokenKind::Minus => "`-`".to_string(),
        TokenKind::Star => "`*`".to_string(),
        TokenKind::Slash => "`/`".to_string(),
        TokenKind::Percent => "`%`".to_string(),
        TokenKind::LParen => "`(`".to_string(),
        TokenKind::RParen => "`)`".to_string(),
        TokenKind::LBrace => "`{`".to_string(),
        TokenKind::RBrace => "`}`".to_string(),
        TokenKind::LBrack => "`[`".to_string(),
        TokenKind::RBrack => "`]`".to_string(),
        TokenKind::Hash => "`#`".to_string(),
        TokenKind::Dot => "`.`".to_string(),
        TokenKind::Comma => "`,`".to_string(),
        TokenKind::Semi => "`;`".to_string(),
        TokenKind::Eq => "`=`".to_string(),
        TokenKind::Arrow => "`->`".to_string(),
        TokenKind::DotDot => "`..`".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a source string with the builtin table at 32-bit width, expecting success.
    fn parse_ok(src: &str) -> Design {
        parse_source(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32)
            .unwrap_or_else(|d| {
                let rendered: Vec<String> = d.iter().map(|x| x.render()).collect();
                panic!("expected a clean parse, got errors:\n{}", rendered.join("\n"))
            })
    }

    /// Parse a source string expecting at least one parse error, returning the diagnostics.
    fn parse_err(src: &str) -> Vec<Diagnostic> {
        match parse_source(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32) {
            Ok(d) => panic!("expected a parse error, but parsing succeeded: {d:?}"),
            Err(e) => e,
        }
    }

    // ---- modules and ports (Req 1.8) ------------------------------------------------------

    #[test]
    fn parses_a_scalar_port_module() {
        let d = parse_ok(
            "module Sort2 (input time a, input time b, output time lo, output time hi) {\n\
             lo = min(a, b);\n\
             hi = min(b, a);\n\
             }",
        );
        assert_eq!(d.modules.len(), 1);
        let m = &d.modules[0];
        assert_eq!(m.name, "Sort2");
        assert_eq!(m.ports.len(), 4);
        assert_eq!(m.ports[0].dir, Dir::Input);
        assert_eq!(m.ports[0].name, "a");
        assert!(matches!(m.ports[0].ty.kind, TyKind::Time));
        assert_eq!(m.ports[2].dir, Dir::Output);
        assert_eq!(m.ports[2].name, "lo");
        assert_eq!(m.body.len(), 2);
    }

    #[test]
    fn parses_vector_ports() {
        let d = parse_ok(
            "module Wta3 (input time x[3], output time y[3]) {\n\
             y[0] = x[0] < min(x[1], x[2]);\n\
             y[1] = x[1] < min(x[0], x[2]);\n\
             y[2] = x[2] < min(x[0], x[1]);\n\
             }",
        );
        let m = &d.modules[0];
        match &m.ports[0].ty.kind {
            TyKind::TimeVec(n) => assert!(matches!(n.kind, CtExprKind::Int(3))),
            other => panic!("expected time[3], got {other:?}"),
        }
        assert_eq!(m.body.len(), 3);
    }

    // ---- params (Req 1.17) ----------------------------------------------------------------

    #[test]
    fn parses_param_list_with_default() {
        let d = parse_ok("module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }");
        let m = &d.modules[0];
        assert_eq!(m.params.len(), 1);
        assert_eq!(m.params[0].name, "N");
        match &m.params[0].default {
            Some(ce) => assert!(matches!(ce.kind, CtExprKind::Int(3))),
            None => panic!("expected a default value"),
        }
        // Vector size referencing a param.
        let d2 = parse_ok("module MinN #(param N = 4) (input time x[N], output time y) { y = x[0]; }");
        match &d2.modules[0].ports[0].ty.kind {
            TyKind::TimeVec(n) => assert!(matches!(&n.kind, CtExprKind::Param(p) if p == "N")),
            other => panic!("expected time[N], got {other:?}"),
        }
    }

    #[test]
    fn parses_param_without_default() {
        let d = parse_ok("module M #(param K) (input time a, output time y) { y = a; }");
        assert_eq!(d.modules[0].params[0].name, "K");
        assert!(d.modules[0].params[0].default.is_none());
    }

    // ---- wires and assignment (Req 1.9) ---------------------------------------------------

    #[test]
    fn parses_wire_decl_with_and_without_driver() {
        let d = parse_ok(
            "module M (input time a, input time b, output time y) {\n\
             wire time w = min(a, b);\n\
             wire time v;\n\
             v = w;\n\
             y = v;\n\
             }",
        );
        let body = &d.modules[0].body;
        assert!(matches!(&body[0], Stmt::WireDecl { driver: Some(_), name, .. } if name == "w"));
        assert!(matches!(&body[1], Stmt::WireDecl { driver: None, name, .. } if name == "v"));
        assert!(matches!(&body[2], Stmt::Assign { .. }));
    }

    #[test]
    fn parses_vector_wire_and_indexed_assignment() {
        let d = parse_ok(
            "module M (input time x[2], output time y) {\n\
             wire time acc[2];\n\
             acc[0] = x[0];\n\
             acc[1] = min(acc[0], x[1]);\n\
             y = acc[1];\n\
             }",
        );
        let body = &d.modules[0].body;
        assert!(matches!(&body[0], Stmt::WireDecl { ty: Ty { kind: TyKind::TimeVec(_), .. }, .. }));
        match &body[1] {
            Stmt::Assign { target, .. } => {
                assert!(matches!(&target.kind, LValueKind::Index(n, _) if n == "acc"));
            }
            other => panic!("expected indexed assign, got {other:?}"),
        }
    }

    // ---- expressions (Req 1.10, 1.11, 1.13, 1.14, 1.15) -----------------------------------

    #[test]
    fn parses_min_variadic() {
        let d = parse_ok("module M (input time a, input time b, input time c, output time y) { y = min(a, b, c); }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Min(args, _), .. } => assert_eq!(args.len(), 3),
            other => panic!("expected min, got {other:?}"),
        }
    }

    #[test]
    fn min_with_one_arg_is_a_parse_error() {
        let errs = parse_err("module M (input time a, output time y) { y = min(a); }");
        assert!(errs.iter().any(|e| e.message.contains("at least two arguments")));
    }

    #[test]
    fn parses_delay_expression() {
        let d = parse_ok("module M (input time a, output time y) { y = a + 2; }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Delay(_, amt, _), .. } => {
                assert!(matches!(amt.kind, CtExprKind::Int(2)));
            }
            other => panic!("expected delay, got {other:?}"),
        }
    }

    #[test]
    fn parses_delay_with_param_amount() {
        let d = parse_ok("module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Delay(_, amt, _), .. } => {
                assert!(matches!(&amt.kind, CtExprKind::Param(p) if p == "N"));
            }
            other => panic!("expected delay, got {other:?}"),
        }
    }

    #[test]
    fn parses_inc_inf_and_index() {
        let d = parse_ok("module M (input time x[2], output time y) { y = inc(x[0]); }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Inc(inner, _), .. } => {
                assert!(matches!(**inner, Expr::Index(_, _, _)));
            }
            other => panic!("expected inc, got {other:?}"),
        }
        let d2 = parse_ok("module M (input time a, output time y) { y = min(a, inf); }");
        match &d2.modules[0].body[0] {
            Stmt::Assign { value: Expr::Min(args, _), .. } => {
                assert!(matches!(args[1], Expr::Inf(_)));
            }
            other => panic!("expected min with inf, got {other:?}"),
        }
    }

    #[test]
    fn parses_relational_infix_and_call_forms() {
        // Infix `<` maps to lt.
        let d = parse_ok("module M (input time a, input time b, output time y) { y = a < b; }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Op { func, args, .. }, .. } => {
                assert_eq!(func, "lt");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected lt op, got {other:?}"),
        }
        // Call `lt(a, b)` maps to the same op.
        let d2 = parse_ok("module M (input time a, input time b, output time y) { y = lt(a, b); }");
        assert!(matches!(&d2.modules[0].body[0], Stmt::Assign { value: Expr::Op { func, .. }, .. } if func == "lt"));
        // The other three symbols.
        for (src_op, func) in [(">", "gt"), ("==", "eq"), ("!=", "neq")] {
            let src = format!("module M (input time a, input time b, output time y) {{ y = a {src_op} b; }}");
            let dd = parse_ok(&src);
            assert!(matches!(&dd.modules[0].body[0], Stmt::Assign { value: Expr::Op { func: f, .. }, .. } if f == func));
        }
    }

    #[test]
    fn parses_generic_call() {
        let d = parse_ok("module M (input time a, input time b, output time y) { y = max(a, b); }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Call { name, args, .. }, .. } => {
                assert_eq!(name, "max");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected call, got {other:?}"),
        }
    }

    #[test]
    fn delay_binds_tighter_than_relational() {
        // `a + 1 < b` parses as `(a + 1) < b`, i.e. the lt's left arg is a Delay.
        let d = parse_ok("module M (input time a, input time b, output time y) { y = a + 1 < b; }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Op { func, args, .. }, .. } => {
                assert_eq!(func, "lt");
                assert!(matches!(args[0], Expr::Delay(_, _, _)), "lhs should be the delay");
                assert!(matches!(args[1], Expr::Ident(_, _)));
            }
            other => panic!("expected top-level lt, got {other:?}"),
        }
    }

    #[test]
    fn parentheses_allow_relational_nesting() {
        // Parenthesized chaining is legal: `(a < b) < c`.
        let d = parse_ok("module M (input time a, input time b, input time c, output time y) { y = (a < b) < c; }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Op { func, args, .. }, .. } => {
                assert_eq!(func, "lt");
                assert!(matches!(args[0], Expr::Op { .. }), "lhs should be the inner lt");
            }
            other => panic!("expected outer lt, got {other:?}"),
        }
    }

    // ---- inst (Req 1.20) ------------------------------------------------------------------

    #[test]
    fn parses_instantiation() {
        let d = parse_ok(
            "module Top (input time p, input time q, output time r) {\n\
             wire time lo; wire time hi;\n\
             inst s = Sort2 (.a(p), .b(q), .lo(lo), .hi(hi));\n\
             r = hi + 2;\n\
             }",
        );
        let inst = d.modules[0].body.iter().find_map(|s| match s {
            Stmt::Inst { name, module, conns, params, .. } => Some((name, module, conns, params)),
            _ => None,
        }).expect("an inst statement");
        assert_eq!(inst.0, "s");
        assert_eq!(inst.1, "Sort2");
        assert_eq!(inst.2.len(), 4);
        assert!(inst.3.is_empty());
        assert_eq!(inst.2[0].0, "a");
    }

    #[test]
    fn parses_parameterized_instantiation() {
        let d = parse_ok(
            "module Top (input time x[8], output time y) {\n\
             inst t0 = MinTree #(.N(8)) (.x(x), .y(y));\n\
             }",
        );
        match &d.modules[0].body[0] {
            Stmt::Inst { params, conns, .. } => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].0, "N");
                assert!(matches!(params[0].1.kind, CtExprKind::Int(8)));
                assert_eq!(conns.len(), 2);
            }
            other => panic!("expected parameterized inst, got {other:?}"),
        }
    }

    // ---- for / if / assert (Req 1.18, 1.19, 1.23) -----------------------------------------

    #[test]
    fn parses_for_generate_loop() {
        let d = parse_ok(
            "module MinN #(param N = 4) (input time x[N], output time y) {\n\
             wire time acc[N];\n\
             acc[0] = x[0];\n\
             for i in 1..N {\n\
             acc[i] = min(acc[i - 1], x[i]);\n\
             }\n\
             y = acc[N - 1];\n\
             }",
        );
        let forgen = d.modules[0].body.iter().find_map(|s| match s {
            Stmt::ForGen { var, lo, hi, body, .. } => Some((var, lo, hi, body)),
            _ => None,
        }).expect("a for-gen");
        assert_eq!(forgen.0, "i");
        assert!(matches!(forgen.1.kind, CtExprKind::Int(1)));
        assert!(matches!(&forgen.2.kind, CtExprKind::Param(p) if p == "N"));
        assert_eq!(forgen.3.len(), 1);
    }

    #[test]
    fn parses_if_generate_with_else() {
        let d = parse_ok(
            "module M #(param N = 1) (input time a, output time y) {\n\
             if (N) { y = a; } else { y = inc(a); }\n\
             }",
        );
        match &d.modules[0].body[0] {
            Stmt::IfGen { then_body, else_body, .. } => {
                assert_eq!(then_body.len(), 1);
                assert_eq!(else_body.len(), 1);
            }
            other => panic!("expected if-gen, got {other:?}"),
        }
    }

    #[test]
    fn parses_if_generate_without_else() {
        let d = parse_ok(
            "module M #(param N = 1) (input time a, output time y) {\n\
             y = a;\n\
             if (N) { wire time t = a; }\n\
             }",
        );
        match d.modules[0].body.iter().find(|s| matches!(s, Stmt::IfGen { .. })) {
            Some(Stmt::IfGen { else_body, .. }) => assert!(else_body.is_empty()),
            _ => panic!("expected an if-gen"),
        }
    }

    #[test]
    fn parses_assert() {
        let d = parse_ok("module M #(param N = 4) (input time a, output time y) { assert N; y = a; }");
        assert!(d.modules[0].body.iter().any(|s| matches!(s, Stmt::Assert { .. })));
    }

    #[test]
    fn parses_compile_time_arithmetic_in_ctexpr() {
        let d = parse_ok("module M #(param N = 4) (input time a, output time y) { y = a + (N * 2 + 1); }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Delay(_, amt, _), .. } => {
                // Top-level is an Add of (N*2) and 1.
                assert!(matches!(amt.kind, CtExprKind::BinOp(CtOp::Add, _, _)));
            }
            other => panic!("expected delay with arithmetic, got {other:?}"),
        }
    }

    // ---- func (Req 1.21) ------------------------------------------------------------------

    #[test]
    fn parses_func() {
        let d = parse_ok("func le(time a, time b) -> time { return a < (b < a); }");
        assert_eq!(d.funcs.len(), 1);
        let f = &d.funcs[0];
        assert_eq!(f.name, "le");
        assert_eq!(f.params, vec!["a".to_string(), "b".to_string()]);
        assert!(matches!(f.body, Expr::Op { .. }));
    }

    // ---- import (Req 1.24) ----------------------------------------------------------------

    #[test]
    fn parses_import() {
        let d = parse_ok("import std.sta; module M (input time a, output time y) { y = a; }");
        assert_eq!(d.imports.len(), 1);
        assert_eq!(d.imports[0].path, "std.sta");
    }

    // ---- test / wave / expect (Req 1.22) --------------------------------------------------

    #[test]
    fn parses_test_block_with_waves() {
        let d = parse_ok(
            "test TbSort2 for Sort2 {\n\
             wave { a = 3; b = 5; expect lo = 3; expect hi = 5; }\n\
             wave { a = inf; b = 4; expect lo = 4; expect hi = inf; }\n\
             }",
        );
        assert_eq!(d.tests.len(), 1);
        let t = &d.tests[0];
        assert_eq!(t.name, "TbSort2");
        assert_eq!(t.module, "Sort2");
        assert_eq!(t.waves.len(), 2);
        let w0 = &t.waves[0];
        assert_eq!(w0.inputs.len(), 2);
        assert_eq!(w0.expects.len(), 2);
        assert!(matches!(w0.inputs[0].1.kind, StimValKind::Time(3)));
        // Second wave has an inf input and an inf expect.
        let w1 = &t.waves[1];
        assert!(matches!(w1.inputs[0].1.kind, StimValKind::Inf));
        assert!(matches!(w1.expects[1].1.kind, StimValKind::Inf));
    }

    #[test]
    fn parses_vector_stimulus_literal() {
        let d = parse_ok(
            "test T for M {\n\
             wave { a = [3, inf, 5]; expect y = [1, 2, 3]; }\n\
             }",
        );
        let w = &d.tests[0].waves[0];
        match &w.inputs[0].1.kind {
            StimValKind::Vec(vs) => {
                assert_eq!(vs.len(), 3);
                assert!(matches!(vs[1].kind, StimValKind::Inf));
            }
            other => panic!("expected vector stimulus, got {other:?}"),
        }
    }

    // ---- new infix operators: <= >= ^ -----------------------------------------------------

    #[test]
    fn new_infix_operators_map_to_their_funcs() {
        for (src, func) in [
            ("a < b", "lt"),
            ("a > b", "gt"),
            ("a <= b", "le"),
            ("a >= b", "ge"),
            ("a == b", "eq"),
            ("a != b", "neq"),
            ("a ^ b", "min"),
        ] {
            let m = format!("module M (input time a, b, output time y) {{ y = {src}; }}");
            let d = parse_ok(&m);
            match &d.modules[0].body[0] {
                Stmt::Assign { value: Expr::Op { func: f, args, .. }, .. } => {
                    assert_eq!(f, func, "`{src}` should map to `{func}`");
                    assert_eq!(args.len(), 2);
                }
                other => panic!("`{src}` should parse as a binary Op, got {other:?}"),
            }
        }
    }

    #[test]
    fn le_and_lt_munch_maximally() {
        // `a <= b` must be one `le`, not `a < (= b)`.
        let d = parse_ok("module M (input time a, b, output time y) { y = a <= b; }");
        match &d.modules[0].body[0] {
            Stmt::Assign { value: Expr::Op { func, .. }, .. } => assert_eq!(func, "le"),
            other => panic!("expected le, got {other:?}"),
        }
    }

    #[test]
    fn old_pipe_operators_no_longer_parse() {
        assert!(parse_err("module M (input time a, b, output time y) { y = a <| b; }").len() > 0);
    }
    // ---- compile-time comparisons in conditions -------------------------------------------

    #[test]
    fn if_condition_accepts_a_comparison() {
        // `if (N == 0)` parses; the condition is a CtExpr BinOp with CtOp::Eq.
        let d = parse_ok(
            "module M #(param N = 0) (input time a, output time y) { if (N == 0) { y = a; } else { y = a + 1; } }",
        );
        let has_ifgen = d.modules[0].body.iter().any(|s| matches!(s, Stmt::IfGen { .. }));
        assert!(has_ifgen, "the if-generate should parse");
    }

    #[test]
    fn ct_comparison_does_not_chain() {
        // `(N == 0) == 1` is fine (parenthesized); `N == 0 == 1` is a chain error.
        assert!(parse_err(
            "module M #(param N = 0) (input time a, output time y) { if (N == 0 == 1) { y = a; } }"
        ).len() > 0);
    }
    // ---- grouped wire declarations --------------------------------------------------------

    #[test]
    fn grouped_wire_declares_one_wire_per_name() {
        // `wire time a, b, c;` -> three undriven scalar wires, then an assignment uses them.
        let d = parse_ok(
            "module M (input time x, output time y) { wire time a, b, c; a = x; b = x; c = x; y = min(a, min(b, c)); }",
        );
        let body = &d.modules[0].body;
        // First three statements are the three wire decls (in order a, b, c).
        let names: Vec<&str> = body.iter().filter_map(|s| match s {
            Stmt::WireDecl { name, driver: None, ty: Ty { kind: TyKind::Time, .. }, .. } => Some(name.as_str()),
            _ => None,
        }).collect();
        assert_eq!(names, vec!["a", "b", "c"], "three scalar wires declared in order");
    }

    #[test]
    fn grouped_wire_per_name_vector_suffix_is_local() {
        // `wire time v[4], s;` -> v is time[4], s is scalar.
        let d = parse_ok(
            "module M (input time x, output time y) { wire time v[4], s; s = x; y = s; }",
        );
        let decls: Vec<&Stmt> = d.modules[0].body.iter().filter(|s| matches!(s, Stmt::WireDecl { .. })).collect();
        assert!(matches!(decls[0], Stmt::WireDecl { ty: Ty { kind: TyKind::TimeVec(_), .. }, name, .. } if name == "v"));
        assert!(matches!(decls[1], Stmt::WireDecl { ty: Ty { kind: TyKind::Time, .. }, name, .. } if name == "s"));
    }

    #[test]
    fn single_wire_with_inline_driver_still_works() {
        let d = parse_ok("module M (input time a, input time b, output time y) { wire time w = min(a, b); y = w; }");
        assert!(d.modules[0].body.iter().any(|s|
            matches!(s, Stmt::WireDecl { name, driver: Some(_), .. } if name == "w")));
    }

    #[test]
    fn inline_driver_in_a_grouped_wire_is_an_error() {
        // `wire time a = x, b;` is ambiguous and rejected.
        assert!(parse_err(
            "module M (input time x, output time y) { wire time a = x, b; y = a; }"
        ).len() > 0);
    }

    // ---- grouped port declarations --------------------------------------------------------

    #[test]
    fn grouped_ports_share_direction_and_type() {
        // `input time a, b, output time y, z` == four ports with carried direction/type.
        let d = parse_ok("module M (input time a, b, output time y, z) { y = a; z = b; }");
        let p = &d.modules[0].ports;
        assert_eq!(p.len(), 4);
        assert_eq!((p[0].name.as_str(), p[0].dir), ("a", Dir::Input));
        assert_eq!((p[1].name.as_str(), p[1].dir), ("b", Dir::Input));
        assert_eq!((p[2].name.as_str(), p[2].dir), ("y", Dir::Output));
        assert_eq!((p[3].name.as_str(), p[3].dir), ("z", Dir::Output));
        assert!(p.iter().all(|x| matches!(x.ty.kind, TyKind::Time)));
    }

    #[test]
    fn per_name_vector_suffix_does_not_leak_to_the_group() {
        // `input time x[4], y` -> x is time[4], y is scalar time (suffix is per-name).
        let d = parse_ok("module M (input time x[4], y, output time z) { z = y; }");
        let p = &d.modules[0].ports;
        assert!(matches!(p[0].ty.kind, TyKind::TimeVec(_)), "x should be a vector");
        assert!(matches!(p[1].ty.kind, TyKind::Time), "y should be scalar");
        assert!(matches!(p[2].ty.kind, TyKind::Time));
    }

    #[test]
    fn a_bare_name_before_any_direction_is_an_error() {
        let errs = parse_err("module M (a, output time y) { y = a; }");
        assert!(!errs.is_empty());
    }

    // ---- new test syntax: default block, expect default, cases ----------------------------

    #[test]
    fn parses_test_default_block() {
        let d = parse_ok(
            "test T for M {\n\
             default { y = 0; z = inf; }\n\
             wave { a = 1; expect default; }\n\
             }",
        );
        let t = &d.tests[0];
        assert_eq!(t.defaults.len(), 2);
        assert_eq!(t.defaults[0].0, "y");
        assert!(matches!(t.defaults[0].1.kind, StimValKind::Time(0)));
        assert!(t.waves[0].expect_default, "the wave should set expect_default");
    }

    #[test]
    fn a_second_default_block_is_an_error() {
        let errs = parse_err(
            "test T for M {\n\
             default { y = 0; }\n\
             default { z = 1; }\n\
             wave { a = 1; }\n\
             }",
        );
        assert!(errs.iter().any(|d| d.message.contains("at most one `default`")));
    }

    #[test]
    fn expect_default_defaults_to_false_without_the_keyword() {
        let d = parse_ok("test T for M { wave { a = 1; expect y = 1; } }");
        assert!(!d.tests[0].waves[0].expect_default);
        assert_eq!(d.tests[0].waves[0].expects.len(), 1);
    }

    #[test]
    fn cases_block_expands_to_one_wave_per_column() {
        let d = parse_ok(
            "cases C for M {\n\
             a = [1, 2, 4];\n\
             b = [2, 2, 3];\n\
             expect first = [1, 2, 3];\n\
             }",
        );
        assert_eq!(d.tests.len(), 1);
        let t = &d.tests[0];
        assert_eq!(t.name, "C");
        assert_eq!(t.module, "M");
        assert_eq!(t.waves.len(), 3, "three columns -> three waves");
        // Wave 1 is column 0: a=1, b=2, expect first=1.
        let w0 = &t.waves[0];
        assert_eq!(w0.inputs.len(), 2);
        assert_eq!(w0.expects.len(), 1);
        assert!(matches!(w0.inputs[0].1.kind, StimValKind::Time(1)));
        assert!(matches!(w0.expects[0].1.kind, StimValKind::Time(1)));
        // Wave 3 is column 2: a=4, b=3, expect first=3.
        let w2 = &t.waves[2];
        assert!(matches!(w2.inputs[0].1.kind, StimValKind::Time(4)));
        assert!(matches!(w2.expects[0].1.kind, StimValKind::Time(3)));
    }

    #[test]
    fn cases_block_allows_expect_default() {
        let d = parse_ok(
            "cases C for M {\n\
             a = [1, 2];\n\
             expect default;\n\
             }",
        );
        assert_eq!(d.tests[0].waves.len(), 2);
        assert!(d.tests[0].waves.iter().all(|w| w.expect_default));
    }

    #[test]
    fn cases_with_mismatched_vector_lengths_is_an_error() {
        let errs = parse_err(
            "cases C for M {\n\
             a = [1, 2, 3];\n\
             b = [2, 2];\n\
             }",
        );
        assert!(errs.iter().any(|d| d.message.contains("same length")));
    }

    // ---- Section 4 examples parse cleanly (Req 1.28 smoke) --------------------------------

    #[test]
    fn section4_examples_parse_without_error() {
        // Sort2 (max/min written as calls that are generic until elaboration).
        parse_ok(
            "module Sort2 (input time a, input time b, output time lo, output time hi) {\n\
             lo = min(a, b);\n\
             hi = max(a, b);\n\
             }",
        );
        // Wta3.
        parse_ok(
            "module Wta3 (input time x[3], output time y[3]) {\n\
             y[0] = x[0] < min(x[1], x[2]);\n\
             y[1] = x[1] < min(x[0], x[2]);\n\
             y[2] = x[2] < min(x[0], x[1]);\n\
             }",
        );
        // DelayN.
        parse_ok("module DelayN #(param N = 3) (input time a, output time y) { y = a + N; }");
        // MinN.
        parse_ok(
            "module MinN #(param N = 4) (input time x[N], output time y) {\n\
             wire time acc[N];\n\
             acc[0] = x[0];\n\
             for i in 1..N { acc[i] = min(acc[i - 1], x[i]); }\n\
             y = acc[N - 1];\n\
             }",
        );
        // Top.
        parse_ok(
            "module Top (input time p, input time q, output time r) {\n\
             wire time lo; wire time hi;\n\
             inst s = Sort2 (.a(p), .b(q), .lo(lo), .hi(hi));\n\
             r = hi + 2;\n\
             }",
        );
    }

    // ========================================================================================
    // Subtask 6.1: error location + plain-language message (Req 1.4, 1.16, 1.26)
    // ========================================================================================

    #[test]
    fn non_associative_chaining_is_a_parse_error_with_location_and_message() {
        // `a < b < c` without parentheses must be rejected (Req 1.16).
        let errs = parse_err(
            "module M (input time a, input time b, input time c, output time y) {\n\
             y = a < b < c;\n\
             }",
        );
        let e = &errs[0];
        assert!(e.is_error());
        // Location: file is carried, line/col are 1-based and point into the source (Req 1.26).
        assert_eq!(&*e.span.file, "test.sta");
        assert!(e.span.line >= 1 && e.span.col >= 1);
        // Plain-language message naming the problem, not a bare code (Req 1.26, 13.2).
        assert!(
            e.message.contains("cannot be chained") || e.message.contains("non-associative"),
            "message should explain the chaining rule, got: {}",
            e.message
        );
        assert!(e.message.contains(' '), "message must be plain language");
    }

    #[test]
    fn each_relational_operator_rejects_chaining() {
        for op in ["<", ">", "==", "!="] {
            let src = format!(
                "module M (input time a, input time b, input time c, output time y) {{ y = a {op} b {op} c; }}"
            );
            let errs = parse_err(&src);
            assert!(
                errs.iter().any(|e| e.message.contains("chained")),
                "chaining `{op}` should be rejected, got: {errs:?}"
            );
        }
    }

    #[test]
    fn missing_semicolon_is_a_parse_error_with_location() {
        // The assignment `y = a` is missing its trailing `;` before `}` (Req 1.4).
        let errs = parse_err("module M (input time a, output time y) { y = a }");
        let e = errs
            .iter()
            .find(|e| e.message.contains(';'))
            .unwrap_or(&errs[0]);
        assert!(e.is_error());
        assert_eq!(&*e.span.file, "test.sta");
        assert!(e.span.line >= 1 && e.span.col >= 1);
        // The message names the expected construct in words (Req 1.26).
        assert!(
            e.message.contains("expected") && e.message.contains(';'),
            "message should say a `;` was expected, got: {}",
            e.message
        );
    }

    #[test]
    fn missing_semicolon_on_wire_decl_is_reported() {
        let errs = parse_err("module M (input time a, output time y) { wire time w = a y = w; }");
        assert!(errs.iter().any(|e| e.message.contains("expected") && e.message.contains(';')));
    }

    #[test]
    fn error_message_carries_file_line_col_via_render() {
        let errs = parse_err("module M (input time a, output time y) { y = a }");
        let rendered = errs[0].render();
        // Rendered form is `file:line:col: error: <message>` (Req 13.1).
        assert!(rendered.starts_with("test.sta:"), "got: {rendered}");
        assert!(rendered.contains(": error: "), "got: {rendered}");
    }

    #[test]
    fn unexpected_top_level_token_is_a_clear_error() {
        let errs = parse_err("wire time x;");
        assert!(errs
            .iter()
            .any(|e| e.message.contains("top-level") && e.message.contains("expected")));
    }

    #[test]
    fn recovers_and_reports_second_error_after_the_first() {
        // Two modules, each with a missing `;`. The parser should report both (Req 2.17 spirit).
        let errs = parse_err(
            "module A (input time a, output time y) { y = a }\n\
             module B (input time a, output time y) { y = a }",
        );
        let semi_errors = errs.iter().filter(|e| e.message.contains(';')).count();
        assert!(semi_errors >= 2, "expected at least two missing-`;` errors, got {errs:?}");
    }

    #[test]
    fn empty_source_parses_to_an_empty_design() {
        let d = parse_ok("");
        assert!(d.modules.is_empty());
        assert!(d.funcs.is_empty());
        assert!(d.tests.is_empty());
        assert!(d.imports.is_empty());
    }
}
