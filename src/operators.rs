//! Centralized operator table: symbols, arity, precedence, associativity (Requirement 14).
//!
//! This is the single source of truth mapping a surface symbol (`<`, `>`, `==`, `!=`) to its
//! function name, arity, precedence, and associativity (Req 14.1, 14.2). Both the lexer (to
//! tokenize multi-character symbols via maximal munch) and the parser (to drive precedence and the
//! non-associative relational rule, Req 1.16) read from it. Adding a new infix operator is one
//! entry here plus a language-level `func`/module that defines its meaning — no lexer or parser
//! code changes (Req 14.3, 14.4).

use crate::diag::{Diagnostic, Span};

/// Associativity of an infix operator, used by the precedence-climbing parser.
///
/// All current relationals are [`Assoc::NonAssoc`]: chaining them without parentheses is a parse
/// error (Req 1.16). `Left`/`Right` are provided so future operators can be added by table entry
/// alone (Req 14.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Assoc {
    Left,
    Right,
    NonAssoc,
}

/// A single row of the operator table: a surface symbol and everything the lexer and parser need
/// to know about it (Req 14.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorEntry {
    /// The surface symbol as written in source, e.g. `"<"`, `">"`, `"=="`, `"!="`.
    pub symbol: String,
    /// The underlying operator or function name the symbol maps to, e.g. `"lt"`, `"gt"`, `"eq"`,
    /// `"neq"` (Req 14.1).
    pub func_name: String,
    /// Number of operands. All current relationals are binary (`2`).
    pub arity: u8,
    /// Precedence level; higher binds tighter. All current relationals share one level (Req 1.15).
    pub precedence: u8,
    /// Associativity; [`Assoc::NonAssoc`] for all current relationals (Req 1.16).
    pub assoc: Assoc,
}

impl OperatorEntry {
    /// Convenience constructor for a table entry.
    pub fn new(
        symbol: impl Into<String>,
        func_name: impl Into<String>,
        arity: u8,
        precedence: u8,
        assoc: Assoc,
    ) -> Self {
        OperatorEntry {
            symbol: symbol.into(),
            func_name: func_name.into(),
            arity,
            precedence,
            assoc,
        }
    }
}

/// The shared precedence level for the relational operators `<`, `>`, `==`, `!=` (Req 1.15).
///
/// They all sit at a single level below `+ n` and above nothing else relational, so a value here is
/// only meaningful relative to other operators the parser knows about.
const RELATIONAL_PRECEDENCE: u8 = 10;

/// The centralized operator table (Req 14.1). Owns every infix operator symbol in one place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorTable {
    entries: Vec<OperatorEntry>,
}

impl OperatorTable {
    /// Build a table from explicit entries without validating it. Prefer [`OperatorTable::builtin`]
    /// for the standard set; use this plus [`OperatorTable::validate`] when assembling a custom
    /// table.
    pub fn from_entries(entries: Vec<OperatorEntry>) -> Self {
        OperatorTable { entries }
    }

    /// The built-in table seeding the four relational operators (Req 14.1, 14.2):
    ///
    /// | symbol | func | arity | precedence           | assoc      |
    /// |--------|------|-------|----------------------|------------|
    /// | `<`    | lt   | 2     | relational (shared)  | NonAssoc   |
    /// | `>`    | gt   | 2     | relational (shared)  | NonAssoc   |
    /// | `<=`   | le   | 2     | relational (shared)  | NonAssoc   |
    /// | `>=`   | ge   | 2     | relational (shared)  | NonAssoc   |
    /// | `==`   | eq   | 2     | relational (shared)  | NonAssoc   |
    /// | `!=`   | neq  | 2     | relational (shared)  | NonAssoc   |
    /// | `^`    | min  | 2     | relational (shared)  | NonAssoc   |
    ///
    /// All are non-associative so chaining them without parentheses is a parse error (Req 1.16).
    /// `<`/`>` are the less-than / greater-than inhibit operators (formerly written `<`/`>`);
    /// `<=`/`>=` are their or-equal forms; `^` is an infix alias for the two-argument `min(a, b)`
    /// (first arrival). `le`/`ge`/`min` resolve to standard-library / primitive definitions.
    pub fn builtin() -> Self {
        let p = RELATIONAL_PRECEDENCE;
        OperatorTable {
            entries: vec![
                OperatorEntry::new("<", "lt", 2, p, Assoc::NonAssoc),
                OperatorEntry::new(">", "gt", 2, p, Assoc::NonAssoc),
                OperatorEntry::new("<=", "le", 2, p, Assoc::NonAssoc),
                OperatorEntry::new(">=", "ge", 2, p, Assoc::NonAssoc),
                OperatorEntry::new("==", "eq", 2, p, Assoc::NonAssoc),
                OperatorEntry::new("!=", "neq", 2, p, Assoc::NonAssoc),
                OperatorEntry::new("^", "min", 2, p, Assoc::NonAssoc),
            ],
        }
    }

    /// Borrow all entries in table order.
    pub fn entries(&self) -> &[OperatorEntry] {
        &self.entries
    }

    /// Look up the entry for an exact surface symbol, if any.
    pub fn lookup_symbol(&self, s: &str) -> Option<&OperatorEntry> {
        self.entries.iter().find(|e| e.symbol == s)
    }

    /// Every operator symbol sorted by descending length so the lexer can perform maximal munch:
    /// a longer symbol is always tried before any shorter prefix of it (e.g. `<` before `<`).
    ///
    /// Ties in length preserve the table's declaration order, which keeps the result deterministic.
    pub fn symbols_longest_first(&self) -> Vec<&str> {
        let mut symbols: Vec<&str> = self.entries.iter().map(|e| e.symbol.as_str()).collect();
        // `sort_by` is stable, so equal-length symbols keep their original relative order.
        symbols.sort_by(|a, b| b.len().cmp(&a.len()));
        symbols
    }

    /// Validate the table, returning an error naming the conflicting symbol if two entries share a
    /// symbol (Req 14.6).
    ///
    /// There is no real source location for a table-configuration error, so the diagnostic carries
    /// a synthetic span pointing at `<operators>:1:1`.
    pub fn validate(&self) -> Result<(), Diagnostic> {
        let mut seen: Vec<&str> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            if seen.contains(&entry.symbol.as_str()) {
                let span = Span::new("<operators>", 1, 1);
                return Err(Diagnostic::error(
                    span,
                    format!(
                        "operator symbol `{}` is configured more than once in the operator table",
                        entry.symbol
                    ),
                )
                .with_note(
                    "each operator symbol must map to exactly one function; \
                     remove or rename the duplicate entry",
                ));
            }
            seen.push(&entry.symbol);
        }
        Ok(())
    }
}

impl Default for OperatorTable {
    fn default() -> Self {
        OperatorTable::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- builtin lookup for each symbol (Req 14.1, 14.2) ---

    #[test]
    fn builtin_maps_each_relational_symbol_to_its_function() {
        let table = OperatorTable::builtin();
        for (symbol, func) in [("<", "lt"), (">", "gt"), ("<=", "le"), (">=", "ge"), ("==", "eq"), ("!=", "neq"), ("^", "min")] {
            let entry = table
                .lookup_symbol(symbol)
                .unwrap_or_else(|| panic!("`{symbol}` should be in the builtin table"));
            assert_eq!(entry.symbol, symbol);
            assert_eq!(entry.func_name, func);
            assert_eq!(entry.arity, 2);
            assert_eq!(entry.assoc, Assoc::NonAssoc);
        }
    }

    #[test]
    fn builtin_relationals_share_one_precedence_level() {
        let table = OperatorTable::builtin();
        let precedences: Vec<u8> = ["<", ">", "<=", ">=", "==", "!=", "^"]
            .iter()
            .map(|s| table.lookup_symbol(s).unwrap().precedence)
            .collect();
        assert!(
            precedences.windows(2).all(|w| w[0] == w[1]),
            "all relationals should share a precedence level, got {precedences:?}"
        );
    }

    #[test]
    fn lookup_unknown_symbol_returns_none() {
        let table = OperatorTable::builtin();
        assert!(table.lookup_symbol("<|").is_none(), "the old `<|` symbol is gone");
        assert!(table.lookup_symbol("=").is_none());
        assert!(table.lookup_symbol("~>").is_none());
    }

    // --- symbols_longest_first ordering (maximal munch, Req 1.6 support) ---

    #[test]
    fn symbols_longest_first_orders_by_descending_length() {
        // A table mixing a two-char symbol with its one-char prefix: `<=` must come before `<`.
        let table = OperatorTable::from_entries(vec![
            OperatorEntry::new("<", "lt", 2, 10, Assoc::NonAssoc),
            OperatorEntry::new("<=", "le", 2, 10, Assoc::NonAssoc),
        ]);
        let ordered = table.symbols_longest_first();
        let pos_two = ordered.iter().position(|s| *s == "<=").unwrap();
        let pos_one = ordered.iter().position(|s| *s == "<").unwrap();
        assert!(
            pos_two < pos_one,
            "longer `<=` must precede shorter `<`, got {ordered:?}"
        );
    }

    #[test]
    fn symbols_longest_first_lengths_are_non_increasing() {
        let table = OperatorTable::builtin();
        let ordered = table.symbols_longest_first();
        assert_eq!(ordered.len(), 7);
        assert!(
            ordered.windows(2).all(|w| w[0].len() >= w[1].len()),
            "lengths must be non-increasing, got {ordered:?}"
        );
    }

    // --- validate: passes on builtin, errors on a duplicate symbol (Req 14.6) ---

    #[test]
    fn validate_passes_on_builtin_table() {
        assert!(OperatorTable::builtin().validate().is_ok());
    }

    #[test]
    fn validate_errors_on_duplicate_symbol_naming_the_symbol() {
        let table = OperatorTable::from_entries(vec![
            OperatorEntry::new("<", "lt", 2, 10, Assoc::NonAssoc),
            OperatorEntry::new("<", "lessthan", 2, 10, Assoc::NonAssoc),
        ]);
        let err = table
            .validate()
            .expect_err("a duplicate symbol must be a configuration error");
        assert!(err.is_error());
        assert!(
            err.message.contains("<"),
            "the error should name the conflicting symbol, got: {}",
            err.message
        );
    }
}
