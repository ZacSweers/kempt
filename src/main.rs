mod cache;
mod cli;
mod commands;
mod config;
mod formatters;
mod git;
mod hook;
mod license;
mod paths;
mod pipeline;
mod upgrade;
mod whitespace;

use anyhow::{Context, Result};
use cache::{Cache, UreqDownloader};
use clap::Parser;
use cli::{CacheCmd, Cli, Cmd, Discovery};
use config::Config;
use git::{GitContext, RealGit};
use paths::Scope;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    run().unwrap_or_else(|e| {
        eprintln!("kempt: {e:#}");
        ExitCode::FAILURE
    })
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    match cli.command {
        Cmd::Init(args) => {
            let cwd = std::env::current_dir().context("read current dir")?;
            let written = commands::run_init(&cwd, args.license_header)?;
            if written.is_empty() {
                println!("kempt: nothing to do (config already exists)");
            } else {
                for p in written {
                    println!("created {}", p.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::InstallHook(args) => {
            let cwd = std::env::current_dir().context("read current dir")?;
            let git = RealGit::discover(&cwd)?;
            let path = hook::install_pre_commit(&git.root().join(".git"), args.force)?;
            println!("installed {}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Format(args) => format_or_check(
            cli.config,
            args.all,
            args.staged,
            args.discovery,
            args.paths,
            args.dry_run,
        ),
        Cmd::Check(args) => format_or_check(
            cli.config,
            args.all,
            args.staged,
            args.discovery,
            args.paths,
            true,
        ),
        Cmd::Hook => run_hook_subcommand(cli.config),
        Cmd::Update => run_update_subcommand(cli.config),
        Cmd::Upgrade(args) => run_upgrade_subcommand(cli.config, args.dry_run),
        Cmd::Vendor(args) => run_vendor_subcommand(cli.config, args.dir),
        Cmd::Cache(c) => run_cache_subcommand(cli.config, c),
    }
}

fn run_vendor_subcommand(config_path: Option<PathBuf>, dir: PathBuf) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("read current dir")?;
    let git = RealGit::discover(&cwd)?;
    let config = load_config(&config_path, git.root())?;
    let cache = Cache::new(Cache::default_root()?);
    let dl = UreqDownloader;

    let outcome = commands::run_vendor(&config, git.root(), &cache, &dl, &dir)?;

    if outcome.entries.is_empty() && outcome.skipped.is_empty() {
        println!("kempt: nothing to vendor (no [ktfmt] or [gjf] in config)");
        return Ok(ExitCode::SUCCESS);
    }

    for skipped in &outcome.skipped {
        println!("kempt: [{skipped}] already uses `path = ...`, skipping");
    }
    if outcome.entries.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }

    println!("vendored {} artifact(s):", outcome.entries.len());
    for e in &outcome.entries {
        println!("  {} {} -> {}", e.tool, e.version, e.dest.display());
    }
    println!();
    println!("update .kempt.toml to point at the vendored copies:");
    for e in &outcome.entries {
        println!();
        println!("  [{}]", e.tool);
        println!("  path = \"{}\"", e.config_value.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn run_cache_subcommand(config_path: Option<PathBuf>, cmd: CacheCmd) -> Result<ExitCode> {
    let cache = Cache::new(Cache::default_root()?);
    match cmd {
        CacheCmd::List => {
            let entries = cache.list_entries()?;
            if entries.is_empty() {
                println!("kempt: cache is empty ({})", cache.root().display());
                return Ok(ExitCode::SUCCESS);
            }
            println!("{}:", cache.root().display());
            for e in entries {
                let name = e.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                println!("  {name:<32} {}", human_bytes(e.size));
            }
            Ok(ExitCode::SUCCESS)
        }
        CacheCmd::Prune(args) => {
            let keep = if args.all {
                Vec::new()
            } else {
                let cwd = std::env::current_dir().context("read current dir")?;
                let git = RealGit::discover(&cwd)?;
                let config = load_config(&config_path, git.root())?;
                commands::keep_paths_for_config(&config, git.root(), &cache)
            };
            let removed = cache.prune(&keep)?;
            if removed.is_empty() {
                println!("kempt: nothing to prune");
            } else {
                println!("removed {} file(s):", removed.len());
                for p in removed {
                    println!("  {}", p.display());
                }
            }
            Ok(ExitCode::SUCCESS)
        }
    }
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

fn format_or_check(
    config_path: Option<PathBuf>,
    all: bool,
    staged: bool,
    discovery: Discovery,
    explicit_paths: Vec<PathBuf>,
    check: bool,
) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("read current dir")?;
    let git = RealGit::discover(&cwd)?;
    let config = load_config(&config_path, git.root())?;
    let cache = Cache::new(Cache::default_root()?);
    let dl = UreqDownloader;
    let has_explicit = !explicit_paths.is_empty();
    if has_explicit && (all || staged || discovery == Discovery::Walk) {
        anyhow::bail!("explicit paths are incompatible with --all, --staged, and --discovery=walk");
    }
    if all && staged {
        anyhow::bail!("--all is incompatible with --staged");
    }
    if all && discovery == Discovery::Walk {
        anyhow::bail!("--all is incompatible with --discovery=walk");
    }
    if staged && discovery == Discovery::Walk {
        anyhow::bail!("--staged is incompatible with --discovery=walk");
    }

    let scope = if has_explicit {
        Scope::Explicit(resolve_explicit_paths(&explicit_paths, &cwd, git.root())?)
    } else {
        match (discovery, staged) {
            (Discovery::Walk, _) => Scope::Walk,
            (Discovery::Vcs, true) => Scope::Staged,
            (Discovery::Vcs, false) => Scope::All,
        }
    };
    let _ = all; // resolves to the default scope; flag exists for explicit symmetry

    let ctx = check_context_for_scope(&scope);
    let out = commands::run_format(&config, &git, &cache, &dl, scope, check, current_year())?;

    print_outcome(&out, ctx, check);
    if out.check_failed {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

/// Resolve user-supplied paths to repo-relative form. Paths are interpreted
/// against `cwd` first, then made relative to `repo_root`. Errors out if any
/// path falls outside the repo.
fn resolve_explicit_paths(
    paths: &[PathBuf],
    cwd: &std::path::Path,
    repo_root: &std::path::Path,
) -> Result<Vec<PathBuf>> {
    let canonical_root = repo_root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", repo_root.display()))?;
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            cwd.join(p)
        };
        let canonical = abs
            .canonicalize()
            .with_context(|| format!("path not found: {}", p.display()))?;
        let rel = canonical
            .strip_prefix(&canonical_root)
            .with_context(|| format!("path {} is outside the repo root", p.display()))?
            .to_path_buf();
        out.push(rel);
    }
    Ok(out)
}

fn run_hook_subcommand(config_path: Option<PathBuf>) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("read current dir")?;
    let git = RealGit::discover(&cwd)?;
    let config = load_config(&config_path, git.root())?;
    let cache = Cache::new(Cache::default_root()?);
    let dl = UreqDownloader;

    let out = commands::run_hook(&config, &git, &cache, &dl, current_year())?;
    if out.check_failed {
        print_check_outcome(&out, commands::CheckContext::Hook);
        return Ok(ExitCode::from(1));
    }
    if !out.changed.is_empty() {
        println!("kempt: re-staged {} file(s)", out.changed.len());
    }
    Ok(ExitCode::SUCCESS)
}

fn run_update_subcommand(config_path: Option<PathBuf>) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("read current dir")?;
    let git = RealGit::discover(&cwd)?;
    let config = load_config(&config_path, git.root())?;
    let cache = Cache::new(Cache::default_root()?);
    let dl = UreqDownloader;
    commands::run_update(&config, git.root(), &cache, &dl)?;
    println!("kempt: cache up to date at {}", cache.root().display());
    Ok(ExitCode::SUCCESS)
}

fn run_upgrade_subcommand(config_path: Option<PathBuf>, dry_run: bool) -> Result<ExitCode> {
    let cwd = std::env::current_dir().context("read current dir")?;
    let git = RealGit::discover(&cwd)?;
    let path = config_path.unwrap_or_else(|| git.root().join(config::CONFIG_FILE));
    if !path.exists() {
        anyhow::bail!("no config at {}; run `kempt init` first", path.display());
    }
    let fetcher = upgrade::UreqVersionFetcher;
    let outcome = upgrade::run_upgrade(&path, &fetcher, dry_run)?;

    for skipped in &outcome.skipped {
        println!("kempt: {skipped}");
    }
    for tool in &outcome.already_current {
        println!("kempt: [{tool}] already on the latest version");
    }
    if outcome.changes.is_empty() {
        println!("kempt: nothing to upgrade");
        return Ok(ExitCode::SUCCESS);
    }
    let verb = if dry_run { "would update" } else { "updated" };
    println!("kempt: {verb} {} tool(s):", outcome.changes.len());
    for c in &outcome.changes {
        println!("  [{}] {} -> {}", c.tool, c.from, c.to);
    }
    if dry_run {
        println!();
        println!("Re-run without --dry-run to apply.");
    } else {
        println!();
        println!("Run `kempt update` to download the new version(s) into the cache.");
    }
    Ok(ExitCode::SUCCESS)
}

fn load_config(override_path: &Option<PathBuf>, repo_root: &std::path::Path) -> Result<Config> {
    let path = override_path
        .clone()
        .unwrap_or_else(|| repo_root.join(config::CONFIG_FILE));
    if !path.exists() {
        return Ok(Config::default());
    }
    Config::load(&path)?.resolve_catalogs(repo_root)
}

fn print_outcome(out: &commands::FormatOutcome, ctx: commands::CheckContext, check: bool) {
    if !check {
        if out.changed.is_empty() {
            println!("kempt: nothing to do");
        } else {
            println!("kempt: changed {} file(s)", out.changed.len());
            for p in &out.changed {
                println!("  {}", p.display());
            }
        }
        return;
    }
    print_check_outcome(out, ctx);
}

fn print_check_outcome(out: &commands::FormatOutcome, ctx: commands::CheckContext) {
    for p in &out.changed {
        println!("{}", p.display());
    }
    if !out.parse_errors.is_empty() {
        eprintln!("{}", out.parse_errors);
    }
    let summary = commands::render_check_summary(out, ctx);
    if !summary.is_empty() {
        eprintln!();
        for line in summary {
            eprintln!("{line}");
        }
    }
}

fn check_context_for_scope(scope: &Scope) -> commands::CheckContext {
    match scope {
        Scope::All => commands::CheckContext::All,
        Scope::Staged => commands::CheckContext::Staged,
        Scope::Walk => commands::CheckContext::Walk,
        Scope::Explicit(_) => commands::CheckContext::Explicit,
    }
}

fn current_year() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since 1970-01-01.
    let days = secs / 86_400;
    // Convert days to year via the civil-from-days algorithm (Howard Hinnant).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let year_offset = if mp >= 10 { 1 } else { 0 };
    (y + year_offset) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_year_is_reasonable() {
        let y = current_year();
        assert!((2026..2100).contains(&y), "got {y}");
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(human_bytes(3u64 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
