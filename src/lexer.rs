//! Lexer: characters to tokens (Requirement 1.1-1.7, 1.12).
//!
//! Converts a source `&str` into a `Vec<Token>` using the centralized [`OperatorTable`]
//! (`crate::operators`) for multi-character operator symbols. Every token carries a 1-based
//! [`Span`] (Req 1.25 support). The lexer collects *all* diagnostics it can in one pass rather
//! than stopping at the first error (consistent with the project-wide collect-don't-throw policy),
//! returning `Err(Vec<Diagnostic>)` only if at least one error was found.
//!
//! Responsibilities:
//! - line (`//`) and block (`/* */`) comments, skipped (Req 1.1)
//! - identifiers `[A-Za-z_][A-Za-z0-9_]*` and keywords, including the reserved `stage`/`reset`
//!   and the `inf` spike-time literal (Req 1.2, 1.5)
//! - whitespace (space, tab, CR, LF) as separators (Req 1.3)
//! - non-negative decimal integer literals range-checked against the `SpikeTime` width (Req 1.6),
//!   with an out-of-range error carrying file:line:col (Req 1.7)
//! - operator symbols via maximal munch from the operator table so `<`, `>`, `==`, `!=`
//!   tokenize as single `Op` tokens before any one-character prefix (Req 1.12)
//! - the multi-character punctuation `->` (Arrow) and `..` (DotDot), also via maximal munch

use crate::diag::{Diagnostic, Span};
use crate::operators::OperatorTable;

/// The `SpikeTime` integer width the lexer range-checks integer literals against (Req 1.6).
///
/// `INF` is reserved as the type maximum (see the runtime header, Req 5.8-5.9), so the largest
/// value a *numeric literal* may take is the type maximum. `inf` is written with the `inf` keyword,
/// not as a numeric literal, so no literal needs to exceed [`TimeWidth::max_literal`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TimeWidth {
    /// 16-bit spike times: literals `0..=65535`.
    W16,
    /// 32-bit spike times: literals `0..=4294967295`.
    W32,
}

impl TimeWidth {
    /// The maximum value a numeric integer literal may take for this width (Req 1.6).
    ///
    /// This equals the type maximum (`65535` for 16-bit, `4294967295` for 32-bit). `INF` shares
    /// that maximum at runtime, but infinity is written as the `inf` keyword rather than a numeric
    /// literal, so numeric literals occupy the whole `0..=max` range.
    pub fn max_literal(self) -> u64 {
        match self {
            TimeWidth::W16 => u16::MAX as u64,
            TimeWidth::W32 => u32::MAX as u64,
        }
    }

    /// Human-readable bit count, used in diagnostics.
    pub fn bits(self) -> u32 {
        match self {
            TimeWidth::W16 => 16,
            TimeWidth::W32 => 32,
        }
    }
}

impl Default for TimeWidth {
    /// 32-bit by default (Req 5.8; the lexer accepts a width parameter defaulting to 32).
    fn default() -> Self {
        TimeWidth::W32
    }
}

/// A lexical token kind (design.md Data Models).
///
/// `Ident` carries its text, `Int` its parsed value (already range-checked), and `Op` the surface
/// operator symbol resolved through the [`OperatorTable`]. Keyword variants cover every reserved
/// word, including `stage`/`reset` which are reserved but unused in v1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// An identifier `[A-Za-z_][A-Za-z0-9_]*` that is not a keyword (Req 1.2).
    Ident(String),
    /// A non-negative decimal integer literal, already range-checked (Req 1.6).
    Int(u64),
    /// The `inf` spike-time literal (Req 1.5, 1.6).
    Inf,

    // --- Keywords (Req 1.5) ---
    KwModule,
    KwInput,
    KwOutput,
    KwWire,
    KwInst,
    KwTest,
    KwWave,
    KwExpect,
    KwFor,
    KwIn,
    KwParam,
    KwAssert,
    KwImport,
    /// `include` — pull in another user file, resolved relative to the including file (multi-file).
    KwInclude,
    /// `cases` — introduces a table-form test expanded to one wave per column.
    KwCases,
    /// `default` — the per-test `default { .. }` block, and the `expect default;` wave form.
    KwDefault,
    /// Reserved keyword `stage`, unused in v1 (Req 1.5).
    KwStage,
    /// Reserved keyword `reset`, unused in v1 (Req 1.5).
    KwReset,

    /// An operator symbol resolved via the [`OperatorTable`], e.g. `"<"`, `">"`, `"=="`, `"!="`
    /// (Req 1.12).
    Op(String),

    // --- Compile-time integer arithmetic punctuation ---
    Plus,
    Minus,
    Star,
    Slash,
    Percent,

    // --- Brackets ---
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,

    // --- Other punctuation ---
    Hash,
    Dot,
    Comma,
    Semi,
    Eq,
    /// `->` (function return arrow).
    Arrow,
    /// `..` (range in `for i in lo..hi`).
    DotDot,
}

/// A token: a [`TokenKind`] paired with the [`Span`] of its first character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    /// Construct a token from a kind and span.
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span }
    }
}

/// Map an identifier string to its keyword [`TokenKind`], or `None` if it is an ordinary
/// identifier (Req 1.5). `inf` maps to [`TokenKind::Inf`] (Req 1.6).
fn keyword_kind(ident: &str) -> Option<TokenKind> {
    Some(match ident {
        "module" => TokenKind::KwModule,
        "input" => TokenKind::KwInput,
        "output" => TokenKind::KwOutput,
        "wire" => TokenKind::KwWire,
        "inst" => TokenKind::KwInst,
        "test" => TokenKind::KwTest,
        "wave" => TokenKind::KwWave,
        "expect" => TokenKind::KwExpect,
        "for" => TokenKind::KwFor,
        "in" => TokenKind::KwIn,
        "param" => TokenKind::KwParam,
        "assert" => TokenKind::KwAssert,
        "import" => TokenKind::KwImport,
        "include" => TokenKind::KwInclude,
        "cases" => TokenKind::KwCases,
        "default" => TokenKind::KwDefault,
        "inf" => TokenKind::Inf,
        "stage" => TokenKind::KwStage,
        "reset" => TokenKind::KwReset,
        _ => return None,
    })
}

/// Tokenize `src` into a `Vec<Token>`, consulting `ops` for multi-character operator symbols and
/// range-checking integer literals against `width`.
///
/// Returns `Ok(tokens)` when the whole source tokenized cleanly, or `Err(diagnostics)` with every
/// error found in one pass (unexpected characters, unterminated block comments, out-of-range
/// literals). The returned token stream does not include a trailing end-of-input marker; callers
/// (the parser) track position by index.
pub fn tokenize(
    src: &str,
    file: &str,
    ops: &OperatorTable,
    width: TimeWidth,
) -> Result<Vec<Token>, Vec<Diagnostic>> {
    Lexer::new(src, file, ops, width).run()
}

/// Internal lexer state: a cursor over the source characters tracking 1-based line/column.
struct Lexer<'a> {
    /// Source characters, indexed by `pos`. Collecting to a `Vec<char>` keeps indexing O(1) and
    /// correct for multi-byte characters (only ASCII is meaningful, but non-ASCII is reported, not
    /// panicked on).
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    file: &'a str,
    /// Operator symbols sorted longest-first for maximal munch (Req 1.12), cached once from the
    /// [`OperatorTable`] so the hot loop needs no further table access.
    symbols: Vec<&'a str>,
    width: TimeWidth,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, file: &'a str, ops: &'a OperatorTable, width: TimeWidth) -> Self {
        Lexer {
            chars: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            file,
            symbols: ops.symbols_longest_first(),
            width,
            tokens: Vec::new(),
            diags: Vec::new(),
        }
    }

    /// Peek the character at the current position, if any.
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    /// Peek the character `n` positions ahead of the current one, if any.
    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    /// A [`Span`] at the current cursor position.
    fn span_here(&self) -> Span {
        Span::new(self.file, self.line, self.col)
    }

    /// Advance the cursor one character, updating line/column. A line feed increments the line and
    /// resets the column; every other character (including a lone carriage return) advances the
    /// column. CRLF is handled correctly because the `\n` does the line bump and the preceding
    /// `\r` only nudges the column, which the `\n` then resets.
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    /// True if the remaining source starts with `s`.
    fn starts_with(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    /// Push a single-character token of `kind`, consuming the character.
    fn push_single(&mut self, kind: TokenKind) {
        let span = self.span_here();
        self.bump();
        self.tokens.push(Token::new(kind, span));
    }

    /// Run the lexer to completion, returning the tokens or the collected diagnostics.
    fn run(mut self) -> Result<Vec<Token>, Vec<Diagnostic>> {
        while self.peek().is_some() {
            self.step();
        }
        if self.diags.is_empty() {
            Ok(self.tokens)
        } else {
            Err(self.diags)
        }
    }

    /// Process one lexeme (whitespace, comment, identifier/keyword, number, operator, or
    /// punctuation) starting at the current cursor.
    fn step(&mut self) {
        let c = match self.peek() {
            Some(c) => c,
            None => return,
        };

        // Whitespace separators (Req 1.3): space, tab, CR, LF.
        if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
            self.bump();
            return;
        }

        // Comments (Req 1.1).
        if c == '/' {
            match self.peek_at(1) {
                Some('/') => {
                    self.skip_line_comment();
                    return;
                }
                Some('*') => {
                    self.skip_block_comment();
                    return;
                }
                _ => {}
            }
        }

        // Identifiers and keywords (Req 1.2, 1.5).
        if is_ident_start(c) {
            self.lex_ident();
            return;
        }

        // Integer literals (Req 1.6, 1.7).
        if c.is_ascii_digit() {
            self.lex_number();
            return;
        }

        // Operator symbols from the table, longest first (maximal munch, Req 1.12).
        if self.try_lex_operator() {
            return;
        }

        // Fixed multi- and single-character punctuation.
        self.lex_punct(c);
    }

    /// Skip a `//` line comment up to but not including the newline (Req 1.1).
    fn skip_line_comment(&mut self) {
        // Consume the two slashes.
        self.bump();
        self.bump();
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
    }

    /// Skip a `/* ... */` block comment (Req 1.1). An unterminated block comment is an error. Block
    /// comments do not nest.
    fn skip_block_comment(&mut self) {
        let start = self.span_here();
        // Consume the opening `/*`.
        self.bump();
        self.bump();
        loop {
            match self.peek() {
                None => {
                    self.diags.push(
                        Diagnostic::error(
                            start,
                            "unterminated block comment: reached end of file before `*/`",
                        )
                        .with_note("a `/*` block comment must be closed with a matching `*/`"),
                    );
                    return;
                }
                Some('*') if self.peek_at(1) == Some('/') => {
                    self.bump();
                    self.bump();
                    return;
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Lex an identifier or keyword (Req 1.2, 1.5). The leading character is already known to be an
    /// identifier start.
    fn lex_ident(&mut self) {
        let span = self.span_here();
        let mut text = String::new();
        while let Some(c) = self.peek() {
            if is_ident_continue(c) {
                text.push(c);
                self.bump();
            } else {
                break;
            }
        }
        let kind = keyword_kind(&text).unwrap_or(TokenKind::Ident(text));
        self.tokens.push(Token::new(kind, span));
    }

    /// Lex a non-negative decimal integer literal, range-checking against the width (Req 1.6, 1.7).
    ///
    /// On overflow the value is still consumed fully (so lexing continues past it) and an
    /// out-of-range error with file:line:col is collected (Req 1.7). The emitted token uses `0` as
    /// a placeholder value since an error was recorded.
    fn lex_number(&mut self) {
        let span = self.span_here();
        let mut digits = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                digits.push(c);
                self.bump();
            } else {
                break;
            }
        }

        let max = self.width.max_literal();
        // Parse into u128 first so a very long run of digits does not itself overflow u64 parsing;
        // then compare against the width maximum.
        let value: Option<u64> = match digits.parse::<u128>() {
            Ok(v) if v <= max as u128 => Some(v as u64),
            _ => None,
        };

        match value {
            Some(v) => self.tokens.push(Token::new(TokenKind::Int(v), span)),
            None => {
                self.diags.push(
                    Diagnostic::error(
                        span.clone(),
                        format!(
                            "integer literal `{digits}` is out of range for a {}-bit spike time \
                             (valid range is 0 to {max})",
                            self.width.bits()
                        ),
                    )
                    .with_note(
                        "write `inf` for the never-spikes value; numeric literals must fit the \
                         selected SpikeTime width (change it with `--time-width`)",
                    ),
                );
                // Emit a placeholder token so index-based positions downstream stay sensible.
                self.tokens.push(Token::new(TokenKind::Int(0), span));
            }
        }
    }

    /// Try to match an operator-table symbol at the cursor via maximal munch (Req 1.12). Returns
    /// `true` and emits an [`TokenKind::Op`] token if one matched.
    fn try_lex_operator(&mut self) -> bool {
        // Find the matching symbol first (immutable borrow of `self.symbols`), then mutate.
        let matched: Option<(String, usize)> = self
            .symbols
            .iter()
            .find(|sym| self.starts_with(sym))
            .map(|sym| ((*sym).to_string(), sym.chars().count()));

        if let Some((symbol, char_len)) = matched {
            let span = self.span_here();
            for _ in 0..char_len {
                self.bump();
            }
            self.tokens.push(Token::new(TokenKind::Op(symbol), span));
            true
        } else {
            false
        }
    }

    /// Lex fixed punctuation, handling the multi-character `->` and `..` via maximal munch before
    /// their single-character prefixes. An unrecognized character is an error.
    fn lex_punct(&mut self, c: char) {
        // Multi-character punctuation first (maximal munch).
        if c == '-' && self.peek_at(1) == Some('>') {
            let span = self.span_here();
            self.bump();
            self.bump();
            self.tokens.push(Token::new(TokenKind::Arrow, span));
            return;
        }
        if c == '.' && self.peek_at(1) == Some('.') {
            let span = self.span_here();
            self.bump();
            self.bump();
            self.tokens.push(Token::new(TokenKind::DotDot, span));
            return;
        }

        let single = match c {
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash,
            '%' => TokenKind::Percent,
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '{' => TokenKind::LBrace,
            '}' => TokenKind::RBrace,
            '[' => TokenKind::LBrack,
            ']' => TokenKind::RBrack,
            '#' => TokenKind::Hash,
            '.' => TokenKind::Dot,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semi,
            '=' => TokenKind::Eq,
            other => {
                let span = self.span_here();
                self.bump();
                self.diags.push(Diagnostic::error(
                    span,
                    format!("unexpected character `{other}`"),
                ));
                return;
            }
        };
        self.push_single(single);
    }
}

/// True if `c` may start an identifier: a letter or underscore (Req 1.2).
fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}

/// True if `c` may continue an identifier: a letter, digit, or underscore (Req 1.2).
fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tokenize with the builtin operator table and 32-bit width (unwrapping on success).
    fn lex(src: &str) -> Vec<Token> {
        tokenize(src, "test.sta", &OperatorTable::builtin(), TimeWidth::W32)
            .unwrap_or_else(|d| panic!("expected clean tokenization, got {d:?}"))
    }

    /// Tokenize at a given width, returning the Result for error assertions.
    fn lex_width(src: &str, width: TimeWidth) -> Result<Vec<Token>, Vec<Diagnostic>> {
        tokenize(src, "test.sta", &OperatorTable::builtin(), width)
    }

    /// Just the kinds, dropping spans, for compact comparisons.
    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    // --- Maximal munch for operator symbols (Req 1.12) ---

    #[test]
    fn maximal_munch_lt_pipe_is_single_op_not_lt_then_pipe() {
        // `<` must tokenize as one Op, never as `<` + `|`.
        assert_eq!(kinds("<"), vec![TokenKind::Op("<".into())]);
    }

    #[test]
    fn maximal_munch_gt_pipe_is_single_op() {
        assert_eq!(kinds(">"), vec![TokenKind::Op(">".into())]);
    }

    #[test]
    fn maximal_munch_eq_eq_is_op_not_two_eq_tokens() {
        // `==` is an operator symbol, distinct from the single `=` punctuation token.
        assert_eq!(kinds("=="), vec![TokenKind::Op("==".into())]);
    }

    #[test]
    fn maximal_munch_bang_eq_is_single_op() {
        assert_eq!(kinds("!="), vec![TokenKind::Op("!=".into())]);
    }

    #[test]
    fn single_eq_is_eq_punctuation_distinct_from_eq_eq() {
        // A lone `=` is the assignment/connection punctuation, not the `==` operator.
        assert_eq!(kinds("="), vec![TokenKind::Eq]);
    }

    #[test]
    fn relational_operators_in_expression_context() {
        // a < b > c == d != e  -> idents interleaved with four distinct Op tokens.
        let got = kinds("a < b > c == d != e");
        assert_eq!(
            got,
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Op("<".into()),
                TokenKind::Ident("b".into()),
                TokenKind::Op(">".into()),
                TokenKind::Ident("c".into()),
                TokenKind::Op("==".into()),
                TokenKind::Ident("d".into()),
                TokenKind::Op("!=".into()),
                TokenKind::Ident("e".into()),
            ]
        );
    }

    #[test]
    fn op_tokens_adjacent_without_whitespace_still_munch_maximally() {
        // `a<b` with no spaces must still split as a, <, b.
        assert_eq!(
            kinds("a<b"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Op("<".into()),
                TokenKind::Ident("b".into()),
            ]
        );
    }

    // --- Comments (Req 1.1) ---

    #[test]
    fn line_comment_is_skipped() {
        assert_eq!(
            kinds("a // this is ignored\nb"),
            vec![TokenKind::Ident("a".into()), TokenKind::Ident("b".into())]
        );
    }

    #[test]
    fn line_comment_at_end_of_file_without_newline() {
        assert_eq!(kinds("a // trailing"), vec![TokenKind::Ident("a".into())]);
    }

    #[test]
    fn block_comment_is_skipped() {
        assert_eq!(
            kinds("a /* b c d */ e"),
            vec![TokenKind::Ident("a".into()), TokenKind::Ident("e".into())]
        );
    }

    #[test]
    fn block_comment_spanning_multiple_lines_is_skipped() {
        assert_eq!(
            kinds("a /* line one\n line two */ b"),
            vec![TokenKind::Ident("a".into()), TokenKind::Ident("b".into())]
        );
    }

    #[test]
    fn unterminated_block_comment_is_an_error() {
        let err = lex_width("a /* never closed", TimeWidth::W32)
            .expect_err("an unterminated block comment must be an error");
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unterminated block comment"));
    }

    // --- Identifiers vs keywords (Req 1.2, 1.5) ---

    #[test]
    fn plain_identifier() {
        assert_eq!(kinds("foo_bar123"), vec![TokenKind::Ident("foo_bar123".into())]);
    }

    #[test]
    fn identifier_may_start_with_underscore() {
        assert_eq!(kinds("_t"), vec![TokenKind::Ident("_t".into())]);
    }

    #[test]
    fn every_keyword_is_recognized() {
        let cases = [
            ("module", TokenKind::KwModule),
            ("input", TokenKind::KwInput),
            ("output", TokenKind::KwOutput),
            ("wire", TokenKind::KwWire),
            ("inst", TokenKind::KwInst),
            ("test", TokenKind::KwTest),
            ("wave", TokenKind::KwWave),
            ("expect", TokenKind::KwExpect),
            ("for", TokenKind::KwFor),
            ("in", TokenKind::KwIn),
            ("param", TokenKind::KwParam),
            ("assert", TokenKind::KwAssert),
            ("import", TokenKind::KwImport),
            ("stage", TokenKind::KwStage),
            ("reset", TokenKind::KwReset),
        ];
        for (text, expected) in cases {
            assert_eq!(kinds(text), vec![expected.clone()], "keyword `{text}`");
        }
    }

    #[test]
    fn reserved_stage_and_reset_tokenize_as_keywords() {
        // Reserved but unused in v1; they must still be keyword tokens, not identifiers.
        assert_eq!(kinds("stage"), vec![TokenKind::KwStage]);
        assert_eq!(kinds("reset"), vec![TokenKind::KwReset]);
    }

    #[test]
    fn keyword_prefix_is_still_an_identifier() {
        // `modulee`, `inputs`, `inferior` share prefixes with keywords but are identifiers.
        assert_eq!(kinds("modulee"), vec![TokenKind::Ident("modulee".into())]);
        assert_eq!(kinds("inputs"), vec![TokenKind::Ident("inputs".into())]);
        assert_eq!(kinds("inferior"), vec![TokenKind::Ident("inferior".into())]);
    }

    // --- inf literal (Req 1.5, 1.6) ---

    #[test]
    fn inf_tokenizes_as_the_inf_literal() {
        assert_eq!(kinds("inf"), vec![TokenKind::Inf]);
    }

    #[test]
    fn inf_as_identifier_prefix_is_an_identifier() {
        assert_eq!(kinds("infinity"), vec![TokenKind::Ident("infinity".into())]);
    }

    // --- Integer literals (Req 1.6) ---

    #[test]
    fn zero_literal() {
        assert_eq!(kinds("0"), vec![TokenKind::Int(0)]);
    }

    #[test]
    fn multi_digit_literal() {
        assert_eq!(kinds("12345"), vec![TokenKind::Int(12345)]);
    }

    #[test]
    fn boundary_literal_is_accepted_for_each_width() {
        // The max literal exactly equals the width maximum and must be accepted.
        let w16 = lex_width("65535", TimeWidth::W16).expect("65535 fits 16-bit");
        assert_eq!(w16[0].kind, TokenKind::Int(65535));

        let w32 = lex_width("4294967295", TimeWidth::W32).expect("4294967295 fits 32-bit");
        assert_eq!(w32[0].kind, TokenKind::Int(4294967295));
    }

    // --- Out-of-range literal error (Req 1.7) for both widths ---

    #[test]
    fn out_of_range_literal_for_16_bit_is_an_error_with_location() {
        // 65536 is one past the 16-bit maximum.
        let err = lex_width("65536", TimeWidth::W16).expect_err("65536 overflows 16-bit");
        assert_eq!(err.len(), 1);
        assert!(err[0].is_error());
        assert!(err[0].message.contains("out of range"));
        assert!(err[0].message.contains("16-bit"));
        // file:line:col is present on the diagnostic span (Req 1.7).
        assert_eq!(&*err[0].span.file, "test.sta");
        assert_eq!(err[0].span.line, 1);
        assert_eq!(err[0].span.col, 1);
    }

    #[test]
    fn sixteen_bit_literal_is_fine_at_thirty_two_bit_width() {
        // The same value that overflows 16-bit is valid at 32-bit width.
        let toks = lex_width("65536", TimeWidth::W32).expect("65536 fits 32-bit");
        assert_eq!(toks[0].kind, TokenKind::Int(65536));
    }

    #[test]
    fn out_of_range_literal_for_32_bit_is_an_error_with_location() {
        // 4294967296 is one past the 32-bit maximum.
        let err =
            lex_width("4294967296", TimeWidth::W32).expect_err("4294967296 overflows 32-bit");
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("out of range"));
        assert!(err[0].message.contains("32-bit"));
    }

    #[test]
    fn absurdly_long_literal_does_not_panic_and_is_an_error() {
        // Far beyond u64/u128; must be reported, not overflow-parsed.
        let err = lex_width("999999999999999999999999999999999", TimeWidth::W32)
            .expect_err("a huge literal must be out of range");
        assert!(err[0].message.contains("out of range"));
    }

    #[test]
    fn out_of_range_literal_reports_correct_column() {
        // The literal starts after `wire x = ` so its column should reflect that position.
        let err = lex_width("wire x = 70000;", TimeWidth::W16)
            .expect_err("70000 overflows 16-bit");
        assert_eq!(err[0].span.line, 1);
        assert_eq!(err[0].span.col, 10); // 1-based column of the `7`.
    }

    // --- Spans: 1-based line/col correctness (Req 1.25 support) ---

    #[test]
    fn first_token_span_is_line_1_col_1() {
        let toks = lex("abc");
        assert_eq!(toks[0].span.line, 1);
        assert_eq!(toks[0].span.col, 1);
    }

    #[test]
    fn columns_advance_across_a_line() {
        // `a bc` -> a at col 1, bc at col 3.
        let toks = lex("a bc");
        assert_eq!((toks[0].span.line, toks[0].span.col), (1, 1));
        assert_eq!((toks[1].span.line, toks[1].span.col), (1, 3));
    }

    #[test]
    fn newline_advances_line_and_resets_column() {
        let toks = lex("a\nbb\nc");
        assert_eq!((toks[0].span.line, toks[0].span.col), (1, 1)); // a
        assert_eq!((toks[1].span.line, toks[1].span.col), (2, 1)); // bb
        assert_eq!((toks[2].span.line, toks[2].span.col), (3, 1)); // c
    }

    #[test]
    fn crlf_newline_resets_column() {
        // `a\r\nb` -> b begins at line 2, col 1 despite the CR.
        let toks = lex("a\r\nb");
        assert_eq!((toks[1].span.line, toks[1].span.col), (2, 1));
    }

    #[test]
    fn tabs_count_as_a_single_column_separator() {
        // Tab is one column advance (no tab-stop expansion); b lands at col 3.
        let toks = lex("a\tbc");
        assert_eq!((toks[1].span.line, toks[1].span.col), (1, 3));
    }

    #[test]
    fn span_after_block_comment_tracks_lines() {
        // Line 2 is " lines */ x"; counting 1-based columns puts `x` at col 11.
        let toks = lex("/* two\n lines */ x");
        assert_eq!((toks[0].span.line, toks[0].span.col), (2, 11)); // x
    }

    #[test]
    fn op_span_points_at_first_char_of_symbol() {
        let toks = lex("a < b");
        // `<` starts at column 3.
        assert_eq!(toks[1].kind, TokenKind::Op("<".into()));
        assert_eq!((toks[1].span.line, toks[1].span.col), (1, 3));
    }

    // --- Punctuation and maximal munch for `->` / `..` ---

    #[test]
    fn arrow_is_a_single_token() {
        assert_eq!(kinds("->"), vec![TokenKind::Arrow]);
    }

    #[test]
    fn minus_is_minus_when_not_followed_by_gt() {
        // A lone `-` followed by a space is Minus; `->` only forms when `>` is adjacent.
        assert_eq!(
            kinds("a - b"),
            vec![
                TokenKind::Ident("a".into()),
                TokenKind::Minus,
                TokenKind::Ident("b".into()),
            ]
        );
    }

    #[test]
    fn dotdot_is_a_single_token_distinct_from_two_dots_logic() {
        assert_eq!(kinds(".."), vec![TokenKind::DotDot]);
    }

    #[test]
    fn single_dot_is_dot() {
        assert_eq!(kinds("."), vec![TokenKind::Dot]);
    }

    #[test]
    fn range_in_for_loop_header() {
        // for i in 0..8  — DotDot sits between the two integer literals.
        assert_eq!(
            kinds("for i in 0..8"),
            vec![
                TokenKind::KwFor,
                TokenKind::Ident("i".into()),
                TokenKind::KwIn,
                TokenKind::Int(0),
                TokenKind::DotDot,
                TokenKind::Int(8),
            ]
        );
    }

    #[test]
    fn all_single_char_punctuation() {
        assert_eq!(
            kinds("+-*/%(){}[]#.,;="),
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::LBrack,
                TokenKind::RBrack,
                TokenKind::Hash,
                TokenKind::Dot,
                TokenKind::Comma,
                TokenKind::Semi,
                TokenKind::Eq,
            ]
        );
    }

    // --- Unexpected character error ---

    #[test]
    fn unexpected_character_is_an_error() {
        let err = lex_width("a @ b", TimeWidth::W32).expect_err("`@` is not a valid character");
        assert_eq!(err.len(), 1);
        assert!(err[0].message.contains("unexpected character"));
        assert_eq!(err[0].span.col, 3);
    }

    // --- A representative module fragment tokenizes cleanly ---

    #[test]
    fn module_fragment_tokenizes() {
        let src = "module Sort2(input time a, input time b, output time lo) {\n\
                   wire time lo = min(a, b); // first arrival\n\
                   lo = a < b;\n\
                   }";
        let toks = lex(src);
        // Spot-check the opening and that no diagnostics were produced (lex() unwraps).
        assert_eq!(toks[0].kind, TokenKind::KwModule);
        assert_eq!(toks[1].kind, TokenKind::Ident("Sort2".into()));
        assert!(toks.iter().any(|t| t.kind == TokenKind::Op("<".into())));
    }

    #[test]
    fn empty_source_yields_no_tokens() {
        assert_eq!(lex(""), Vec::<Token>::new());
    }

    #[test]
    fn whitespace_only_source_yields_no_tokens() {
        assert_eq!(lex("  \t\r\n  "), Vec::<Token>::new());
    }

    // --- Default width is 32-bit ---

    #[test]
    fn default_time_width_is_32_bit() {
        assert_eq!(TimeWidth::default(), TimeWidth::W32);
        assert_eq!(TimeWidth::default().max_literal(), u32::MAX as u64);
    }
}
