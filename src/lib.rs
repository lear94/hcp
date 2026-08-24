//! http-cockpit — a keyboard-driven TUI HTTP client with forensic telemetry.
//!
//! The binary is a thin shell around these modules: the terminal loop lives in
//! `main.rs`, while every decision it makes (key routing, request preparation,
//! layout, persistence) lives here so it can be tested without a terminal.

pub mod app;
pub mod cli;
pub mod clipboard;
pub mod engine;
pub mod input;
pub mod store;
pub mod syntax;
pub mod telemetry;
pub mod ui;
pub mod viewer;
