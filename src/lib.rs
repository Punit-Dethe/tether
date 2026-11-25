//! tether keeps a long-lived fork in step with its upstream.
//!
//! The binary is a thin shell over this library so the same engine can be
//! driven by tests and, later, embedded by other tools.

pub mod budget;
pub mod cli;
pub mod commands;
pub mod config;
pub mod decisions;
pub mod engine;
pub mod error;
pub mod git;
pub mod inbox;
pub mod paths;
pub mod ports;
pub mod report;
pub mod review;
pub mod syncpoint;
pub mod time;
