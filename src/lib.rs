//! tether keeps a long-lived fork in step with its upstream.
//!
//! The binary is a thin shell over this library so the same engine can be
//! driven by tests and, later, embedded by other tools.

pub mod cli;
pub mod commands;
pub mod error;
pub mod git;
pub mod report;
