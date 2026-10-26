//! Built-in tools.
//!
//! Everything here is deliberately small and dependency-free: a tool is a few dozen lines
//! of `std`, and the value is in the contract (sandboxing, budgeted output, errors as
//! data) rather than in the amount of code.

pub mod fs;
pub mod git;
pub mod shell;
