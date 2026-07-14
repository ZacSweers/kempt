// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(ValueEnum, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    /// Use git to enumerate files (`git ls-files`). Default.
    #[default]
    Vcs,
    /// Walk the filesystem from the repo root. Ignore files (`.gitignore`)
    /// are not consulted. Use `[paths].exclude` to filter results. The
    /// `.git/` directory is always pruned.
    Walk,
}

#[derive(Parser, Debug)]
#[command(
    name = "kempt",
    version,
    about = "Multi-language source formatter (Kotlin, Java, Rust, license headers, whitespace)"
)]
pub struct Cli {
    /// Path to config file. Defaults to `.kempt.toml` in the repo root.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Format files (modifies in place). Pass `--dry-run` to preview without
    /// writing.
    Format(FormatArgs),
    /// Check formatting without modifying files. Exits non-zero if changes are needed.
    Check(CheckArgs),
    /// Write a starter .kempt.toml.
    Init(InitArgs),
    /// Install a pre-commit hook in this git repo.
    InstallHook(InstallHookArgs),
    /// Run as the pre-commit hook. Invoked by `.git/hooks/pre-commit`.
    Hook,
    /// Download/refresh formatter artifacts per config.
    Update,
    /// Bump tool versions in `.kempt.toml` to the latest upstream releases.
    Upgrade(UpgradeArgs),
    /// Copy formatter artifacts into the repo (default `config/bin`) so they
    /// can be checked in. Prints the config snippet to swap in afterward.
    Vendor(VendorArgs),
    /// Inspect or clean the formatter binary cache.
    #[command(subcommand)]
    Cache(CacheCmd),
}

#[derive(Args, Debug, Default)]
pub struct InitArgs {
    /// Also write config/license-header.txt and enable [license-header].
    #[arg(long)]
    pub license_header: bool,
}

#[derive(Args, Debug, Default)]
pub struct UpgradeArgs {
    /// Show what would change without modifying `.kempt.toml`.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct VendorArgs {
    /// Directory to copy formatter artifacts into. Relative paths resolve
    /// against the repo root.
    #[arg(long, default_value = "config/bin")]
    pub dir: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum CacheCmd {
    /// List cached formatter artifacts.
    List,
    /// Remove cached artifacts not referenced by the current config.
    Prune(PruneArgs),
}

#[derive(Args, Debug, Default)]
pub struct PruneArgs {
    /// Remove every cached formatter artifact regardless of config.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Default)]
pub struct FormatArgs {
    /// Explicitly target all tracked files. Same as the default behavior;
    /// the flag exists so suggestions can be unambiguous about scope.
    /// Incompatible with `--staged`, `--discovery=walk`, or explicit paths.
    #[arg(long)]
    pub all: bool,
    /// Operate on staged files (index) only.
    /// Incompatible with `--all`, `--discovery=walk`, or explicit paths.
    #[arg(long)]
    pub staged: bool,
    /// Show what would change without modifying any files. Exits non-zero
    /// if changes are needed. Equivalent to `kempt check`.
    #[arg(long)]
    pub dry_run: bool,
    /// File discovery mode.
    #[arg(long, value_enum, default_value_t = Discovery::Vcs)]
    pub discovery: Discovery,
    /// Process explicitly targeted paths even when they match global or
    /// per-tool `paths.exclude`. Requires at least one positional target.
    #[arg(long, requires = "paths")]
    pub force: bool,
    /// Optional files, directories, or glob patterns to process. Directories
    /// are recursive and patterns are resolved relative to the current
    /// directory. When provided, scope flags are not allowed.
    #[arg(value_name = "PATH_OR_PATTERN")]
    pub paths: Vec<PathBuf>,
}

#[derive(Args, Debug, Default)]
pub struct CheckArgs {
    /// Explicitly target all tracked files. Same as the default behavior.
    /// Incompatible with `--staged`, `--discovery=walk`, or explicit paths.
    #[arg(long)]
    pub all: bool,
    /// Operate on staged files (index) only.
    /// Incompatible with `--all`, `--discovery=walk`, or explicit paths.
    #[arg(long)]
    pub staged: bool,
    /// File discovery mode.
    #[arg(long, value_enum, default_value_t = Discovery::Vcs)]
    pub discovery: Discovery,
    /// Check explicitly targeted paths even when they match global or
    /// per-tool `paths.exclude`. Requires at least one positional target.
    #[arg(long, requires = "paths")]
    pub force: bool,
    /// Optional files, directories, or glob patterns to check. Directories
    /// are recursive and patterns are resolved relative to the current
    /// directory. When provided, scope flags are not allowed.
    #[arg(value_name = "PATH_OR_PATTERN")]
    pub paths: Vec<PathBuf>,
}

#[derive(Args, Debug, Default)]
pub struct InstallHookArgs {
    /// Overwrite an existing pre-commit hook.
    #[arg(long)]
    pub force: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_requires_an_explicit_target() {
        assert!(Cli::try_parse_from(["kempt", "format", "--force"]).is_err());
        assert!(Cli::try_parse_from(["kempt", "check", "--force"]).is_err());
        assert!(Cli::try_parse_from(["kempt", "format", "--force", "src"]).is_ok());
        assert!(Cli::try_parse_from(["kempt", "check", "--force", "src"]).is_ok());
    }
}
