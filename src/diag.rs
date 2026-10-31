//! Diagnostics: `Span`, `Severity`, `Diagnostic`, plain-language rendering, and a collector
//! (Requirements 2.17, 13.1, 13.2).
//!
//! All diagnostics share this one type and are *collected*, not thrown, so a single compile
//! reports every problem it can find rather than stopping at the first error (Req 2.17).

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// A source location identifying the start of a construct.
///
/// `line` and `col` are 1-based (Req 4.2): the first character of a file is at `line = 1`,
/// `col = 1`. `file` is the path or name of the source unit the location belongs to.
///
/// `Span` derives serde `Serialize`/`Deserialize` so it can travel inside the serde-serializable
/// netlist IR as each node's `loc` provenance (Req 4.2, 4.9, 12.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Span {
    pub file: Arc<str>,
    pub line: u32,
    pub col: u32,
}

impl Span {
    /// Create a span, enforcing the 1-based invariant for `line` and `col` (Req 4.2).
    ///
    /// # Panics
    ///
    /// Panics if `line` or `col` is zero. Callers constructing spans from byte offsets must
    /// convert to 1-based line/column before building a `Span`.
    pub fn new(file: impl Into<Arc<str>>, line: u32, col: u32) -> Self {
        assert!(line >= 1, "Span.line must be >= 1 (Req 4.2), got {line}");
        assert!(col >= 1, "Span.col must be >= 1 (Req 4.2), got {col}");
        Span {
            file: file.into(),
            line,
            col,
        }
    }
}

impl fmt::Display for Span {
    /// Renders as `file:line:col` (Req 13.1).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.file, self.line, self.col)
    }
}

/// The severity of a diagnostic. Errors fail compilation; warnings never stop it (Req 13.6, 13.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// The lowercased label used by the plain-language renderer.
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A single diagnostic: a located, plain-language message with optional extra context.
///
/// `message` is a plain-language description that names the violated rule and what triggered it,
/// never a bare numeric or symbolic code (Req 13.2). `note` carries optional extra context such as
/// the time-invariance explanation (Req 13.4) or a combinational-cycle path (Req 13.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    pub note: Option<String>,
}

impl Diagnostic {
    /// Build an error diagnostic with no note.
    pub fn error(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            span,
            message: message.into(),
            note: None,
        }
    }

    /// Build a warning diagnostic with no note.
    pub fn warning(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Warning,
            span,
            message: message.into(),
            note: None,
        }
    }

    /// Attach a note (builder style), replacing any existing note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// True if this diagnostic is an error (vs a warning).
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Render as plain language: `file:line:col: severity: message` with the optional note on a
    /// following indented line (Req 13.1, 13.2).
    pub fn render(&self) -> String {
        let mut out = format!("{}: {}: {}", self.span, self.severity, self.message);
        if let Some(note) = &self.note {
            out.push_str("\n  note: ");
            out.push_str(note);
        }
        out
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

/// Accumulates multiple diagnostics across a compile so every detected problem is reported in one
/// pass rather than stopping at the first (Req 2.17).
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// A fresh, empty collector.
    pub fn new() -> Self {
        Diagnostics { items: Vec::new() }
    }

    /// Add a diagnostic to the collection.
    pub fn push(&mut self, diag: Diagnostic) {
        self.items.push(diag);
    }

    /// Add an error diagnostic.
    pub fn error(&mut self, span: Span, message: impl Into<String>) {
        self.push(Diagnostic::error(span, message));
    }

    /// Add a warning diagnostic.
    pub fn warning(&mut self, span: Span, message: impl Into<String>) {
        self.push(Diagnostic::warning(span, message));
    }

    /// Merge all diagnostics from another collector into this one.
    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(other);
    }

    /// True when no diagnostics have been collected.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Total number of diagnostics collected (errors and warnings).
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True if at least one *error* was collected. Warnings alone do not count, so a compile with
    /// only warnings still succeeds (Req 13.6, 13.7, 13.8).
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(Diagnostic::is_error)
    }

    /// Number of error-severity diagnostics.
    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.is_error()).count()
    }

    /// Number of warning-severity diagnostics.
    pub fn warning_count(&self) -> usize {
        self.items.iter().filter(|d| !d.is_error()).count()
    }

    /// Iterate over the collected diagnostics in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// Borrow the collected diagnostics as a slice.
    pub fn as_slice(&self) -> &[Diagnostic] {
        &self.items
    }

    /// Consume the collector, yielding the owned diagnostics.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }

    /// Render every diagnostic in insertion order, one per entry, joined by newlines (Req 13.1).
    pub fn render(&self) -> String {
        self.items
            .iter()
            .map(Diagnostic::render)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl FromIterator<Diagnostic> for Diagnostics {
    fn from_iter<I: IntoIterator<Item = Diagnostic>>(iter: I) -> Self {
        Diagnostics {
            items: iter.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span() -> Span {
        Span::new("design.sta", 3, 7)
    }

    // --- Span invariants (Req 4.2) ---

    #[test]
    fn span_accepts_one_based_line_and_col() {
        let s = Span::new("f.sta", 1, 1);
        assert_eq!(s.line, 1);
        assert_eq!(s.col, 1);
        assert_eq!(&*s.file, "f.sta");
    }

    #[test]
    #[should_panic(expected = "Span.line must be >= 1")]
    fn span_rejects_zero_line() {
        Span::new("f.sta", 0, 1);
    }

    #[test]
    #[should_panic(expected = "Span.col must be >= 1")]
    fn span_rejects_zero_col() {
        Span::new("f.sta", 1, 0);
    }

    // --- Rendering format: file:line:col + severity + message (+ note) (Req 13.1, 13.2) ---

    #[test]
    fn span_renders_file_line_col() {
        assert_eq!(span().to_string(), "design.sta:3:7");
    }

    #[test]
    fn error_renders_with_location_severity_and_message() {
        let d = Diagnostic::error(span(), "wire `s` has no driver");
        assert_eq!(d.render(), "design.sta:3:7: error: wire `s` has no driver");
    }

    #[test]
    fn warning_renders_with_warning_severity() {
        let d = Diagnostic::warning(span(), "wire `t` is declared but never read");
        assert_eq!(
            d.render(),
            "design.sta:3:7: warning: wire `t` is declared but never read"
        );
    }

    #[test]
    fn note_renders_on_an_indented_following_line() {
        let d = Diagnostic::error(span(), "numeric literal used as a wire")
            .with_note("using a numeric constant as a wire would violate time-invariance");
        assert_eq!(
            d.render(),
            "design.sta:3:7: error: numeric literal used as a wire\n  \
             note: using a numeric constant as a wire would violate time-invariance"
        );
    }

    #[test]
    fn message_is_plain_language_not_a_bare_code() {
        // Req 13.2: the message names the rule and the trigger in words.
        let d = Diagnostic::error(span(), "duplicate name `a` was already declared");
        assert!(d.message.contains(' '));
        assert!(!d.message.chars().all(|c| c.is_ascii_digit()));
    }

    // --- Collector: accumulation and error-vs-warning distinction (Req 2.17, 13.6-13.8) ---

    #[test]
    fn collector_starts_empty() {
        let diags = Diagnostics::new();
        assert!(diags.is_empty());
        assert!(!diags.has_errors());
        assert_eq!(diags.len(), 0);
    }

    #[test]
    fn collector_accumulates_multiple_diagnostics_in_one_pass() {
        let mut diags = Diagnostics::new();
        diags.error(span(), "undeclared name `x`");
        diags.error(span(), "undeclared name `y`");
        diags.warning(span(), "wire `z` is unused");
        // All three are retained rather than stopping at the first error (Req 2.17).
        assert_eq!(diags.len(), 3);
        assert_eq!(diags.error_count(), 2);
        assert_eq!(diags.warning_count(), 1);
    }

    #[test]
    fn only_warnings_do_not_count_as_errors() {
        let mut diags = Diagnostics::new();
        diags.warning(span(), "wire `a` is unused");
        diags.warning(span(), "output `b` is unconnected");
        // A compile with only warnings has no errors and should still succeed (Req 13.6-13.8).
        assert!(!diags.is_empty());
        assert!(!diags.has_errors());
        assert_eq!(diags.warning_count(), 2);
    }

    #[test]
    fn any_error_makes_has_errors_true() {
        let mut diags = Diagnostics::new();
        diags.warning(span(), "wire `a` is unused");
        diags.error(span(), "multiple drivers for wire `a`");
        assert!(diags.has_errors());
    }

    #[test]
    fn iter_preserves_insertion_order() {
        let mut diags = Diagnostics::new();
        diags.error(span(), "first");
        diags.warning(span(), "second");
        diags.error(span(), "third");
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, ["first", "second", "third"]);
    }

    #[test]
    fn render_lists_every_diagnostic_one_per_line() {
        let mut diags = Diagnostics::new();
        diags.error(span(), "first problem");
        diags.warning(span(), "second problem");
        let rendered = diags.render();
        assert_eq!(
            rendered,
            "design.sta:3:7: error: first problem\n\
             design.sta:3:7: warning: second problem"
        );
    }

    #[test]
    fn extend_merges_collections() {
        let mut a = Diagnostics::new();
        a.error(span(), "a1");
        let b: Diagnostics = vec![
            Diagnostic::warning(span(), "b1"),
            Diagnostic::error(span(), "b2"),
        ]
        .into_iter()
        .collect();
        a.extend(b);
        assert_eq!(a.len(), 3);
        assert_eq!(a.error_count(), 2);
    }
}
