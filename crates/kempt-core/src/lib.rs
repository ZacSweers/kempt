// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Cache external tools and invoke kempt commands without spawning kempt.
//!
//! Artifact caching is enabled by default. Enable `commands` for typed execution
//! or `cli` for argument parsing and terminal rendering.
//! The cache works with caller-provided downloaders. Enable `http` to use the
//! bundled blocking HTTP client.

#[cfg(feature = "cache")]
pub mod cache;
#[cfg(feature = "commands")]
#[path = "api.rs"]
pub mod commands;
#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "commands")]
mod formatter_cache;
#[cfg(feature = "commands")]
#[path = "commands.rs"]
mod operations;
#[cfg(feature = "commands")]
mod command_args;
#[cfg(feature = "commands")]
mod config;
#[cfg(feature = "commands")]
mod formatters;
#[cfg(feature = "commands")]
mod git;
#[cfg(feature = "commands")]
mod gradle_dependencies;
#[cfg(feature = "commands")]
mod hook;
#[cfg(feature = "commands")]
mod license;
#[cfg(feature = "commands")]
mod paths;
#[cfg(feature = "commands")]
mod pipeline;
#[cfg(feature = "commands")]
mod upgrade;
#[cfg(feature = "commands")]
mod whitespace;
#[cfg(feature = "commands")]
mod process_output;
