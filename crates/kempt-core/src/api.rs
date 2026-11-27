// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Typed, quiet execution of kempt commands.
//!
//! Each call has its own working directory and output capture. Execution never
//! changes the process directory or environment. Commands retain the CLI's Git
//! requirements, including explicit-path and filesystem-walk formatting.

use crate::cache::{Cache, Downloader, UreqDownloader};
use crate::config::{Config, CONFIG_FILE};
use crate::formatter_cache;
use crate::git::{GitContext, RealGit};
use crate::process_output::Capture;
use crate::{hook, operations, paths, upgrade};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

pub use crate::operations::{
    CheckContext, FormatOutcome, PartialFormattingOptions as HookOptions, VendorEntry,
    VendorOutcome,
};
pub use crate::process_output::ProcessOutput;
pub use crate::upgrade::{
    Change as VersionChange, UpgradeOutcome, UreqVersionFetcher, VersionFetcher,
};

/// A command and optional configuration-file override.
#[derive(Debug, Clone)]
pub struct CommandRequest {
    /// Relative configuration paths are interpreted from the execution directory.
    pub config: Option<PathBuf>,
    pub command: Command,
}

impl CommandRequest {
    pub fn new(command: Command) -> Self {
        Self {
            config: None,
            command,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    Format(FormatOptions),
    Check(CheckOptions),
    Init(InitOptions),
    InstallHook(InstallHookOptions),
    Hook,
    Update,
    Upgrade(UpgradeOptions),
    Vendor(VendorOptions),
    CacheList,
    CachePrune(PruneOptions),
}

/// File selection. Explicit targets may be files, directories, or glob patterns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FileScope {
    #[default]
    All,
    Staged,
    Touched {
        base: Option<String>,
    },
    Walk,
    Explicit {
        targets: Vec<PathBuf>,
        force: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct FormatOptions {
    pub scope: FileScope,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CheckOptions {
    pub scope: FileScope,
}

#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    pub license_header: bool,
}

#[derive(Debug, Clone, Default)]
pub struct InstallHookOptions {
    pub force: bool,
}

#[derive(Debug, Clone, Default)]
pub struct UpgradeOptions {
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct VendorOptions {
    /// Relative destinations are interpreted from the repository root.
    pub directory: PathBuf,
}

impl Default for VendorOptions {
    fn default() -> Self {
        Self {
            directory: "config/bin".into(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PruneOptions {
    pub all: bool,
}

/// Explicit inputs to an execution. Inject downloaders and version fetchers for
/// tests or custom networking. The working directory must be absolute.
pub struct ExecutionContext<'a> {
    pub working_directory: PathBuf,
    pub cache: Cache,
    pub downloader: &'a dyn Downloader,
    pub version_fetcher: &'a dyn VersionFetcher,
    pub year: u32,
    pub hook_options: HookOptions,
}

impl ExecutionContext<'static> {
    /// Use an explicit cache and the standard HTTP implementations. Experimental
    /// hook options are disabled. Relative cache paths resolve from the working
    /// directory, which must be absolute.
    pub fn new(working_directory: PathBuf, cache: Cache) -> Result<Self> {
        if !working_directory.is_absolute() {
            bail!("execution directory must be absolute");
        }
        let root = absolute_from(&working_directory, cache.root());
        Ok(Self {
            working_directory,
            cache: Cache::new(root),
            downloader: &UreqDownloader,
            version_fetcher: &UreqVersionFetcher,
            year: current_year(),
            hook_options: HookOptions::default(),
        })
    }

    /// Read cache, year, and experimental hook defaults once for this context.
    /// Relative cache overrides are resolved against `working_directory`.
    pub fn from_env(working_directory: PathBuf) -> Result<Self> {
        let mut context = Self::new(working_directory, Cache::new(Cache::default_root()?))?;
        context.hook_options = HookOptions::from_env();
        Ok(context)
    }
}

/// Command data and captured subprocess output. No output is printed by execution.
#[derive(Debug)]
pub struct CommandOutcome {
    pub result: CommandResult,
    pub output: Vec<ProcessOutput>,
}

impl CommandOutcome {
    /// CLI-compatible status. Formatting differences and check diagnostics return 1.
    pub fn exit_code(&self) -> u8 {
        match &self.result {
            CommandResult::Format { report, .. } | CommandResult::Hook { report }
                if report.check_failed =>
            {
                1
            }
            _ => 0,
        }
    }
}

#[derive(Debug)]
pub enum CommandResult {
    Format {
        report: FormatOutcome,
        context: CheckContext,
        check: bool,
    },
    Hook {
        report: FormatOutcome,
    },
    Init {
        created: Vec<PathBuf>,
    },
    InstallHook {
        path: PathBuf,
    },
    Update {
        cache_root: PathBuf,
    },
    Upgrade {
        report: UpgradeOutcome,
        dry_run: bool,
    },
    Vendor {
        report: VendorOutcome,
    },
    CacheList {
        cache_root: PathBuf,
        entries: Vec<crate::cache::CacheEntry>,
    },
    CachePrune {
        removed: Vec<PathBuf>,
    },
}

/// An operational failure, retaining diagnostics and output from earlier steps.
#[derive(Debug)]
pub struct RunError {
    pub source: anyhow::Error,
    pub output: Vec<ProcessOutput>,
    pub diagnostics: Vec<String>,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.source)
    }
}

impl std::error::Error for RunError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[derive(Debug)]
pub(crate) struct HookFailure {
    pub detail: String,
    pub source: anyhow::Error,
}

impl std::fmt::Display for HookFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.source)
    }
}
impl std::error::Error for HookFailure {}

/// Execute a typed request without printing or terminating the process.
pub fn execute(
    request: &CommandRequest,
    context: &ExecutionContext<'_>,
) -> std::result::Result<CommandOutcome, RunError> {
    let capture = Capture::default();
    let result = dispatch(request, context, &capture);
    let output = capture.take();
    match result {
        Ok(result) => Ok(CommandOutcome { result, output }),
        Err(source) => {
            let diagnostics = source
                .downcast_ref::<HookFailure>()
                .map(|error| vec![error.detail.clone()])
                .unwrap_or_default();
            Err(RunError {
                source,
                output,
                diagnostics,
            })
        }
    }
}

fn dispatch(
    request: &CommandRequest,
    context: &ExecutionContext<'_>,
    capture: &Capture,
) -> Result<CommandResult> {
    let cwd = &context.working_directory;
    if !cwd.is_absolute() {
        bail!("execution directory must be absolute");
    }
    let cache = formatter_cache::Cache::new(absolute_from(cwd, context.cache.root()));
    let runtime = operations::Runtime {
        year: context.year,
        capture,
        partial: context.hook_options,
    };
    match &request.command {
        Command::Init(options) => {
            return Ok(CommandResult::Init {
                created: operations::run_init(cwd, options.license_header)?,
            })
        }
        Command::CacheList => {
            return Ok(CommandResult::CacheList {
                cache_root: cache.root().into(),
                entries: cache.list_entries()?,
            })
        }
        Command::CachePrune(PruneOptions { all: true }) => {
            return Ok(CommandResult::CachePrune {
                removed: cache.prune(&[])?,
            })
        }
        _ => {}
    }
    let git = RealGit::discover(cwd)?.with_capture(capture);
    let config_path = request
        .config
        .as_ref()
        .map(|path| absolute_from(cwd, path))
        .unwrap_or_else(|| git.root().join(CONFIG_FILE));
    match &request.command {
        Command::InstallHook(options) => {
            return Ok(CommandResult::InstallHook {
                path: hook::install_pre_commit(&git.root().join(".git"), options.force)?,
            })
        }
        Command::Upgrade(options) => {
            if !config_path.exists() {
                bail!(
                    "no config at {}; run `kempt init` first",
                    config_path.display()
                );
            }
            return Ok(CommandResult::Upgrade {
                report: upgrade::run_upgrade(
                    &config_path,
                    context.version_fetcher,
                    options.dry_run,
                )?,
                dry_run: options.dry_run,
            });
        }
        _ => {}
    }
    let config = if config_path.exists() {
        Config::load(&config_path)?.resolve_catalogs(git.root())?
    } else {
        Config::default()
    };
    match &request.command {
        Command::Format(options) => format(
            &options.scope,
            options.dry_run,
            &config,
            &git,
            &cache,
            context,
            &runtime,
        ),
        Command::Check(options) => format(
            &options.scope,
            true,
            &config,
            &git,
            &cache,
            context,
            &runtime,
        ),
        Command::Hook => Ok(CommandResult::Hook {
            report: operations::run_hook_with_runtime(
                &config,
                &git,
                &cache,
                context.downloader,
                &runtime,
            )?,
        }),
        Command::Update => {
            operations::run_update(&config, git.root(), &cache, context.downloader)?;
            Ok(CommandResult::Update {
                cache_root: cache.root().into(),
            })
        }
        Command::Vendor(options) => Ok(CommandResult::Vendor {
            report: operations::run_vendor(
                &config,
                git.root(),
                &cache,
                context.downloader,
                &options.directory,
            )?,
        }),
        Command::CachePrune(_) => Ok(CommandResult::CachePrune {
            removed: cache.prune(&operations::keep_paths_for_config(
                &config,
                git.root(),
                &cache,
            ))?,
        }),
        _ => unreachable!("commands without configuration are dispatched above"),
    }
}

fn format(
    scope: &FileScope,
    check: bool,
    config: &Config,
    git: &RealGit,
    cache: &formatter_cache::Cache,
    context: &ExecutionContext<'_>,
    runtime: &operations::Runtime<'_>,
) -> Result<CommandResult> {
    let (scope, check_context) = match scope {
        FileScope::All => (paths::Scope::All, CheckContext::All),
        FileScope::Staged => (paths::Scope::Staged, CheckContext::Staged),
        FileScope::Touched { base } => (
            paths::Scope::Touched { base: base.clone() },
            CheckContext::Touched { base: base.clone() },
        ),
        FileScope::Walk => (paths::Scope::Walk, CheckContext::Walk),
        FileScope::Explicit { targets, force } => {
            if targets.is_empty() {
                bail!("explicit scope requires at least one target");
            }
            (
                paths::Scope::Explicit {
                    files: paths::resolve_explicit_targets(
                        targets,
                        &context.working_directory,
                        git.root(),
                    )?,
                    force: *force,
                },
                CheckContext::Explicit,
            )
        }
    };
    let report = operations::run_format_with_runtime(
        config,
        git,
        cache,
        context.downloader,
        scope,
        check,
        runtime,
    )?;
    Ok(CommandResult::Format {
        report,
        context: check_context,
        check,
    })
}

fn absolute_from(directory: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        directory.join(path)
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
    #[test]
    fn current_year_is_reasonable() {
        let year = super::current_year();
        assert!((2026..2100).contains(&year), "got {year}");
    }
}
