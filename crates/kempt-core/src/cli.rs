// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(ValueEnum, Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Discovery {
    /// Use git to enumerate files (`git ls-files`). Default.
    #[default]
    Vcs,
    /// Walk the filesystem from the repo root. Ignore files (`.gitignore`)
    /// are not consulted. Use `[paths]` exclusions to filter results. The
    /// `.git/` directory is always pruned.
    Walk,
}

#[derive(Parser, Debug)]
#[command(
    name = "kempt",
    version,
    about = "Multi-language source formatter (Kotlin, Java, Rust, license headers, whitespace)"
)]
struct Cli {
    /// Path to config file. Defaults to `.kempt.toml` in the repo root.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
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
struct InitArgs {
    /// Also write config/license-header.txt and enable [license-header].
    #[arg(long)]
    pub license_header: bool,
}

#[derive(Args, Debug, Default)]
struct UpgradeArgs {
    /// Show what would change without modifying `.kempt.toml`.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
struct VendorArgs {
    /// Directory to copy formatter artifacts into. Relative paths resolve
    /// against the repo root.
    #[arg(long, default_value = "config/bin")]
    pub dir: PathBuf,
}

#[derive(Subcommand, Debug)]
enum CacheCmd {
    /// List cached formatter artifacts.
    List,
    /// Remove cached artifacts not referenced by the current config.
    Prune(PruneArgs),
}

#[derive(Args, Debug, Default)]
struct PruneArgs {
    /// Remove every cached formatter artifact regardless of config.
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Default)]
struct FormatArgs {
    /// Explicitly target all tracked files. Same as the default behavior;
    /// the flag exists so suggestions can be unambiguous about scope.
    /// Incompatible with `--staged`, `--touched`, `--discovery=walk`, or
    /// explicit paths.
    #[arg(long)]
    pub all: bool,
    /// Operate on staged files (index) only.
    /// Incompatible with `--all`, `--touched`, `--discovery=walk`, or explicit
    /// paths.
    #[arg(long)]
    pub staged: bool,
    /// Operate on files changed on this branch since it diverged from the
    /// default branch, including committed, staged, unstaged, and untracked
    /// files. Use `--base` to override the detected default branch.
    #[arg(long)]
    pub touched: bool,
    /// Git ref to use as the comparison base for `--touched`.
    #[arg(long, value_name = "REF", requires = "touched")]
    pub base: Option<String>,
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
struct CheckArgs {
    /// Explicitly target all tracked files. Same as the default behavior.
    /// Incompatible with `--staged`, `--touched`, `--discovery=walk`, or
    /// explicit paths.
    #[arg(long)]
    pub all: bool,
    /// Operate on staged files (index) only.
    /// Incompatible with `--all`, `--touched`, `--discovery=walk`, or explicit
    /// paths.
    #[arg(long)]
    pub staged: bool,
    /// Operate on files changed on this branch since it diverged from the
    /// default branch, including committed, staged, unstaged, and untracked
    /// files. Use `--base` to override the detected default branch.
    #[arg(long)]
    pub touched: bool,
    /// Git ref to use as the comparison base for `--touched`.
    #[arg(long, value_name = "REF", requires = "touched")]
    pub base: Option<String>,
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
struct InstallHookArgs {
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

    #[test]
    fn base_requires_touched_scope() {
        assert!(Cli::try_parse_from(["kempt", "format", "--base", "origin/main"]).is_err());
        assert!(Cli::try_parse_from(["kempt", "check", "--base", "origin/main"]).is_err());
        assert!(
            Cli::try_parse_from(["kempt", "format", "--touched", "--base", "origin/main"]).is_ok()
        );
        assert!(
            Cli::try_parse_from(["kempt", "check", "--touched", "--base", "origin/main"]).is_ok()
        );
    }
}

/// Result of parsing CLI arguments, including the executable name.
#[derive(Debug)]
pub enum ParseOutcome {
    Command(crate::commands::CommandRequest),
    Display { text: String, version: bool },
}

/// Argument errors use clap's status 2. Invalid scope combinations retain the
/// command's existing status 1 and error text.
#[derive(Debug)]
pub enum ParseError {
    Arguments(clap::Error),
    Request(anyhow::Error),
}

impl ParseError {
    pub fn exit_code(&self) -> i32 {
        match self { Self::Arguments(error) => error.exit_code(), Self::Request(_) => 1 }
    }

    /// Explicitly print this error using the CLI's normal error presentation.
    pub fn print(&self) -> std::io::Result<()> {
        match self {
            Self::Arguments(error) => error.print(),
            Self::Request(error) => {
                use std::io::Write;
                writeln!(std::io::stderr().lock(), "kempt: {error:#}")
            }
        }
    }
}
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Arguments(error) => write!(f, "{error}"), Self::Request(error) => write!(f, "{error:#}") }
    }
}
impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self { Self::Arguments(error) => Some(error), Self::Request(error) => Some(error.as_ref()) }
    }
}

/// Parse without printing or terminating the process. The first argument is
/// the program name. Help and version requests return displayable text.
pub fn parse_from<I, T>(args: I) -> Result<ParseOutcome, ParseError>
where I: IntoIterator<Item = T>, T: Into<std::ffi::OsString> + Clone {
    let parsed = match Cli::try_parse_from(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            let version = error.kind() == clap::error::ErrorKind::DisplayVersion;
            if version || error.kind() == clap::error::ErrorKind::DisplayHelp {
                return Ok(ParseOutcome::Display { text: error.to_string(), version });
            }
            return Err(ParseError::Arguments(error));
        }
    };
    to_request(parsed).map(ParseOutcome::Command).map_err(ParseError::Request)
}

fn to_request(parsed: Cli) -> anyhow::Result<crate::commands::CommandRequest> {
    use crate::commands as api;
    let command = match parsed.command {
        Cmd::Format(args) => api::Command::Format(api::FormatOptions {
            scope: file_scope(args.all, args.staged, args.touched, args.base, args.discovery, args.paths, args.force)?, dry_run: args.dry_run,
        }),
        Cmd::Check(args) => api::Command::Check(api::CheckOptions {
            scope: file_scope(args.all, args.staged, args.touched, args.base, args.discovery, args.paths, args.force)?,
        }),
        Cmd::Init(args) => api::Command::Init(api::InitOptions { license_header: args.license_header }),
        Cmd::InstallHook(args) => api::Command::InstallHook(api::InstallHookOptions { force: args.force }),
        Cmd::Hook => api::Command::Hook,
        Cmd::Update => api::Command::Update,
        Cmd::Upgrade(args) => api::Command::Upgrade(api::UpgradeOptions { dry_run: args.dry_run }),
        Cmd::Vendor(args) => api::Command::Vendor(api::VendorOptions { directory: args.dir }),
        Cmd::Cache(CacheCmd::List) => api::Command::CacheList,
        Cmd::Cache(CacheCmd::Prune(args)) => api::Command::CachePrune(api::PruneOptions { all: args.all }),
    };
    Ok(api::CommandRequest { command, config: parsed.config })
}

fn file_scope(all: bool, staged: bool, touched: bool, base: Option<String>, discovery: Discovery, targets: Vec<PathBuf>, force: bool) -> anyhow::Result<crate::commands::FileScope> {
    use crate::commands::FileScope;
    if !targets.is_empty() && (all || staged || touched || discovery == Discovery::Walk) {
        anyhow::bail!("explicit paths are incompatible with --all, --staged, --touched, and --discovery=walk");
    }
    for (enabled, conflicting, message) in [
        (all, staged, "--all is incompatible with --staged"),
        (all, discovery == Discovery::Walk, "--all is incompatible with --discovery=walk"),
        (all, touched, "--all is incompatible with --touched"),
        (staged, discovery == Discovery::Walk, "--staged is incompatible with --discovery=walk"),
        (staged, touched, "--staged is incompatible with --touched"),
        (touched, discovery == Discovery::Walk, "--touched is incompatible with --discovery=walk"),
    ] { if enabled && conflicting { anyhow::bail!(message); } }
    Ok(if !targets.is_empty() { FileScope::Explicit { targets, force } }
        else if touched { FileScope::Touched { base } }
        else if discovery == Discovery::Walk { FileScope::Walk }
        else if staged { FileScope::Staged }
        else { FileScope::All })
}

/// Render a result to caller-owned writers. Execution itself remains quiet.
pub fn render(outcome: &crate::commands::CommandOutcome, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write) -> std::io::Result<()> {
    use crate::commands::CommandResult;
    render_process_output(&outcome.output, stdout, stderr)?;
    match &outcome.result {
        CommandResult::Format { report, context, check } => {
            if *check { render_check(report, context.clone(), stdout, stderr)?; }
            else if report.changed.is_empty() { writeln!(stdout, "kempt: nothing to do")?; }
            else {
                writeln!(stdout, "kempt: changed {} file(s)", report.changed.len())?;
                for path in &report.changed { writeln!(stdout, "  {}", path.display())?; }
            }
        }
        CommandResult::Hook { report } => {
            if report.check_failed { render_check(report, crate::commands::CheckContext::Hook, stdout, stderr)?; }
            else if !report.changed.is_empty() { writeln!(stdout, "kempt: re-staged {} file(s)", report.changed.len())?; }
        }
        CommandResult::Init { created } => {
            if created.is_empty() { writeln!(stdout, "kempt: nothing to do (config already exists)")?; }
            else { for path in created { writeln!(stdout, "created {}", path.display())?; } }
        }
        CommandResult::InstallHook { path } => writeln!(stdout, "installed {}", path.display())?,
        CommandResult::Update { cache_root } => writeln!(stdout, "kempt: cache up to date at {}", cache_root.display())?,
        CommandResult::CacheList { cache_root, entries } => {
            if entries.is_empty() { writeln!(stdout, "kempt: cache is empty ({})", cache_root.display())?; }
            else {
                writeln!(stdout, "{}:", cache_root.display())?;
                for entry in entries {
                    let name = entry.path.file_name().and_then(|name| name.to_str()).unwrap_or("?");
                    writeln!(stdout, "  {name:<32} {}", human_bytes(entry.size))?;
                }
            }
        }
        CommandResult::CachePrune { removed } => {
            if removed.is_empty() { writeln!(stdout, "kempt: nothing to prune")?; }
            else {
                writeln!(stdout, "removed {} file(s):", removed.len())?;
                for path in removed { writeln!(stdout, "  {}", path.display())?; }
            }
        }
        CommandResult::Vendor { report } => {
            if report.entries.is_empty() && report.skipped.is_empty() {
                writeln!(stdout, "kempt: nothing to vendor (no downloadable formatters in config)")?;
            }
            for skipped in &report.skipped { writeln!(stdout, "kempt: [{skipped}] already uses `path = ...`, skipping")?; }
            if !report.entries.is_empty() {
                writeln!(stdout, "vendored {} artifact(s):", report.entries.len())?;
                for entry in &report.entries { writeln!(stdout, "  {} {} -> {}", entry.tool, entry.version, entry.dest.display())?; }
                writeln!(stdout, "\nupdate .kempt.toml to point at the vendored copies:")?;
                for entry in &report.entries { writeln!(stdout, "\n  [{}]\n  path = \"{}\"", entry.tool, entry.config_value.display())?; }
            }
        }
        CommandResult::Upgrade { report, dry_run } => {
            for skipped in &report.skipped { writeln!(stdout, "kempt: {skipped}")?; }
            for tool in &report.already_current { writeln!(stdout, "kempt: [{tool}] already on the latest version")?; }
            if report.changes.is_empty() { writeln!(stdout, "kempt: nothing to upgrade")?; }
            else {
                let verb = if *dry_run { "would update" } else { "updated" };
                writeln!(stdout, "kempt: {verb} {} tool(s):", report.changes.len())?;
                for change in &report.changes { writeln!(stdout, "  [{}] {} -> {}", change.tool, change.from, change.to)?; }
                if *dry_run { writeln!(stdout, "\nRe-run without --dry-run to apply.")?; }
                else { writeln!(stdout, "\nRun `kempt update` to download the new version(s) into the cache.")?; }
            }
        }
    }
    Ok(())
}

/// Render an operational failure, including output produced before it failed.
pub fn render_error(error: &crate::commands::RunError, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write) -> std::io::Result<()> {
    render_process_output(&error.output, stdout, stderr)?;
    for diagnostic in &error.diagnostics { write!(stderr, "{diagnostic}")?; }
    writeln!(stderr, "kempt: {error}")
}

fn render_process_output(output: &[crate::commands::ProcessOutput], stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write) -> std::io::Result<()> {
    for process in output {
        if process.display_stdout { stdout.write_all(&process.stdout)?; }
        if process.display_stderr { stderr.write_all(&process.stderr)?; }
    }
    Ok(())
}

fn render_check(report: &crate::commands::FormatOutcome, context: crate::commands::CheckContext, stdout: &mut dyn std::io::Write, stderr: &mut dyn std::io::Write) -> std::io::Result<()> {
    for path in &report.changed { writeln!(stdout, "{}", path.display())?; }
    if !report.parse_errors.is_empty() { writeln!(stderr, "{}", report.parse_errors)?; }
    let summary = crate::operations::render_check_summary(report, context);
    if !summary.is_empty() {
        writeln!(stderr)?;
        for line in summary { writeln!(stderr, "{line}")?; }
    }
    Ok(())
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} {}", UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}


#[cfg(test)]
mod rendering_tests {
    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(super::human_bytes(0), "0 B");
        assert_eq!(super::human_bytes(512), "512 B");
        assert_eq!(super::human_bytes(2048), "2.0 KiB");
        assert_eq!(super::human_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(super::human_bytes(3u64 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
