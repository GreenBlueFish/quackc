//! `quackc` — the compiler for the Quack s-t algebra hardware-description language.
//!
//! The compiler is implemented in Rust and emits C++17 (Requirement 15.1, 15.3). It is organized
//! as a single cargo crate with one module per pipeline stage; each module's public types are the
//! documented inter-stage interfaces (Requirement 10, 15.5).
//!
//! The modules below are stubs scaffolded by task 1 and are filled in by later tasks following the
//! pipeline order: operators -> lexer -> parser -> ast -> sema -> elaborate -> ir -> checks -> opt
//! -> codegen_cpp / sim_event / export, with `diag`, `cli`, and `testrun` as cross-cutting stages.

pub mod operators;
pub mod lexer;
pub mod parser;
pub mod import;
pub mod ast;
pub mod sema;
pub mod elaborate;
pub mod ir;
pub mod backend;
pub mod checks;
pub mod opt;
pub mod codegen_cpp;
pub mod sim_event;
pub mod export;
pub mod diag;
pub mod cli;
pub mod testrun;
