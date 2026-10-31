//! The `Backend` trait and the version-checking dispatcher — the stable extension point
//! (Requirements 10.8, 10.9, 4.10).
//!
//! Every consumer of the netlist IR (the functional C++ backend, the event-driven engine, the
//! JSON/DOT exporters) implements the single [`Backend`] trait and reads only `&NetlistIr`, never
//! frontend internals (Req 10.9). This is the mechanism that makes a future backend — say a
//! Verilog / race-logic emitter — purely additive: it is a new `impl Backend`, not a change to any
//! existing stage (Req 10.3).
//!
//! A backend declares the IR *major* version it understands via [`Backend::supported_ir_major`].
//! The [`run_backend`] dispatcher checks that declared major against the IR's own
//! [`SchemaVersion`] *before* calling [`Backend::run`], and rejects an incompatible IR with an
//! incompatible-version [`Diagnostic`] without consuming it (Req 10.8, 4.10). A minor-version
//! difference is always compatible; only a major mismatch is rejected (compatibility is by major
//! only — see [`SchemaVersion::is_compatible_with_major`]).

use crate::diag::{Diagnostic, Span};
use crate::ir::NetlistIr;

/// Options passed to a backend's [`run`](Backend::run). Backend-neutral knobs that every consumer
/// may honor or ignore; kept deliberately small and marked `#[non_exhaustive]` so new options can
/// be added without breaking existing `impl Backend`s or construction sites that use
/// [`BackendOptions::default`] / the builder setters.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackendOptions {
    /// The `SpikeTime` bit width the output should target (e.g. 16 or 32). A backend that does not
    /// care about width ignores this (Req 8.9).
    pub time_width: u16,
    /// Inline sub-module instances into their parent rather than emitting separate callables, when
    /// the backend supports both forms (Req 5.6).
    pub inline_instances: bool,
    /// Skip optimization-dependent code paths; the pass pipeline itself is driven elsewhere, but a
    /// backend may use this to avoid assuming an optimized graph (Req 10.6, 11.5).
    pub no_opt: bool,
}

impl Default for BackendOptions {
    fn default() -> Self {
        // 32-bit SpikeTime is the default width (Req 5.7); instances are emitted as separate
        // callables and optimization is assumed on unless explicitly disabled.
        BackendOptions {
            time_width: 32,
            inline_instances: false,
            no_opt: false,
        }
    }
}

impl BackendOptions {
    /// A fresh set of default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the target `SpikeTime` width (builder style).
    pub fn with_time_width(mut self, time_width: u16) -> Self {
        self.time_width = time_width;
        self
    }

    /// Request instance inlining (builder style).
    pub fn with_inline_instances(mut self, inline: bool) -> Self {
        self.inline_instances = inline;
        self
    }

    /// Disable optimization-dependent paths (builder style).
    pub fn with_no_opt(mut self, no_opt: bool) -> Self {
        self.no_opt = no_opt;
        self
    }
}

/// One emitted artifact: a named unit of backend output (a C++ file, a JSON document, a DOT graph,
/// an in-memory trace rendered as text, …). The `path` is a suggested relative file name; a
/// backend that produces a single blob of text uses one `EmittedFile`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmittedFile {
    /// Suggested relative path / name for this artifact, e.g. `"Top.hpp"` or `"netlist.json"`.
    pub path: String,
    /// The artifact's full text contents.
    pub contents: String,
}

impl EmittedFile {
    /// Build an emitted file from a path and its contents.
    pub fn new(path: impl Into<String>, contents: impl Into<String>) -> Self {
        EmittedFile {
            path: path.into(),
            contents: contents.into(),
        }
    }
}

/// The result of a successful [`Backend::run`]: the artifacts it produced plus any non-fatal
/// warnings. Marked `#[non_exhaustive]` so additional result fields can be added later without
/// breaking match sites outside this crate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BackendOutput {
    /// The emitted artifacts, in a backend-defined deterministic order.
    pub files: Vec<EmittedFile>,
    /// Non-fatal diagnostics (warnings) produced while running; errors are returned via the `Err`
    /// arm of [`Backend::run`] instead (Req 13.6, 13.7).
    pub warnings: Vec<Diagnostic>,
}

impl BackendOutput {
    /// An empty output (no files, no warnings).
    pub fn new() -> Self {
        Self::default()
    }

    /// An output carrying exactly the given files and no warnings.
    pub fn from_files(files: Vec<EmittedFile>) -> Self {
        BackendOutput {
            files,
            warnings: Vec::new(),
        }
    }

    /// Append an emitted file (builder style).
    pub fn with_file(mut self, file: EmittedFile) -> Self {
        self.files.push(file);
        self
    }

    /// Append a warning (builder style).
    pub fn with_warning(mut self, warning: Diagnostic) -> Self {
        self.warnings.push(warning);
        self
    }
}

/// A consumer of the stable netlist IR (Req 10.9).
///
/// An implementor reads only `&NetlistIr` and the backend-neutral [`BackendOptions`]; it must not
/// reach into frontend internals. It declares the IR *major* version it supports so the
/// [`run_backend`] dispatcher can reject an incompatible IR before any work happens (Req 10.8,
/// 4.10).
pub trait Backend {
    /// A short, stable name for this backend, used in diagnostics and tooling.
    fn name(&self) -> &str;

    /// The IR *major* schema version this backend understands; a mismatch against the IR's own
    /// major version is rejected by the dispatcher without running the backend (Req 10.8, 4.10).
    fn supported_ir_major(&self) -> u16;

    /// Run the backend over the IR, producing its artifacts or a list of error diagnostics.
    ///
    /// The dispatcher ([`run_backend`]) guarantees the IR major version is compatible before this
    /// is called, so an implementor need not re-check the schema version.
    fn run(&self, ir: &NetlistIr, opts: &BackendOptions) -> Result<BackendOutput, Vec<Diagnostic>>;
}

/// Run `backend` over `ir`, first rejecting an incompatible IR *major* version (Req 10.8, 4.10).
///
/// Before invoking [`Backend::run`], the dispatcher checks
/// `ir.schema_version.is_compatible_with_major(backend.supported_ir_major())`. On a mismatch it
/// returns an incompatible-version error [`Diagnostic`] and never calls `run`, so a backend never
/// consumes an IR whose major version it does not understand. This guard is self-contained here on
/// the dispatcher side (the `checks` stage may add an equivalent check it owns separately).
pub fn run_backend(
    backend: &dyn Backend,
    ir: &NetlistIr,
    opts: &BackendOptions,
) -> Result<BackendOutput, Vec<Diagnostic>> {
    let supported = backend.supported_ir_major();
    if !ir.schema_version.is_compatible_with_major(supported) {
        return Err(vec![incompatible_version_diagnostic(
            backend.name(),
            ir,
            supported,
        )]);
    }
    backend.run(ir, opts)
}

/// Build the incompatible-IR-version error diagnostic naming the backend, the IR's version, and
/// the major the backend supports — a plain-language message, not a bare code (Req 13.2, 4.10).
fn incompatible_version_diagnostic(backend_name: &str, ir: &NetlistIr, supported_major: u16) -> Diagnostic {
    // The IR has no single source location of its own; attribute the rejection to the top module
    // the IR was built from so the message still carries a location (Req 13.1).
    let span = Span::new(ir.top.clone(), 1, 1);
    Diagnostic::error(
        span,
        format!(
            "backend `{}` cannot consume this netlist: it supports IR schema major version {} but \
             the netlist is version {} (major {})",
            backend_name, supported_major, ir.schema_version, ir.schema_version.major
        ),
    )
    .with_note(
        "the netlist IR is versioned by major.minor; a backend only accepts an IR whose major \
         version matches the one it supports",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{NetlistIr, SchemaVersion, CURRENT_SCHEMA_VERSION};
    use std::cell::Cell;

    /// A minimal empty IR at a chosen schema version, for exercising the dispatcher without
    /// building a real design.
    fn ir_at(version: SchemaVersion) -> NetlistIr {
        NetlistIr {
            schema_version: version,
            top: "Top".to_string(),
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// A mock backend that records whether its `run` was invoked (via an interior-mutable flag) so
    /// a test can assert the dispatcher never calls `run` on an incompatible IR.
    struct MockBackend {
        supported_major: u16,
        ran: Cell<bool>,
    }

    impl MockBackend {
        fn new(supported_major: u16) -> Self {
            MockBackend {
                supported_major,
                ran: Cell::new(false),
            }
        }
    }

    impl Backend for MockBackend {
        fn name(&self) -> &str {
            "mock"
        }

        fn supported_ir_major(&self) -> u16 {
            self.supported_major
        }

        fn run(
            &self,
            ir: &NetlistIr,
            opts: &BackendOptions,
        ) -> Result<BackendOutput, Vec<Diagnostic>> {
            self.ran.set(true);
            // Echo a tiny artifact so the test can confirm the real body ran and options arrived.
            Ok(BackendOutput::from_files(vec![EmittedFile::new(
                "mock.txt",
                format!("top={} width={}", ir.top, opts.time_width),
            )]))
        }
    }

    #[test]
    fn dispatcher_runs_backend_on_compatible_ir() {
        // The IR's current major is 1; a backend supporting major 1 is compatible and runs.
        let backend = MockBackend::new(CURRENT_SCHEMA_VERSION.major);
        let ir = ir_at(CURRENT_SCHEMA_VERSION);
        let opts = BackendOptions::new().with_time_width(16);

        let out = run_backend(&backend, &ir, &opts).expect("compatible IR should run");

        assert!(backend.ran.get(), "run must be invoked for a compatible IR");
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].path, "mock.txt");
        assert_eq!(out.files[0].contents, "top=Top width=16");
    }

    #[test]
    fn compatible_across_minor_version_difference() {
        // Same major, different minor: still compatible (compatibility is by major only, Req 4.10).
        let backend = MockBackend::new(1);
        let ir = ir_at(SchemaVersion::new(1, 7));

        let out = run_backend(&backend, &ir, &BackendOptions::new())
            .expect("same major, different minor is compatible");

        assert!(backend.ran.get());
        assert_eq!(out.files.len(), 1);
    }

    #[test]
    fn dispatcher_rejects_incompatible_major_without_running() {
        // The backend supports major 2 but the IR is major 1: the dispatcher must reject it and
        // must NOT call `run` (Req 10.8, 4.10).
        let backend = MockBackend::new(2);
        let ir = ir_at(SchemaVersion::new(1, 0));

        let err = run_backend(&backend, &ir, &BackendOptions::new())
            .expect_err("incompatible major must be rejected");

        assert!(!backend.ran.get(), "run must NOT be invoked for an incompatible IR");
        assert_eq!(err.len(), 1);
        assert!(err[0].is_error());
        // Plain-language message names the backend and both versions (Req 13.2).
        let rendered = err[0].render();
        assert!(rendered.contains("mock"), "message names the backend: {rendered}");
        assert!(rendered.contains("major version 2"), "names supported major: {rendered}");
        assert!(rendered.contains("1.0"), "names the IR version: {rendered}");
    }

    #[test]
    fn rejection_diagnostic_carries_a_location_and_note() {
        let backend = MockBackend::new(2);
        let ir = ir_at(SchemaVersion::new(1, 0));

        let err = run_backend(&backend, &ir, &BackendOptions::new()).unwrap_err();
        let d = &err[0];
        // Attributed to the top module name as the source location (Req 13.1).
        assert_eq!(&*d.span.file, "Top");
        assert!(d.note.is_some(), "incompatible-version error carries an explanatory note");
    }

    #[test]
    fn backend_options_builder_sets_fields() {
        let opts = BackendOptions::new()
            .with_time_width(16)
            .with_inline_instances(true)
            .with_no_opt(true);
        assert_eq!(opts.time_width, 16);
        assert!(opts.inline_instances);
        assert!(opts.no_opt);
    }

    #[test]
    fn backend_options_default_is_32_bit_no_inline_opt_on() {
        let opts = BackendOptions::default();
        assert_eq!(opts.time_width, 32);
        assert!(!opts.inline_instances);
        assert!(!opts.no_opt);
    }

    #[test]
    fn backend_output_builders_accumulate() {
        let out = BackendOutput::new()
            .with_file(EmittedFile::new("a.cpp", "// a"))
            .with_file(EmittedFile::new("b.cpp", "// b"));
        assert_eq!(out.files.len(), 2);
        assert!(out.warnings.is_empty());
    }
}
