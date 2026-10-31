//! Import resolution and standard-library auto-import (Requirements 9.1, 10.4, 10.5, 14.3).
//!
//! The frontend never special-cases the standard library. Instead, the single `import` mechanism
//! (Req 1.24) carries it: an [`ImportResolver`] holds an ordered list of filesystem search paths
//! and, when configured with [`ImportResolver::with_auto_std`], prepends `std.sta` ahead of a
//! user design's own imports (Req 9.1). A user file and `std.sta` therefore travel the identical
//! lexer / parser / sema / elaborate path — the library has no privileged compiler support, so a
//! new library block is a pure `std.sta` edit with no compiler change (Req 10.4, 10.5).
//!
//! ## Known library path convention
//!
//! The default search path for the auto-imported `std.sta` is resolved in this order:
//!
//! 1. the `QUACKC_STDLIB_PATH` environment variable, if set (points at the directory that
//!    *contains* `std.sta`), so a packaged install can relocate the library; then
//! 2. the `std/` directory alongside the crate manifest (`CARGO_MANIFEST_DIR/std`), which is where
//!    the library lives in this repository.
//!
//! Additional search paths (for user `import`s that name a file by a bare path) can be appended
//! with [`ImportResolver::with_search_path`]. Resolution tries each search path in order and
//! returns the first readable match.
//!
//! ## Cycle detection
//!
//! `import` forms a directed graph over source units. [`ImportResolver::resolve_all`] walks it
//! depth-first starting from a set of roots (the auto-`std.sta` first, then the user's imports),
//! tracking the active resolution chain; revisiting a target already on the chain is an import
//! cycle and is reported as a [`Diagnostic`] (Req: cycles in imports are an error). A target that
//! has already been fully resolved on a different branch is simply skipped (shared, not cyclic).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::ast::Design;
use crate::diag::{Diagnostic, Span};
use crate::lexer::TimeWidth;
use crate::operators::OperatorTable;
use crate::parser::parse_source;

/// The canonical standard-library file name (Decision 1). This is the target the auto-import
/// prepends and the file name the resolver looks for under each library search path.
pub const STD_LIB_FILE: &str = "std.sta";

/// The environment variable that overrides the default standard-library directory. When set, it
/// must point at the directory that *contains* `std.sta`.
pub const STDLIB_PATH_ENV: &str = "QUACKC_STDLIB_PATH";

/// A resolved source unit: the text of a `.sta` file together with the name used as its `file`
/// field in spans (so diagnostics point at the real path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceUnit {
    /// The resolved target name as written in the `import` (e.g. `std.sta`), used to key the
    /// import graph and to detect cycles.
    pub target: String,
    /// The path the unit was read from, used as the `file` in every [`Span`] produced for it.
    pub path: PathBuf,
    /// The full source text.
    pub text: String,
}

impl SourceUnit {
    /// The display name used for spans / diagnostics: the resolved path as a string.
    pub fn file_name(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

/// Resolves `import` targets to [`SourceUnit`]s against an ordered list of search paths, optionally
/// auto-importing `std.sta` ahead of a user design's own imports.
#[derive(Clone, Debug, Default)]
pub struct ImportResolver {
    /// Directories searched, in order, for an `import` target file.
    search_paths: Vec<PathBuf>,
    /// Whether `std.sta` is auto-imported ahead of user imports (Req 9.1).
    auto_std: bool,
}

impl ImportResolver {
    /// A resolver with no search paths and no auto-import. Use [`with_default_std_path`] /
    /// [`with_search_path`] / [`with_auto_std`] to configure it.
    ///
    /// [`with_default_std_path`]: ImportResolver::with_default_std_path
    /// [`with_search_path`]: ImportResolver::with_search_path
    /// [`with_auto_std`]: ImportResolver::with_auto_std
    pub fn new() -> Self {
        ImportResolver {
            search_paths: Vec::new(),
            auto_std: false,
        }
    }

    /// Append an explicit search-path directory (builder style). Directories are searched in the
    /// order they were added.
    pub fn with_search_path(mut self, dir: impl Into<PathBuf>) -> Self {
        self.search_paths.push(dir.into());
        self
    }

    /// Append the known default standard-library directory (builder style): the
    /// `QUACKC_STDLIB_PATH` override if set, otherwise `CARGO_MANIFEST_DIR/std`.
    pub fn with_default_std_path(mut self) -> Self {
        self.search_paths.push(default_std_dir());
        self
    }

    /// Enable auto-import of `std.sta` ahead of the user's own imports (Req 9.1).
    ///
    /// This also ensures the default standard-library directory is on the search path (appending
    /// it if no search path has been configured yet) so the auto-imported `std.sta` resolves out
    /// of the box.
    pub fn with_auto_std(mut self) -> Self {
        self.auto_std = true;
        if self.search_paths.is_empty() {
            self.search_paths.push(default_std_dir());
        }
        self
    }

    /// The search paths this resolver consults, in order.
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Whether `std.sta` auto-import is enabled.
    pub fn auto_std_enabled(&self) -> bool {
        self.auto_std
    }

    /// The ordered list of import targets to resolve for a design, with the auto-`std.sta` (if
    /// enabled) prepended ahead of the user's own imports (Req 9.1). Duplicate targets are kept in
    /// first-seen order and deduplicated so a user who also writes `import std.sta;` does not get
    /// the library twice.
    pub fn import_order(&self, user_imports: &[String]) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut order = Vec::new();
        if self.auto_std {
            if seen.insert(STD_LIB_FILE.to_string()) {
                order.push(STD_LIB_FILE.to_string());
            }
        }
        for imp in user_imports {
            if seen.insert(imp.clone()) {
                order.push(imp.clone());
            }
        }
        order
    }

    /// Resolve a single `import` target to a [`SourceUnit`] by searching each configured search
    /// path for a matching file. The target may name the file directly (`std.sta`) or omit the
    /// `.sta` extension (`std`); both are tried under every search path.
    ///
    /// Returns a located [`Diagnostic`] error if the target is not found under any search path.
    pub fn resolve(&self, target: &str) -> Result<SourceUnit, Diagnostic> {
        for dir in &self.search_paths {
            for candidate in candidate_file_names(target) {
                let path = dir.join(&candidate);
                if path.is_file() {
                    return read_unit(target, path);
                }
            }
        }
        Err(Diagnostic::error(
            import_span(target),
            format!(
                "cannot resolve import `{target}`: no matching `.sta` file found on the library \
                 search path"
            ),
        )
        .with_note(format!(
            "searched {} path(s); set `{STDLIB_PATH_ENV}` to the directory containing `std.sta` \
             or add the file's directory to the search path",
            self.search_paths.len()
        )))
    }

    /// Resolve the full transitive import set for a design, starting from `user_imports` (with the
    /// auto-`std.sta` prepended when enabled), detecting import cycles (Req: cycles are an error).
    ///
    /// Returns the resolved units in dependency order (a target appears after everything it
    /// imports), or the collected diagnostics (an unresolved target or an import cycle). Because a
    /// resolved unit's own `import` statements must be parsed to discover its dependencies, the
    /// caller supplies a `parse_imports` closure that extracts the import target strings from a
    /// unit's text; this keeps the resolver independent of the parser's concrete types.
    pub fn resolve_all(
        &self,
        user_imports: &[String],
        parse_imports: impl Fn(&SourceUnit) -> Vec<String>,
    ) -> Result<Vec<SourceUnit>, Vec<Diagnostic>> {
        let mut diags = Vec::new();
        let mut resolved_order: Vec<SourceUnit> = Vec::new();
        let mut done: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = Vec::new();

        for root in self.import_order(user_imports) {
            self.visit(
                &root,
                &parse_imports,
                &mut done,
                &mut stack,
                &mut resolved_order,
                &mut diags,
            );
        }

        if diags.is_empty() {
            Ok(resolved_order)
        } else {
            Err(diags)
        }
    }

    /// Depth-first visit of one import target for [`resolve_all`], post-ordering the resolved units
    /// and reporting a cycle when the target is already on the active resolution chain.
    #[allow(clippy::too_many_arguments)]
    fn visit(
        &self,
        target: &str,
        parse_imports: &impl Fn(&SourceUnit) -> Vec<String>,
        done: &mut HashSet<String>,
        stack: &mut Vec<String>,
        resolved_order: &mut Vec<SourceUnit>,
        diags: &mut Vec<Diagnostic>,
    ) {
        if done.contains(target) {
            // Already fully resolved on another branch: shared, not cyclic.
            return;
        }
        if let Some(pos) = stack.iter().position(|t| t == target) {
            // The target is on the active chain: an import cycle (Req: cycles are an error).
            let mut chain: Vec<String> = stack[pos..].to_vec();
            chain.push(target.to_string());
            diags.push(
                Diagnostic::error(
                    import_span(target),
                    format!("import cycle detected: {}", chain.join(" -> ")),
                )
                .with_note(
                    "imports must form a directed acyclic graph; a file cannot import itself \
                     directly or transitively",
                ),
            );
            return;
        }

        let unit = match self.resolve(target) {
            Ok(u) => u,
            Err(d) => {
                diags.push(d);
                return;
            }
        };

        stack.push(target.to_string());
        for dep in parse_imports(&unit) {
            self.visit(&dep, parse_imports, done, stack, resolved_order, diags);
        }
        stack.pop();

        done.insert(target.to_string());
        resolved_order.push(unit);
    }
}

/// The default standard-library directory: `QUACKC_STDLIB_PATH` if set, otherwise the `std/`
/// directory alongside the crate manifest.
fn default_std_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(STDLIB_PATH_ENV) {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("std")
}

/// Candidate file names for a target: the target as written, and (if it has no extension) the
/// target with `.sta` appended. `std.sta` yields `[std.sta]`; `std` yields `[std, std.sta]`.
fn candidate_file_names(target: &str) -> Vec<String> {
    let mut names = vec![target.to_string()];
    if Path::new(target).extension().is_none() {
        names.push(format!("{target}.sta"));
    }
    names
}

/// Read a resolved file into a [`SourceUnit`], mapping an I/O error to a located diagnostic.
fn read_unit(target: &str, path: PathBuf) -> Result<SourceUnit, Diagnostic> {
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(SourceUnit {
            target: target.to_string(),
            path,
            text,
        }),
        Err(e) => Err(Diagnostic::error(
            import_span(target),
            format!("cannot read import `{target}` from `{}`: {e}", path.display()),
        )),
    }
}

/// A synthetic span naming the import target, used for resolution errors that have no in-source
/// location of their own.
fn import_span(target: &str) -> Span {
    Span::new(target.to_string(), 1, 1)
}

/// Parse a user design's source together with the auto-imported standard library into a single
/// merged [`Design`], ready for `sema::analyze` (Req 9.1, 10.4).
///
/// The `resolver` must have auto-import enabled (via [`ImportResolver::with_auto_std`]) for the
/// standard library to be prepended. The user source is parsed first to discover its own `import`
/// statements; those plus the auto-`std.sta` are resolved transitively (with cycle detection) and
/// every resolved unit is parsed and merged. Because `std.sta` and the user file go through the
/// identical parser, a library block is just more modules / funcs in the merged design — no
/// compiler change is needed to add one (Req 10.4, 10.5).
///
/// Returns the merged design, or every parse / resolution diagnostic collected along the way.
pub fn load_design(
    resolver: &ImportResolver,
    user_src: &str,
    user_file: &str,
    ops: &OperatorTable,
    width: TimeWidth,
) -> Result<Design, Vec<Diagnostic>> {
    // Default: do not run the standard library's own testbenches (Req: std tests off by default).
    load_design_scoped(resolver, user_src, user_file, ops, width, false)
}

/// Like [`load_design`], but `include_std_tests` controls whether the auto-imported standard
/// library's `test`/`cases` blocks are merged (and therefore run). `include`d user files NEVER
/// contribute tests — only the top user file's tests run (plus the stdlib's when requested).
pub fn load_design_scoped(
    resolver: &ImportResolver,
    user_src: &str,
    user_file: &str,
    ops: &OperatorTable,
    width: TimeWidth,
    include_std_tests: bool,
) -> Result<Design, Vec<Diagnostic>> {
    // Parse the user file first so we can read its own imports and includes.
    let user_design = parse_source(user_src, user_file, ops, width)?;
    let user_imports: Vec<String> = user_design.imports.iter().map(|i| i.path.clone()).collect();

    // Resolve the auto-std + user imports transitively (library search path), with cycle detection.
    let units = resolver.resolve_all(&user_imports, |unit| {
        parse_source(&unit.text, &unit.file_name(), ops, width)
            .map(|d| d.imports.into_iter().map(|i| i.path).collect())
            .unwrap_or_default()
    })?;

    let mut merged = Design {
        modules: Vec::new(),
        funcs: Vec::new(),
        tests: Vec::new(),
        imports: Vec::new(),
        includes: Vec::new(),
    };
    let mut diags = Vec::new();

    // 1. Imported units (the standard library and any import-path files). Modules/funcs always
    //    merge; their tests merge ONLY when `include_std_tests` is set.
    for unit in &units {
        match parse_source(&unit.text, &unit.file_name(), ops, width) {
            Ok(d) => merge_design(&mut merged, d, include_std_tests),
            Err(mut e) => diags.append(&mut e),
        }
    }

    // 2. `include`d user files, resolved RELATIVE to the including file's directory, transitively
    //    (with cycle detection). Their modules/funcs merge; their tests NEVER run (only the top
    //    file's tests do).
    let include_units = match resolve_includes(&user_design, user_file, ops, width) {
        Ok(u) => u,
        Err(mut e) => {
            diags.append(&mut e);
            Vec::new()
        }
    };
    for unit in &include_units {
        match parse_source(&unit.text, &unit.file_name(), ops, width) {
            Ok(d) => merge_design(&mut merged, d, false),
            Err(mut e) => diags.append(&mut e),
        }
    }

    if !diags.is_empty() {
        return Err(diags);
    }

    // 3. The top user file last, with its tests always included.
    merge_design(&mut merged, user_design, true);
    Ok(merged)
}

/// Resolve a design's `include` statements to [`SourceUnit`]s, each resolved relative to the
/// directory of the file that declared it, following includes transitively with cycle detection.
/// Returns units in dependency order (a file after everything it includes).
fn resolve_includes(
    root_design: &Design,
    root_file: &str,
    ops: &OperatorTable,
    width: TimeWidth,
) -> Result<Vec<SourceUnit>, Vec<Diagnostic>> {
    let mut out = Vec::new();
    let mut done: HashSet<PathBuf> = HashSet::new();
    let mut stack: Vec<PathBuf> = Vec::new();
    let mut diags = Vec::new();

    // Seed the walk with the root file's includes, each resolved against the root file's directory.
    let root_dir = parent_dir(Path::new(root_file));
    for inc in &root_design.includes {
        visit_include(
            &root_dir, &inc.path, &inc.span, ops, width,
            &mut out, &mut done, &mut stack, &mut diags,
        );
    }
    if diags.is_empty() { Ok(out) } else { Err(diags) }
}

/// Depth-first resolve one include target relative to `base_dir`, recursing into the included
/// file's own includes (resolved relative to ITS directory). Reports a cycle or a missing file.
#[allow(clippy::too_many_arguments)]
fn visit_include(
    base_dir: &Path,
    target: &str,
    span: &Span,
    ops: &OperatorTable,
    width: TimeWidth,
    out: &mut Vec<SourceUnit>,
    done: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
    diags: &mut Vec<Diagnostic>,
) {
    // Resolve the path relative to the including file's directory; try the name as written and,
    // if it has no extension, with `.sta` appended.
    let resolved = resolve_include_path(base_dir, target);
    let path = match resolved {
        Some(p) => p,
        None => {
            diags.push(
                Diagnostic::error(
                    span.clone(),
                    format!("cannot resolve include `{target}`: no such file relative to `{}`", base_dir.display()),
                )
                .with_note("an `include` path is resolved relative to the file that declares it"),
            );
            return;
        }
    };
    if done.contains(&path) {
        return; // already included on another branch (shared, not cyclic)
    }
    if stack.contains(&path) {
        diags.push(
            Diagnostic::error(
                span.clone(),
                format!("include cycle detected at `{}`", path.display()),
            )
            .with_note("includes must form a directed acyclic graph"),
        );
        return;
    }

    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            diags.push(Diagnostic::error(
                span.clone(),
                format!("cannot read include `{}`: {e}", path.display()),
            ));
            return;
        }
    };

    // Parse to find THIS file's own includes, resolved relative to its own directory.
    stack.push(path.clone());
    if let Ok(d) = parse_source(&text, &path.to_string_lossy(), ops, width) {
        let dir = parent_dir(&path);
        for inc in &d.includes {
            visit_include(&dir, &inc.path, &inc.span, ops, width, out, done, stack, diags);
        }
    }
    stack.pop();

    done.insert(path.clone());
    out.push(SourceUnit {
        target: target.to_string(),
        path,
        text,
    });
}

/// Resolve an include path string against `base_dir`, trying the path as written and (if it has no
/// extension) with `.sta` appended. Returns the first existing file.
fn resolve_include_path(base_dir: &Path, target: &str) -> Option<PathBuf> {
    for cand in candidate_file_names(target) {
        let p = base_dir.join(&cand);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// The directory containing `file`, or `.` if it has no parent.
fn parent_dir(file: &Path) -> PathBuf {
    file.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Append one design's modules and funcs into `into`. Tests merge only when `with_tests` is set,
/// which enforces the test-scoping rule (only the top file's tests, plus the stdlib's on request;
/// never an include's). Import/include statements are not carried across — already resolved.
fn merge_design(into: &mut Design, from: Design, with_tests: bool) {
    into.modules.extend(from.modules);
    into.funcs.extend(from.funcs);
    if with_tests {
        into.tests.extend(from.tests);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// The repository's `std/` directory, where the real `std.sta` lives.
    fn repo_std_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("std")
    }

    #[test]
    fn auto_std_prepends_std_ahead_of_user_imports() {
        let r = ImportResolver::new()
            .with_search_path(repo_std_dir())
            .with_auto_std();
        let order = r.import_order(&["foo.sta".to_string(), "bar.sta".to_string()]);
        assert_eq!(order, vec!["std.sta", "foo.sta", "bar.sta"]);
    }

    #[test]
    fn auto_std_dedupes_an_explicit_std_import() {
        let r = ImportResolver::new().with_auto_std();
        // A user who also writes `import std.sta;` does not get the library twice.
        let order = r.import_order(&["std.sta".to_string(), "x.sta".to_string()]);
        assert_eq!(order, vec!["std.sta", "x.sta"]);
    }

    #[test]
    fn without_auto_std_only_user_imports_are_ordered() {
        let r = ImportResolver::new();
        let order = r.import_order(&["x.sta".to_string()]);
        assert_eq!(order, vec!["x.sta"]);
    }

    #[test]
    fn resolve_finds_real_std_sta() {
        let r = ImportResolver::new().with_auto_std();
        let unit = r.resolve(STD_LIB_FILE).expect("std.sta must resolve");
        assert_eq!(unit.target, "std.sta");
        assert!(unit.path.ends_with("std.sta"));
        // The real library defines the core derived ops.
        assert!(unit.text.contains("func le"));
        assert!(unit.text.contains("func max"));
        assert!(unit.text.contains("module Gt"));
    }

    #[test]
    fn resolve_accepts_an_extensionless_target() {
        let r = ImportResolver::new().with_search_path(repo_std_dir());
        let unit = r.resolve("std").expect("`std` should resolve to std.sta");
        assert!(unit.path.ends_with("std.sta"));
    }

    #[test]
    fn resolve_missing_target_is_a_diagnostic() {
        let r = ImportResolver::new().with_search_path(repo_std_dir());
        let err = r
            .resolve("definitely_not_here.sta")
            .expect_err("a missing import must error");
        assert!(err.is_error());
        assert!(err.message.contains("cannot resolve import"));
    }

    #[test]
    fn resolve_all_resolves_std_via_auto_import() {
        let r = ImportResolver::new().with_auto_std();
        // std.sta imports nothing further, so the chain is just [std.sta].
        let units = r
            .resolve_all(&[], |_| Vec::new())
            .expect("auto-std should resolve");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].target, "std.sta");
    }

    #[test]
    fn resolve_all_detects_a_direct_cycle() {
        // Build a temp dir with a.sta importing itself transitively through b.sta -> a.sta.
        let dir = std::env::temp_dir().join(format!("quackc_import_cycle_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("a.sta"), "import b.sta;\n").unwrap();
        fs::write(dir.join("b.sta"), "import a.sta;\n").unwrap();

        let r = ImportResolver::new().with_search_path(&dir);
        // Parse imports by scanning for `import <target>;` lines (test-local, no parser dependency).
        let parse = |u: &SourceUnit| scan_imports(&u.text);
        let err = r
            .resolve_all(&["a.sta".to_string()], parse)
            .expect_err("a -> b -> a is a cycle");
        assert!(err.iter().any(|d| d.message.contains("import cycle detected")));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_all_allows_a_shared_diamond_without_a_false_cycle() {
        // top -> {left, right}; left -> shared; right -> shared. `shared` is visited twice but is
        // not a cycle (it is a shared dependency), so this must resolve cleanly.
        let dir = std::env::temp_dir().join(format!("quackc_import_diamond_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("top.sta"), "import left.sta;\nimport right.sta;\n").unwrap();
        fs::write(dir.join("left.sta"), "import shared.sta;\n").unwrap();
        fs::write(dir.join("right.sta"), "import shared.sta;\n").unwrap();
        fs::write(dir.join("shared.sta"), "// leaf\n").unwrap();

        let r = ImportResolver::new().with_search_path(&dir);
        let parse = |u: &SourceUnit| scan_imports(&u.text);
        let units = r
            .resolve_all(&["top.sta".to_string()], parse)
            .expect("a shared diamond is not a cycle");
        // Post-order: shared before left/right before top; shared appears exactly once.
        let targets: Vec<&str> = units.iter().map(|u| u.target.as_str()).collect();
        assert_eq!(targets.iter().filter(|t| **t == "shared.sta").count(), 1);
        assert_eq!(targets.last(), Some(&"top.sta"));

        let _ = fs::remove_dir_all(&dir);
    }

    /// Minimal, test-local extraction of `import <target>;` targets from source text, so the cycle
    /// tests do not depend on the parser.
    fn scan_imports(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("import ") {
                let target = rest.trim_end_matches(';').trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
            }
        }
        out
    }

    // --- include resolution + test scoping ---------------------------------------------------

    /// A resolver whose only search path is the real std dir (so std.sta auto-imports).
    fn std_resolver() -> ImportResolver {
        ImportResolver::new().with_auto_std()
    }

    #[test]
    fn include_pulls_in_a_sibling_files_modules_but_not_its_tests() {
        let dir = std::env::temp_dir().join(format!("quack_inc_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        // helper.sta defines a module AND a test; the top file includes it.
        fs::write(
            dir.join("helper.sta"),
            "module Helper (input time a, output time y) { y = a + 1; }\n\
             test HelperTb for Helper { wave { a = 1; expect y = 2; } }\n",
        ).unwrap();
        let top = "include helper.sta;\n\
                   module Top (input time a, output time y) {\n\
                   wire time t; inst h = Helper (.a(a), .y(t)); y = t; }\n\
                   test TopTb for Top { wave { a = 1; expect y = 2; } }\n";
        let top_path = dir.join("top.sta");
        fs::write(&top_path, top).unwrap();

        let d = load_design(
            &std_resolver(),
            top,
            &top_path.to_string_lossy(),
            &OperatorTable::builtin(),
            TimeWidth::W32,
        )
        .expect("include should resolve a sibling file");

        // The included module is present...
        assert!(d.modules.iter().any(|m| m.name == "Helper"), "Helper module should be included");
        assert!(d.modules.iter().any(|m| m.name == "Top"));
        // ...but ONLY the top file's test is kept (HelperTb is dropped; no std tests by default).
        let names: Vec<&str> = d.tests.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"TopTb"), "the top file's test must run");
        assert!(!names.contains(&"HelperTb"), "an included file's test must NOT run");
        assert!(!names.iter().any(|n| n.contains("Exhaustive")), "std tests off by default");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_resolves_a_subfolder_path() {
        let dir = std::env::temp_dir().join(format!("quack_incsub_{}", std::process::id()));
        let sub = dir.join("helpers");
        let _ = fs::create_dir_all(&sub);
        fs::write(sub.join("math.sta"), "module Inc (input time a, output time y) { y = a + 1; }\n").unwrap();
        let top = "include helpers/math.sta;\n\
                   module Top (input time a, output time y) { wire time t; inst i = Inc (.a(a), .y(t)); y = t; }\n";
        let top_path = dir.join("top.sta");
        fs::write(&top_path, top).unwrap();
        let d = load_design(&std_resolver(), top, &top_path.to_string_lossy(), &OperatorTable::builtin(), TimeWidth::W32)
            .expect("subfolder include should resolve");
        assert!(d.modules.iter().any(|m| m.name == "Inc"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn include_cycle_is_an_error() {
        let dir = std::env::temp_dir().join(format!("quack_inccyc_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        fs::write(dir.join("a.sta"), "include b.sta;\nmodule A (input time a, output time y) { y = a; }\n").unwrap();
        fs::write(dir.join("b.sta"), "include a.sta;\nmodule B (input time a, output time y) { y = a; }\n").unwrap();
        let top = "include a.sta;\nmodule Top (input time a, output time y) { y = a; }\n";
        let top_path = dir.join("top.sta");
        fs::write(&top_path, top).unwrap();
        let errs = load_design(&std_resolver(), top, &top_path.to_string_lossy(), &OperatorTable::builtin(), TimeWidth::W32)
            .expect_err("an include cycle must error");
        assert!(errs.iter().any(|e| e.message.contains("include cycle")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_include_is_an_error() {
        let dir = std::env::temp_dir().join(format!("quack_incmiss_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let top = "include nope.sta;\nmodule Top (input time a, output time y) { y = a; }\n";
        let top_path = dir.join("top.sta");
        fs::write(&top_path, top).unwrap();
        let errs = load_design(&std_resolver(), top, &top_path.to_string_lossy(), &OperatorTable::builtin(), TimeWidth::W32)
            .expect_err("a missing include must error");
        assert!(errs.iter().any(|e| e.message.contains("cannot resolve include")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn std_tests_merge_only_when_requested() {
        let top = "module Top (input time a, output time y) { y = a; }\n";
        // Default: no std tests.
        let d_off = load_design(&std_resolver(), top, "top.sta", &OperatorTable::builtin(), TimeWidth::W32).unwrap();
        assert!(!d_off.tests.iter().any(|t| t.name.contains("Exhaustive")), "std tests off by default");
        // Opt in.
        let d_on = load_design_scoped(&std_resolver(), top, "top.sta", &OperatorTable::builtin(), TimeWidth::W32, true).unwrap();
        assert!(d_on.tests.iter().any(|t| t.name.contains("Exhaustive")), "--std-tests brings them in");
    }
}
