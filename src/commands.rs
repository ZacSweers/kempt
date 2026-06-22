//! Subcommand implementations. Glue between the lower-level modules.

use crate::cache::{Cache, Downloader, GjfFlavor};
use crate::config::{Config, Gjf, HookMode, NativeMode, ToolSource};
use crate::formatters;
use crate::git::GitContext;
use crate::hook::{self, StagingCheck};
use crate::license::SourceKind;
use crate::paths::{self, Scope};
use crate::pipeline::{self, Headers, PipelineReport};
use anyhow::{anyhow, Context, Result};
use globset::GlobSet;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const EXPERIMENTAL_PARTIAL_GJF_ENV: &str = "KEMPT_EXPERIMENTAL_PARTIAL_GJF";

/// Outcome of a format/check run.
#[derive(Debug, Default)]
pub struct FormatOutcome {
    /// Files that were (or would be) modified by any kempt step.
    pub changed: BTreeSet<PathBuf>,
    /// True when running in check mode and at least one change is needed.
    pub check_failed: bool,
    /// Filtered stderr from ktfmt/gjf (typically parse errors). Only
    /// populated in check mode.
    pub parse_errors: String,
}

impl FormatOutcome {
    /// True if the formatter reported syntax/parse errors (something kempt
    /// can't fix automatically).
    pub fn has_parse_errors(&self) -> bool {
        !self.parse_errors.is_empty()
    }
}

/// Run the full format pipeline (in-process steps + ktfmt + gjf).
///
/// `check` selects dry-run mode: nothing is written; non-zero exit indicates
/// changes are needed.
pub fn run_format(
    config: &Config,
    git: &dyn GitContext,
    cache: &Cache,
    downloader: &dyn Downloader,
    scope: Scope,
    check: bool,
    year: u32,
) -> Result<FormatOutcome> {
    let candidates = collect_candidates(git, &scope, config)?;
    if candidates.is_empty() {
        return Ok(FormatOutcome::default());
    }
    let mut outcome = apply_pipeline(config, git.root(), &candidates, check, year)?;
    apply_jvm_formatters(
        config,
        cache,
        downloader,
        git.root(),
        &candidates,
        check,
        &mut outcome,
    )?;
    apply_rustfmt(config, git.root(), &candidates, check, &mut outcome)?;
    Ok(outcome)
}

/// Run as the pre-commit hook. Mirrors `run_format(scope=Staged)` but with
/// partial-staging safety check + re-staging after format.
pub fn run_hook(
    config: &Config,
    git: &dyn GitContext,
    cache: &Cache,
    downloader: &dyn Downloader,
    year: u32,
) -> Result<FormatOutcome> {
    run_hook_inner(
        config,
        git,
        cache,
        downloader,
        year,
        experimental_partial_gjf_enabled(),
    )
}

fn run_hook_inner(
    config: &Config,
    git: &dyn GitContext,
    cache: &Cache,
    downloader: &dyn Downloader,
    year: u32,
    allow_partial_gjf: bool,
) -> Result<FormatOutcome> {
    let candidates = collect_candidates(git, &Scope::Staged, config)?;
    if candidates.is_empty() {
        return Ok(FormatOutcome::default());
    }
    let check_mode = matches!(config.hook.mode, HookMode::Check);
    let mut normal_candidates = candidates.clone();
    let mut partial_gjf_files = Vec::new();

    if !check_mode {
        match hook::check_partial_staging(git, &candidates)? {
            StagingCheck::Safe => {}
            StagingCheck::PartialStage { files } => {
                if allow_partial_gjf {
                    partial_gjf_files = partial_gjf_candidates(config, git.root(), &files)?;
                    let handled: BTreeSet<PathBuf> = partial_gjf_files.iter().cloned().collect();
                    let unhandled: Vec<PathBuf> = files
                        .iter()
                        .filter(|p| !handled.contains(*p))
                        .cloned()
                        .collect();
                    if unhandled.is_empty() {
                        normal_candidates.retain(|p| !handled.contains(p));
                    } else {
                        eprint!("{}", hook::format_partial_stage_error(&unhandled));
                        return Err(anyhow!(
                            "partial staging detected; {EXPERIMENTAL_PARTIAL_GJF_ENV} only supports GJF-managed Java files"
                        ));
                    }
                } else {
                    eprint!("{}", hook::format_partial_stage_error(&files));
                    return Err(anyhow!("partial staging detected"));
                }
            }
        }
    }

    let mut outcome = apply_pipeline(config, git.root(), &normal_candidates, check_mode, year)?;
    apply_jvm_formatters(
        config,
        cache,
        downloader,
        git.root(),
        &normal_candidates,
        check_mode,
        &mut outcome,
    )?;
    apply_rustfmt(
        config,
        git.root(),
        &normal_candidates,
        check_mode,
        &mut outcome,
    )?;
    let partial_gjf_changed = if !check_mode && !partial_gjf_files.is_empty() {
        apply_partial_gjf_to_index(config, git, cache, downloader, &partial_gjf_files)?
    } else {
        BTreeSet::new()
    };
    outcome.changed.extend(partial_gjf_changed.iter().cloned());

    if !check_mode && !outcome.changed.is_empty() {
        let to_add: Vec<PathBuf> = outcome
            .changed
            .iter()
            .filter(|p| !partial_gjf_changed.contains(*p))
            .cloned()
            .collect();
        git.add(&to_add).context("git add post-format")?;
    }
    Ok(outcome)
}

fn experimental_partial_gjf_enabled() -> bool {
    std::env::var(EXPERIMENTAL_PARTIAL_GJF_ENV)
        .map(|v| {
            matches!(
                v.as_str(),
                "1" | "true" | "TRUE" | "yes" | "YES" | "on" | "ON"
            )
        })
        .unwrap_or(false)
}

/// Write a starter config + license header template to `target_dir`. The
/// config is tailored to what kempt finds in the repo: `[ktfmt]` is included
/// only when `.kt`/`.kts` files exist, `[gjf]` only when `.java` files
/// exist, and `[rustfmt]` only when `.rs` files exist. An empty repo gets all
/// formatter sections.
/// Idempotent.
pub fn run_init(target_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let cfg = target_dir.join(crate::config::CONFIG_FILE);
    if !cfg.exists() {
        let langs = detect_languages(target_dir);
        let body = build_starter_config(langs);
        std::fs::write(&cfg, body).with_context(|| format!("write {}", cfg.display()))?;
        written.push(cfg);
    }
    let header_dir = target_dir.join("config");
    let header = header_dir.join("license-header.txt");
    if !header.exists() {
        std::fs::create_dir_all(&header_dir)
            .with_context(|| format!("create {}", header_dir.display()))?;
        std::fs::write(&header, STARTER_HEADER)
            .with_context(|| format!("write {}", header.display()))?;
        written.push(header);
    }
    Ok(written)
}

/// Languages kempt found in the repo at init time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DetectedLanguages {
    pub kotlin: bool,
    pub java: bool,
    pub rust: bool,
}

impl DetectedLanguages {
    fn complete(self) -> bool {
        self.kotlin && self.java && self.rust
    }
}

/// Walk `target_dir` looking for `.kt`/`.kts`, `.java`, and `.rs` files.
/// Skips `.git/`, `build/`, `target/`, and `node_modules/`. Stops scanning
/// once every language has been seen.
pub fn detect_languages(target_dir: &Path) -> DetectedLanguages {
    let mut found = DetectedLanguages::default();
    let walker = walkdir::WalkDir::new(target_dir)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            let name = e.file_name();
            !matches!(
                name.to_str(),
                Some(".git" | "build" | "target" | "node_modules")
            )
        });
    for entry in walker.flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        match entry.path().extension().and_then(|e| e.to_str()) {
            Some("kt" | "kts") => found.kotlin = true,
            Some("java") => found.java = true,
            Some("rs") => found.rust = true,
            _ => {}
        }
        if found.complete() {
            break;
        }
    }
    found
}

fn build_starter_config(langs: DetectedLanguages) -> String {
    // No detected languages means there is no useful signal, so emit the full
    // starter config.
    let neither = !langs.kotlin && !langs.java && !langs.rust;
    let want_ktfmt = langs.kotlin || neither;
    let want_gjf = langs.java || neither;
    let want_rustfmt = langs.rust || neither;

    let mut out = String::from(
        "# kempt configuration: https://github.com/ZacSweers/kempt\n# Run `kempt --help` to see all options.\n\n",
    );
    if want_ktfmt {
        out.push_str(&format!(
            "[ktfmt]\nversion = \"{STARTER_KTFMT_VERSION}\"\n\n"
        ));
    }
    if want_gjf {
        out.push_str(&format!("[gjf]\nversion = \"{STARTER_GJF_VERSION}\"\n\n"));
    }
    if want_rustfmt {
        out.push_str("[rustfmt]\n\n");
    }
    out.push_str("[license-header]\nfile = \"config/license-header.txt\"\n\n");
    out.push_str("[hook]\nmode = \"format\"   # format | check\n");
    out
}

/// Cache paths the current config wants to keep. Tools using `path = ...`
/// contribute nothing here since their binaries don't live in the cache.
pub fn keep_paths_for_config(config: &Config, repo_root: &Path, cache: &Cache) -> Vec<PathBuf> {
    let mut keep = Vec::new();
    if let Some(kt) = &config.ktfmt {
        if let ToolSource::Cached(v) = kt.source(repo_root) {
            keep.push(cache.ktfmt_path(&v));
        }
    }
    if let Some(g) = &config.gjf {
        if let ToolSource::Cached(v) = g.source(repo_root) {
            // For native modes that fall back to jar, we don't know the final
            // flavor without actually running resolve. The keep-set adds both
            // candidate paths; only one will exist on disk and prune ignores
            // missing entries.
            keep.push(cache.gjf_jar_path(&v));
            if g.native != NativeMode::Never {
                if let Some(asset) = crate::cache::current_native_asset() {
                    if crate::cache::native_supported_for_version(&v, &asset) {
                        keep.push(cache.gjf_native_path(&v, &asset));
                    }
                }
            }
        }
    }
    keep
}

/// Pre-fetch formatter artifacts per config. Tools using `path = ...` are
/// skipped because their binary is already in the repo.
pub fn run_update(
    config: &Config,
    repo_root: &Path,
    cache: &Cache,
    downloader: &dyn Downloader,
) -> Result<()> {
    if let Some(kt) = &config.ktfmt {
        if let ToolSource::Cached(v) = kt.source(repo_root) {
            cache.ensure_ktfmt(&v, downloader)?;
        }
    }
    if let Some(g) = &config.gjf {
        if let ToolSource::Cached(v) = g.source(repo_root) {
            ensure_gjf_artifact(&v, g, cache, downloader)?;
        }
    }
    Ok(())
}

/// Resolve `gjf` to a concrete artifact on disk, downloading if needed.
fn ensure_gjf_artifact(
    version: &str,
    g: &Gjf,
    cache: &Cache,
    downloader: &dyn Downloader,
) -> Result<formatters::Invoker> {
    let prefer = !matches!(g.native, NativeMode::Never);
    let require = matches!(g.native, NativeMode::Always);
    match crate::cache::resolve_gjf_flavor(version, prefer, require)? {
        GjfFlavor::Jar => {
            let path = cache.ensure_gjf_jar(version, downloader)?;
            Ok(formatters::Invoker::Jar(path))
        }
        GjfFlavor::Native(asset) => {
            let path = cache.ensure_gjf_native(version, &asset, downloader)?;
            Ok(formatters::Invoker::Native(path))
        }
    }
}

/// Resolve a [`Gjf`] config to an [`formatters::Invoker`] (either a jar or
/// native binary), downloading from the cache as needed and respecting an
/// in-repo `path = ...` if set.
fn resolve_gjf_invoker(
    g: &Gjf,
    repo_root: &Path,
    cache: &Cache,
    downloader: &dyn Downloader,
) -> Result<formatters::Invoker> {
    match g.source(repo_root) {
        ToolSource::Cached(version) => ensure_gjf_artifact(&version, g, cache, downloader),
        ToolSource::Local(path) => {
            if !path.exists() {
                anyhow::bail!("gjf binary not found: {}", path.display());
            }
            // Detect by file extension: `.jar` runs via java, anything else
            // is treated as a native binary.
            let is_jar = path.extension().and_then(|e| e.to_str()) == Some("jar");
            Ok(if is_jar {
                formatters::Invoker::Jar(path)
            } else {
                formatters::Invoker::Native(path)
            })
        }
    }
}

/// Outcome of `kempt vendor`. `entries` lists newly-copied artifacts;
/// `skipped` names tools that were already vendored (using `path = ...`).
#[derive(Debug, Default)]
pub struct VendorOutcome {
    pub entries: Vec<VendorEntry>,
    pub skipped: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub struct VendorEntry {
    pub tool: &'static str,
    pub version: String,
    /// Path written to disk (absolute).
    pub dest: PathBuf,
    /// Path relative to repo root, suitable for the `path = ...` config field.
    pub config_value: PathBuf,
}

/// Download (if needed) and copy formatter artifacts into `target_dir`. The
/// directory is interpreted relative to `repo_root`. Tools already using
/// `path = ...` are skipped.
pub fn run_vendor(
    config: &Config,
    repo_root: &Path,
    cache: &Cache,
    downloader: &dyn Downloader,
    target_dir: &Path,
) -> Result<VendorOutcome> {
    let abs_target = if target_dir.is_absolute() {
        target_dir.to_path_buf()
    } else {
        repo_root.join(target_dir)
    };
    std::fs::create_dir_all(&abs_target)
        .with_context(|| format!("create {}", abs_target.display()))?;

    let mut outcome = VendorOutcome::default();

    if let Some(kt) = &config.ktfmt {
        match kt.source(repo_root) {
            ToolSource::Cached(v) => {
                let src = cache.ensure_ktfmt(&v, downloader)?;
                let entry = copy_into("ktfmt", &v, &src, &abs_target, target_dir)?;
                outcome.entries.push(entry);
            }
            ToolSource::Local(_) => outcome.skipped.push("ktfmt"),
        }
    }
    if let Some(g) = &config.gjf {
        match g.source(repo_root) {
            ToolSource::Cached(v) => {
                // Vendor whichever artifact this config resolves to (jar or
                // native). Filename in the cache already encodes the flavor.
                let invoker = ensure_gjf_artifact(&v, g, cache, downloader)?;
                let src = match &invoker {
                    formatters::Invoker::Jar(p) | formatters::Invoker::Native(p) => p.clone(),
                };
                let entry = copy_into("gjf", &v, &src, &abs_target, target_dir)?;
                outcome.entries.push(entry);
            }
            ToolSource::Local(_) => outcome.skipped.push("gjf"),
        }
    }

    Ok(outcome)
}

fn copy_into(
    tool: &'static str,
    version: &str,
    src: &Path,
    abs_target: &Path,
    rel_target: &Path,
) -> Result<VendorEntry> {
    let filename = src
        .file_name()
        .ok_or_else(|| anyhow!("cache artifact has no file name: {}", src.display()))?;
    let dest = abs_target.join(filename);
    std::fs::copy(src, &dest)
        .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;
    let config_value = if rel_target.is_absolute() {
        dest.clone()
    } else {
        rel_target.join(filename)
    };
    Ok(VendorEntry {
        tool,
        version: version.to_string(),
        dest,
        config_value,
    })
}

/// Versions baked into the starter config emitted by `kempt init`.
///
/// These get out of date as upstream releases happen. The
/// `.github/workflows/bump-starter-versions.yml` workflow scrapes the latest
/// releases weekly and opens a PR bumping these constants. Format must stay
/// `pub const NAME: &str = "x.y.z";` exactly so the workflow's regex hits.
pub const STARTER_KTFMT_VERSION: &str = "0.63";
pub const STARTER_GJF_VERSION: &str = "1.35.0";

const STARTER_HEADER: &str =
    "// Copyright (C) ${YEAR} <author>\n// SPDX-License-Identifier: Apache-2.0\n";

/// Compute the candidate file set: universe (per scope) minus the global
/// `[paths].exclude`. Per-tool include/exclude is applied later, at use
/// site.
fn collect_candidates(
    git: &dyn GitContext,
    scope: &Scope,
    config: &Config,
) -> Result<Vec<PathBuf>> {
    let universe = paths::collect_universe(git, scope.clone())?;
    if matches!(scope, Scope::Explicit(_)) {
        // Explicit paths bypass all globsets, including the global one.
        return Ok(universe);
    }
    let global_exclude_globs = config.paths.exclude.resolve(git.root())?;
    let global_exclude =
        paths::build_globset(&global_exclude_globs).context("invalid [paths].exclude glob")?;
    Ok(paths::apply_global_excludes(universe, &global_exclude))
}

/// Per-tool globsets resolved from config, used to decide which files each
/// tool processes.
struct ToolScopes {
    ktfmt: Option<(GlobSet, GlobSet)>,
    gjf: Option<(GlobSet, GlobSet)>,
    rustfmt: Option<(GlobSet, GlobSet)>,
    whitespace: (GlobSet, GlobSet),
}

impl ToolScopes {
    fn build(config: &Config, repo_root: &Path) -> Result<Self> {
        let ktfmt = match &config.ktfmt {
            Some(kt) => {
                let rp = kt.resolve_paths(repo_root)?;
                Some(paths::tool_globset(&rp)?)
            }
            None => None,
        };
        let gjf = match &config.gjf {
            Some(g) => {
                let rp = g.resolve_paths(repo_root)?;
                Some(paths::tool_globset(&rp)?)
            }
            None => None,
        };
        let rustfmt = match &config.rustfmt {
            Some(r) => {
                let rp = r.resolve_paths(repo_root)?;
                Some(paths::tool_globset(&rp)?)
            }
            None => None,
        };
        let whitespace = paths::tool_globset(&config.whitespace.resolve_paths(repo_root)?)?;
        Ok(Self {
            ktfmt,
            gjf,
            rustfmt,
            whitespace,
        })
    }

    fn matches_ktfmt(&self, path: &Path) -> bool {
        match &self.ktfmt {
            Some((inc, exc)) => inc.is_match(path) && !exc.is_match(path),
            None => false,
        }
    }

    fn matches_gjf(&self, path: &Path) -> bool {
        match &self.gjf {
            Some((inc, exc)) => inc.is_match(path) && !exc.is_match(path),
            None => false,
        }
    }

    fn matches_rustfmt(&self, path: &Path) -> bool {
        match &self.rustfmt {
            Some((inc, exc)) => inc.is_match(path) && !exc.is_match(path),
            None => false,
        }
    }

    fn matches_whitespace(&self, path: &Path) -> bool {
        let (inc, exc) = &self.whitespace;
        inc.is_match(path) && !exc.is_match(path)
    }
}

fn apply_pipeline(
    config: &Config,
    repo_root: &Path,
    files: &[PathBuf],
    check: bool,
    year: u32,
) -> Result<FormatOutcome> {
    let headers = Headers::build(config, repo_root, year)?;
    let ws_opts = crate::whitespace::Options::from(&config.whitespace);
    let scopes = ToolScopes::build(config, repo_root)?;
    let mut report = PipelineReport::default();

    for rel in files {
        let abs = repo_root.join(rel);
        let Some(kind) = SourceKind::from_path(rel) else {
            continue;
        };

        // License-header insertion is determined by file kind (extension)
        // plus the per-tool excludes list. Tool path scope (e.g. ktfmt's
        // include/exclude) is intentionally *not* gating here: headers and
        // formatter routing are separate concerns. A user can configure a
        // global `[license-header]` without configuring `[ktfmt]` and
        // still get headers in their kt files.
        let header_arg = headers.for_kind(kind).and_then(|h| {
            if h.is_excluded(rel) {
                None
            } else {
                Some((h.rendered.as_str(), h.marker.as_str()))
            }
        });

        // Whitespace passes only run if the file is in the whitespace tool's
        // scope.
        let effective_ws = if scopes.matches_whitespace(rel) {
            ws_opts
        } else {
            crate::whitespace::Options::default()
        };

        // Skip files that wouldn't be touched by any in-process step.
        if header_arg.is_none() && !effective_ws.strip_trailing && !effective_ws.final_newline {
            continue;
        }

        let content =
            std::fs::read_to_string(&abs).with_context(|| format!("read {}", abs.display()))?;
        let (new_content, file_report) =
            pipeline::process_content(&content, kind, header_arg, effective_ws);
        if file_report.changed() {
            report.record(rel, &file_report);
            if !check {
                std::fs::write(&abs, new_content)
                    .with_context(|| format!("write {}", abs.display()))?;
            }
        }
    }

    let mut outcome = FormatOutcome::default();
    for p in report.changed {
        outcome.changed.insert(p);
    }
    if check && !outcome.changed.is_empty() {
        outcome.check_failed = true;
    }
    Ok(outcome)
}

fn apply_jvm_formatters(
    config: &Config,
    cache: &Cache,
    downloader: &dyn Downloader,
    repo_root: &Path,
    files: &[PathBuf],
    check: bool,
    outcome: &mut FormatOutcome,
) -> Result<()> {
    let scopes = ToolScopes::build(config, repo_root)?;
    let kt_files: Vec<PathBuf> = files
        .iter()
        .filter(|p| scopes.matches_ktfmt(p))
        .map(|p| repo_root.join(p))
        .collect();
    let java_files: Vec<PathBuf> = files
        .iter()
        .filter(|p| scopes.matches_gjf(p))
        .map(|p| repo_root.join(p))
        .collect();

    if let Some(kt) = &config.ktfmt {
        if !kt_files.is_empty() {
            let jar = resolve_jar(kt.source(repo_root), &|v| cache.ensure_ktfmt(v, downloader))?;
            let invoker = formatters::Invoker::Jar(jar);
            let base = formatters::ktfmt_args(kt.style, check);
            if check {
                let run = formatters::run_batched_check(
                    "ktfmt",
                    &invoker,
                    &base,
                    &kt_files,
                    formatters::MAX_ARG_BYTES,
                )?;
                merge_jvm_check_run(outcome, repo_root, run);
            } else {
                let before = snapshot_files(&kt_files)?;
                formatters::run_batched(
                    "ktfmt",
                    &invoker,
                    &base,
                    &kt_files,
                    formatters::MAX_ARG_BYTES,
                )?;
                merge_format_changes(outcome, repo_root, before)?;
            }
        }
    }

    if let Some(g) = &config.gjf {
        if !java_files.is_empty() {
            let invoker = resolve_gjf_invoker(g, repo_root, cache, downloader)?;
            let base = formatters::gjf_args(g.style, check);
            if check {
                let run = formatters::run_argfile_check("gjf", &invoker, base, &java_files)?;
                merge_jvm_check_run(outcome, repo_root, run);
            } else {
                let before = snapshot_files(&java_files)?;
                formatters::run_argfile("gjf", &invoker, base, &java_files)?;
                merge_format_changes(outcome, repo_root, before)?;
            }
        }
    }

    Ok(())
}

fn apply_rustfmt(
    config: &Config,
    repo_root: &Path,
    files: &[PathBuf],
    check: bool,
    outcome: &mut FormatOutcome,
) -> Result<()> {
    if config.rustfmt.is_none() {
        return Ok(());
    }
    let scopes = ToolScopes::build(config, repo_root)?;
    let rust_files: Vec<PathBuf> = files
        .iter()
        .filter(|p| scopes.matches_rustfmt(p))
        .cloned()
        .collect();
    if rust_files.is_empty() {
        return Ok(());
    }

    if check {
        for rel in rust_files {
            let output = cargo_fmt(repo_root, check, std::slice::from_ref(&rel))?;
            if !output.status.success() {
                outcome.check_failed = true;
                outcome.changed.insert(rel);
                append_rustfmt_stderr(outcome, &output.stderr);
            }
        }
    } else {
        let abs_files: Vec<PathBuf> = rust_files.iter().map(|p| repo_root.join(p)).collect();
        let before = snapshot_files(&abs_files)?;
        let output = cargo_fmt(repo_root, check, &rust_files)?;
        if !output.status.success() {
            return Err(formatter_failure("cargo fmt", output));
        }
        merge_format_changes(outcome, repo_root, before)?;
    }
    Ok(())
}

fn cargo_fmt(repo_root: &Path, check: bool, files: &[PathBuf]) -> Result<std::process::Output> {
    let mut cmd = Command::new("cargo");
    cmd.arg("fmt");
    if check {
        cmd.arg("--check");
    }
    cmd.arg("--").current_dir(repo_root);
    for file in files {
        cmd.arg(file);
    }
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawn cargo fmt failed")
}

fn append_rustfmt_stderr(outcome: &mut FormatOutcome, stderr: &[u8]) {
    let stderr = String::from_utf8_lossy(stderr);
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return;
    }
    if !outcome.parse_errors.is_empty() {
        outcome.parse_errors.push('\n');
    }
    outcome.parse_errors.push_str(trimmed);
}

fn formatter_failure(tool: &str, output: std::process::Output) -> anyhow::Error {
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let details = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if details.is_empty() {
        anyhow!("{tool} failed (exit {code})")
    } else {
        anyhow!("{tool} failed (exit {code}):\n{details}")
    }
}

fn partial_gjf_candidates(
    config: &Config,
    repo_root: &Path,
    files: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    if config.gjf.is_none() {
        return Ok(Vec::new());
    }
    let scopes = ToolScopes::build(config, repo_root)?;
    Ok(files
        .iter()
        .filter(|p| SourceKind::from_path(p) == Some(SourceKind::Java) && scopes.matches_gjf(p))
        .cloned()
        .collect())
}

fn apply_partial_gjf_to_index(
    config: &Config,
    git: &dyn GitContext,
    cache: &Cache,
    downloader: &dyn Downloader,
    files: &[PathBuf],
) -> Result<BTreeSet<PathBuf>> {
    let Some(g) = &config.gjf else {
        return Ok(BTreeSet::new());
    };
    let invoker = resolve_gjf_invoker(g, git.root(), cache, downloader)?;
    let mut changed = BTreeSet::new();
    for rel in files {
        let diff = git.staged_diff(rel, 0)?;
        let line_ranges = parse_added_line_ranges(&diff);
        if line_ranges.is_empty() {
            continue;
        }

        let staged_contents = git.read_staged_file(rel)?;
        let mut tmp = tempfile::Builder::new()
            .prefix("kempt-partial-gjf-")
            .suffix(".java")
            .tempfile()
            .context("create partial gjf tempfile")?;
        tmp.write_all(&staged_contents)
            .with_context(|| format!("write staged contents for {}", rel.display()))?;
        tmp.flush()
            .with_context(|| format!("flush staged contents for {}", rel.display()))?;

        let mut args = formatters::gjf_args(g.style, false);
        for (start, end) in line_ranges {
            args.push("--lines".into());
            args.push(format!("{start}:{end}").into());
        }
        args.push(tmp.path().into());
        formatters::run("gjf", &invoker, args)
            .with_context(|| format!("partial gjf failed for {}", rel.display()))?;

        let formatted = std::fs::read(tmp.path())
            .with_context(|| format!("read partial gjf output for {}", rel.display()))?;
        if formatted != staged_contents {
            git.update_staged_file(rel, &formatted)?;
            changed.insert(rel.clone());
        }
    }
    Ok(changed)
}

fn parse_added_line_ranges(diff: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for line in diff.lines().filter(|line| line.starts_with("@@")) {
        let Some(spec) = line
            .split_whitespace()
            .find_map(|part| part.strip_prefix('+'))
        else {
            continue;
        };
        let mut parts = spec.splitn(2, ',');
        let Some(start) = parts.next().and_then(|s| s.parse::<usize>().ok()) else {
            continue;
        };
        let count = parts
            .next()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1);
        if count > 0 {
            ranges.push((start, start + count - 1));
        }
    }
    ranges
}

/// Where the user invoked kempt from, used to tailor the "run X to fix"
/// suggestion in the check summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckContext {
    /// `kempt check` (default scope = all tracked).
    All,
    /// `kempt check --staged`.
    Staged,
    /// `kempt check --discovery=walk`.
    Walk,
    /// Running as the pre-commit hook with `[hook] mode = "check"`.
    Hook,
    /// User passed an explicit list of paths.
    Explicit,
}

impl CheckContext {
    fn fix_command(self) -> &'static str {
        match self {
            Self::All => "kempt format --all",
            Self::Staged | Self::Hook => "kempt format --staged",
            Self::Walk => "kempt format --discovery=walk",
            Self::Explicit => "kempt format",
        }
    }

    fn allows_per_file_suggestion(self) -> bool {
        // The hook's trailer is already multi-line and re-stages anyway;
        // per-file is awkward there. When the user already passed explicit
        // paths, they've got the file list.
        !matches!(self, Self::Hook | Self::Explicit)
    }
}

/// Threshold above which the per-file copy/paste line is suppressed.
/// Past this, "kempt format --all" is more practical anyway.
pub const MAX_PER_FILE_SUGGESTION: usize = 30;

fn shell_escape(p: &Path) -> String {
    let s = p.display().to_string();
    let needs_quote = s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || matches!(c, '\'' | '"' | '\\' | '$' | '`' | '*' | '?'));
    if !needs_quote {
        return s;
    }
    // Single-quote, escape any embedded single quotes.
    let escaped = s.replace('\'', "'\\''");
    format!("'{escaped}'")
}

/// Build the actionable summary lines for a check-mode outcome. Returns
/// nothing when the outcome is clean (no changes, no parse errors).
pub fn render_check_summary(outcome: &FormatOutcome, ctx: CheckContext) -> Vec<String> {
    let mut lines = Vec::new();
    let n_changed = outcome.changed.len();
    let has_errors = outcome.has_parse_errors();
    if n_changed == 0 && !has_errors {
        return lines;
    }
    let cmd = ctx.fix_command();
    let (n_word, verb) = if n_changed == 1 {
        ("file", "needs")
    } else {
        ("files", "need")
    };

    if has_errors && n_changed > 0 {
        lines.push(format!(
            "kempt: {n_changed} {n_word} {verb} formatting; some have syntax errors."
        ));
        lines.push("  - Fix the syntax errors above".to_string());
        lines.push(format!("  - Run `{cmd}` to format the rest"));
    } else if has_errors {
        lines.push("kempt: syntax errors prevent formatting (see above).".to_string());
    } else if matches!(ctx, CheckContext::Hook) {
        lines.push(format!(
            "kempt: {n_changed} staged {n_word} {verb} formatting."
        ));
        lines.push(format!(
            "Run `{cmd}` to format and re-stage, then commit again."
        ));
        lines.push("Or commit with `--no-verify` to bypass.".to_string());
    } else if matches!(ctx, CheckContext::Explicit) {
        // The user already typed the file list; tell them to swap `check`
        // for `format`.
        lines.push(format!(
            "kempt: {n_changed} {n_word} {verb} formatting. Re-run with `format` instead of `check` to apply."
        ));
    } else {
        lines.push(format!(
            "kempt: {n_changed} {n_word} {verb} formatting. Run `{cmd}` to apply."
        ));
        if ctx.allows_per_file_suggestion() && n_changed <= MAX_PER_FILE_SUGGESTION {
            let escaped: Vec<String> = outcome.changed.iter().map(|p| shell_escape(p)).collect();
            lines.push("Or to format just these files:".to_string());
            lines.push(format!("  kempt format {}", escaped.join(" ")));
        }
    }
    lines
}

fn merge_jvm_check_run(outcome: &mut FormatOutcome, repo_root: &Path, run: formatters::CheckRun) {
    if !run.success {
        outcome.check_failed = true;
    }
    for abs in run.paths {
        let p = Path::new(&abs);
        let rel = p.strip_prefix(repo_root).unwrap_or(p).to_path_buf();
        outcome.changed.insert(rel);
    }
    if !run.stderr.is_empty() {
        if !outcome.parse_errors.is_empty() {
            outcome.parse_errors.push('\n');
        }
        outcome.parse_errors.push_str(&run.stderr);
    }
}

fn snapshot_files(files: &[PathBuf]) -> Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut before = Vec::with_capacity(files.len());
    for file in files {
        let contents = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
        before.push((file.clone(), contents));
    }
    Ok(before)
}

fn merge_format_changes(
    outcome: &mut FormatOutcome,
    repo_root: &Path,
    before: Vec<(PathBuf, Vec<u8>)>,
) -> Result<()> {
    for (abs, old_contents) in before {
        let new_contents =
            std::fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        if new_contents != old_contents {
            let rel = abs
                .strip_prefix(repo_root)
                .with_context(|| format!("strip repo root from {}", abs.display()))?
                .to_path_buf();
            outcome.changed.insert(rel);
        }
    }
    Ok(())
}

/// Resolve a [`ToolSource`] to a concrete jar path. `Cached` delegates to the
/// download/cache helper; `Local` checks that the jar exists.
fn resolve_jar(
    source: ToolSource,
    ensure_cached: &dyn Fn(&str) -> Result<PathBuf>,
) -> Result<PathBuf> {
    match source {
        ToolSource::Cached(v) => ensure_cached(&v),
        ToolSource::Local(path) => {
            if !path.exists() {
                anyhow::bail!("jar not found: {}", path.display());
            }
            Ok(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::testing::FakeDownloader;
    use crate::config::{LicenseHeader, Paths, Whitespace};
    use crate::git::{testing::FakeGit, RealGit};

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
    }

    fn config_inproc_only(root: &Path) -> Config {
        // Header file at config/header.txt
        write(root, "config/header.txt", "// (c) ${YEAR} test\n");
        Config {
            ktfmt: None,
            gjf: None,
            rustfmt: None,
            license_header: Some(LicenseHeader {
                file: PathBuf::from("config/header.txt"),
            }),
            paths: Paths {
                exclude: crate::config::GlobList::Inline(vec![]),
            },
            whitespace: Whitespace::default(),
            hook: Default::default(),
        }
    }

    #[cfg(unix)]
    fn fake_gjf_appending_reformatted(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let fake_gjf = root.join("fake-gjf");
        std::fs::write(
            &fake_gjf,
            "#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               case \"$arg\" in\n\
                 @*)\n\
                   argfile=\"${arg#@}\"\n\
                   while IFS= read -r f; do\n\
                     [ -n \"$f\" ] || continue\n\
                     printf 'reformatted\\n' >> \"$f\"\n\
                   done < \"$argfile\"\n\
                   ;;\n\
               esac\n\
             done\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_gjf).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_gjf, perms).unwrap();
        fake_gjf
    }

    #[cfg(unix)]
    fn config_gjf_only(fake_gjf: PathBuf) -> Config {
        Config {
            ktfmt: None,
            gjf: Some(crate::config::Gjf {
                version: None,
                path: Some(fake_gjf),
                style: Default::default(),
                license_header: None,
                native: Default::default(),
                paths: None,
            }),
            rustfmt: None,
            license_header: None,
            paths: Paths {
                exclude: crate::config::GlobList::Inline(vec![]),
            },
            whitespace: Whitespace::default(),
            hook: Default::default(),
        }
    }

    fn config_rustfmt_only() -> Config {
        Config {
            rustfmt: Some(crate::config::Rustfmt::default()),
            paths: Paths {
                exclude: crate::config::GlobList::Inline(vec![]),
            },
            ..Default::default()
        }
    }

    #[cfg(unix)]
    fn fake_gjf_marking_new_line(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let fake_gjf = root.join("fake-partial-gjf");
        std::fs::write(
            &fake_gjf,
            "#!/bin/sh\n\
             file=\"\"\n\
             for arg in \"$@\"; do\n\
               case \"$arg\" in\n\
                 *.java) file=\"$arg\" ;;\n\
               esac\n\
             done\n\
             [ -n \"$file\" ] || exit 2\n\
             awk '{ if ($0 ~ /new staged/) print $0 \" // formatted\"; else print }' \"$file\" > \"$file.out\"\n\
             mv \"$file.out\" \"$file\"\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_gjf).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake_gjf, perms).unwrap();
        fake_gjf
    }

    #[cfg(unix)]
    fn git_cmd(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[test]
    fn parse_added_line_ranges_reads_cached_diff_hunks() {
        let diff = "\
diff --git a/Foo.java b/Foo.java\n\
@@ -1 +1,2 @@\n\
@@ -8,0 +10,3 @@\n\
@@ -20,2 +24,0 @@\n";
        assert_eq!(parse_added_line_ranges(diff), vec![(1, 2), (10, 12)]);
    }

    #[test]
    fn run_format_inserts_header_and_fixes_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.kt", "package foo   \n");
        let cfg = config_inproc_only(root);
        let git = FakeGit::new(root).with_tracked(vec!["src/Foo.kt"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, false, 2026).unwrap();
        assert_eq!(out.changed.len(), 1);
        assert!(!out.check_failed);

        let body = std::fs::read_to_string(root.join("src/Foo.kt")).unwrap();
        assert!(body.starts_with("// (c) 2026 test"));
        assert!(!body.contains("foo   "));
    }

    #[test]
    fn run_format_check_mode_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.kt", "package foo   \n");
        let cfg = config_inproc_only(root);
        let git = FakeGit::new(root).with_tracked(vec!["src/Foo.kt"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, true, 2026).unwrap();
        assert_eq!(out.changed.len(), 1);
        assert!(out.check_failed);

        let body = std::fs::read_to_string(root.join("src/Foo.kt")).unwrap();
        assert_eq!(body, "package foo   \n", "check mode must not modify files");
    }

    #[test]
    fn run_format_clean_files_yield_empty_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.kt", "// (c) 2024 test\npackage foo\n");
        let cfg = config_inproc_only(root);
        let git = FakeGit::new(root).with_tracked(vec!["src/Foo.kt"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, false, 2026).unwrap();
        assert!(out.changed.is_empty());
        assert!(!out.check_failed);
    }

    #[cfg(unix)]
    #[test]
    fn run_format_reports_jvm_formatter_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.java", "public class Foo {}\n");
        let cfg = config_gjf_only(fake_gjf_appending_reformatted(root));
        let git = FakeGit::new(root).with_tracked(vec!["src/Foo.java"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, false, 2026).unwrap();
        assert_eq!(out.changed, BTreeSet::from([PathBuf::from("src/Foo.java")]));
        assert!(!out.check_failed);

        let body = std::fs::read_to_string(root.join("src/Foo.java")).unwrap();
        assert!(body.contains("reformatted"));
    }

    #[test]
    fn run_format_runs_cargo_fmt_for_rust_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"kempt-rust-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(root, "src/lib.rs", "pub fn answer()->i32{1}\n");
        let cfg = config_rustfmt_only();
        let git = FakeGit::new(root).with_tracked(vec!["src/lib.rs"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, false, 2026).unwrap();
        assert_eq!(out.changed, BTreeSet::from([PathBuf::from("src/lib.rs")]));

        let body = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(body, "pub fn answer() -> i32 {\n    1\n}\n");
    }

    #[test]
    fn run_format_inserts_rust_header_and_runs_cargo_fmt() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"kempt-rust-header-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(root, "config/header.txt", "// (c) ${YEAR} test\n");
        write(root, "src/lib.rs", "pub fn answer()->i32{1}\n");
        let mut cfg = config_rustfmt_only();
        cfg.license_header = Some(LicenseHeader {
            file: PathBuf::from("config/header.txt"),
        });
        let git = FakeGit::new(root).with_tracked(vec!["src/lib.rs"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, false, 2026).unwrap();
        assert_eq!(out.changed, BTreeSet::from([PathBuf::from("src/lib.rs")]));

        let body = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(
            body,
            "// (c) 2026 test\npub fn answer() -> i32 {\n    1\n}\n"
        );
    }

    #[test]
    fn run_format_check_mode_reports_rustfmt_changes_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "Cargo.toml",
            "[package]\nname = \"kempt-rust-check-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        write(root, "src/lib.rs", "pub fn answer()->i32{1}\n");
        let cfg = config_rustfmt_only();
        let git = FakeGit::new(root).with_tracked(vec!["src/lib.rs"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_format(&cfg, &git, &cache, &dl, Scope::All, true, 2026).unwrap();
        assert_eq!(out.changed, BTreeSet::from([PathBuf::from("src/lib.rs")]));
        assert!(out.check_failed);

        let body = std::fs::read_to_string(root.join("src/lib.rs")).unwrap();
        assert_eq!(body, "pub fn answer()->i32{1}\n");
    }

    #[test]
    fn run_hook_aborts_on_partial_stage() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.kt", "package foo   \n");
        let cfg = config_inproc_only(root);
        let git = FakeGit::new(root)
            .with_tracked(vec!["src/Foo.kt"])
            .with_staged(vec!["src/Foo.kt"])
            .with_unstaged(vec!["src/Foo.kt"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let err = run_hook_inner(&cfg, &git, &cache, &dl, 2026, false).unwrap_err();
        assert!(format!("{err:#}").contains("partial staging"));
        // file should not have been modified
        let body = std::fs::read_to_string(root.join("src/Foo.kt")).unwrap();
        assert_eq!(body, "package foo   \n");
    }

    #[cfg(unix)]
    #[test]
    fn run_hook_partial_gjf_updates_index_without_staging_unstaged_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_cmd(root, &["init"]);
        git_cmd(root, &["config", "user.email", "test@example.com"]);
        git_cmd(root, &["config", "user.name", "Test User"]);

        write(
            root,
            "src/Foo.java",
            "public class Foo {\n\
             void staged() {\n\
               System.out.println(\"old staged\");\n\
             }\n\
             void unstaged() {\n\
               System.out.println(\"old unstaged\");\n\
             }\n\
             }\n",
        );
        git_cmd(root, &["add", "src/Foo.java"]);
        git_cmd(root, &["commit", "-m", "initial"]);

        write(
            root,
            "src/Foo.java",
            "public class Foo {\n\
             void staged() {\n\
               System.out.println(\"new staged\");\n\
             }\n\
             void unstaged() {\n\
               System.out.println(\"old unstaged\");\n\
             }\n\
             }\n",
        );
        git_cmd(root, &["add", "src/Foo.java"]);
        write(
            root,
            "src/Foo.java",
            "public class Foo {\n\
             void staged() {\n\
               System.out.println(\"new staged\");\n\
             }\n\
             void unstaged() {\n\
               System.out.println(\"worktree unstaged\");\n\
             }\n\
             }\n",
        );

        let cfg = config_gjf_only(fake_gjf_marking_new_line(root));
        let git = RealGit::discover(root).unwrap();
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_hook_inner(&cfg, &git, &cache, &dl, 2026, true).unwrap();
        assert_eq!(out.changed, BTreeSet::from([PathBuf::from("src/Foo.java")]));

        let staged = git_cmd(root, &["show", ":src/Foo.java"]);
        assert!(staged.contains("new staged\"); // formatted"));
        assert!(staged.contains("old unstaged"));
        assert!(!staged.contains("worktree unstaged"));

        let worktree = std::fs::read_to_string(root.join("src/Foo.java")).unwrap();
        assert!(worktree.contains("new staged\");"));
        assert!(worktree.contains("worktree unstaged"));
        assert!(!worktree.contains("// formatted"));
    }

    #[test]
    fn run_hook_formats_and_restages_safe_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.kt", "package foo   \n");
        let cfg = config_inproc_only(root);
        let git = FakeGit::new(root)
            .with_tracked(vec!["src/Foo.kt"])
            .with_staged(vec!["src/Foo.kt"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        let out = run_hook(&cfg, &git, &cache, &dl, 2026).unwrap();
        assert_eq!(out.changed.len(), 1);
        let added = git.added.borrow();
        assert_eq!(*added, vec![PathBuf::from("src/Foo.kt")]);
    }

    // Regression test for the hook silently dropping ktfmt/gjf re-stages.
    // Uses gjf's `path = "..."` with a non-`.jar` extension so kempt picks
    // `Invoker::Native` and runs the binary directly. The fake shell script
    // mimics `gjf --replace @argfile` by mutating each listed file in place.
    #[cfg(unix)]
    #[test]
    fn run_hook_restages_jvm_formatter_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/Foo.java", "public class Foo {}\n");
        let cfg = config_gjf_only(fake_gjf_appending_reformatted(root));

        let git = FakeGit::new(root)
            .with_tracked(vec!["src/Foo.java"])
            .with_staged(vec!["src/Foo.java"]);
        let cache = Cache::new(root.join(".cache"));
        let dl = FakeDownloader::new(b"".to_vec());

        run_hook(&cfg, &git, &cache, &dl, 2026).unwrap();

        let added = git.added.borrow();
        assert!(
            added.contains(&PathBuf::from("src/Foo.java")),
            "expected hook to git-add the gjf-modified file, got {:?}",
            *added
        );

        let body = std::fs::read_to_string(root.join("src/Foo.java")).unwrap();
        assert!(
            body.contains("reformatted"),
            "fake gjf did not run; file body: {body:?}"
        );
    }

    #[test]
    fn run_init_writes_starter_files() {
        let dir = tempfile::tempdir().unwrap();
        let written = run_init(dir.path()).unwrap();
        assert_eq!(written.len(), 2);
        assert!(dir.path().join(".kempt.toml").exists());
        assert!(dir.path().join("config/license-header.txt").exists());
    }

    #[test]
    fn run_init_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let _ = run_init(dir.path()).unwrap();
        let again = run_init(dir.path()).unwrap();
        assert!(again.is_empty(), "second run should write nothing");
    }

    fn write_blank(dir: &Path, rel: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, "").unwrap();
    }

    #[test]
    fn detect_languages_finds_kotlin() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Foo.kt");
        let langs = detect_languages(dir.path());
        assert!(langs.kotlin && !langs.java && !langs.rust);
    }

    #[test]
    fn detect_languages_finds_java() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Bar.java");
        let langs = detect_languages(dir.path());
        assert!(langs.java && !langs.kotlin && !langs.rust);
    }

    #[test]
    fn detect_languages_finds_rust() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/lib.rs");
        let langs = detect_languages(dir.path());
        assert!(langs.rust && !langs.kotlin && !langs.java);
    }

    #[test]
    fn detect_languages_finds_kts_as_kotlin() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "build.gradle.kts");
        let langs = detect_languages(dir.path());
        assert!(langs.kotlin);
    }

    #[test]
    fn detect_languages_finds_both() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Foo.kt");
        write_blank(dir.path(), "src/Bar.java");
        write_blank(dir.path(), "src/lib.rs");
        let langs = detect_languages(dir.path());
        assert!(langs.kotlin && langs.java && langs.rust);
    }

    #[test]
    fn detect_languages_skips_build_and_dot_git() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "build/Generated.kt");
        write_blank(dir.path(), ".git/hooks/script.java");
        write_blank(dir.path(), "target/generated.rs");
        let langs = detect_languages(dir.path());
        assert!(!langs.kotlin && !langs.java && !langs.rust);
    }

    #[test]
    fn run_init_kotlin_only_omits_gjf() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Foo.kt");
        run_init(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".kempt.toml")).unwrap();
        assert!(body.contains("[ktfmt]"));
        assert!(!body.contains("[gjf]"));
        assert!(!body.contains("[rustfmt]"));
    }

    #[test]
    fn run_init_java_only_omits_ktfmt() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Bar.java");
        run_init(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".kempt.toml")).unwrap();
        assert!(body.contains("[gjf]"));
        assert!(!body.contains("[ktfmt]"));
        assert!(!body.contains("[rustfmt]"));
    }

    #[test]
    fn run_init_rust_only_omits_ktfmt_and_gjf() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/lib.rs");
        run_init(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".kempt.toml")).unwrap();
        assert!(body.contains("[rustfmt]"));
        assert!(!body.contains("[ktfmt]"));
        assert!(!body.contains("[gjf]"));
    }

    #[test]
    fn run_init_empty_repo_writes_both_sections() {
        let dir = tempfile::tempdir().unwrap();
        run_init(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".kempt.toml")).unwrap();
        assert!(body.contains("[ktfmt]"));
        assert!(body.contains("[gjf]"));
        assert!(body.contains("[rustfmt]"));
    }

    #[test]
    fn run_init_starter_does_not_write_explicit_default_styles() {
        // Both ktfmt and gjf default to google style; the starter shouldn't
        // emit `style = "google"` redundantly.
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Foo.kt");
        write_blank(dir.path(), "src/Bar.java");
        write_blank(dir.path(), "src/lib.rs");
        run_init(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".kempt.toml")).unwrap();
        assert!(
            !body.contains("style = \"google\""),
            "starter config should rely on the default style, got:\n{body}"
        );
    }

    #[test]
    fn run_init_starter_parses_as_valid_config() {
        let dir = tempfile::tempdir().unwrap();
        write_blank(dir.path(), "src/Foo.kt");
        write_blank(dir.path(), "src/Bar.java");
        write_blank(dir.path(), "src/lib.rs");
        run_init(dir.path()).unwrap();
        let body = std::fs::read_to_string(dir.path().join(".kempt.toml")).unwrap();
        // Must be a valid kempt config end-to-end.
        let cfg = Config::parse(&body).expect("starter parses");
        assert!(cfg.ktfmt.is_some());
        assert!(cfg.gjf.is_some());
        assert!(cfg.rustfmt.is_some());
        assert_eq!(cfg.ktfmt.unwrap().style, crate::config::KtfmtStyle::Google);
        assert_eq!(cfg.gjf.unwrap().style, crate::config::GjfStyle::Google);
    }

    #[test]
    fn run_update_fetches_only_configured_tools() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"jar".to_vec());

        let cfg = Config {
            ktfmt: Some(crate::config::Ktfmt {
                version: Some(crate::config::VersionSpec::literal("0.56")),
                path: None,
                style: Default::default(),
                license_header: None,
                paths: None,
            }),
            ..Default::default()
        };
        run_update(&cfg, dir.path(), &cache, &dl).unwrap();
        assert_eq!(dl.calls.borrow().len(), 1);
        assert!(cache.ktfmt_path("0.56").exists());
        assert!(!cache.gjf_jar_path("1.28.0").exists());
    }

    #[test]
    fn run_update_skips_tools_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"jar".to_vec());

        let cfg = Config {
            ktfmt: Some(crate::config::Ktfmt {
                version: None,
                path: Some(PathBuf::from("config/bin/ktfmt.jar")),
                style: Default::default(),
                license_header: None,
                paths: None,
            }),
            ..Default::default()
        };
        run_update(&cfg, dir.path(), &cache, &dl).unwrap();
        assert!(
            dl.calls.borrow().is_empty(),
            "must not download for in-repo jars"
        );
    }

    #[test]
    fn keep_paths_excludes_in_repo_jars() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let cfg = Config {
            ktfmt: Some(crate::config::Ktfmt {
                version: None,
                path: Some(PathBuf::from("config/bin/ktfmt.jar")),
                style: Default::default(),
                license_header: None,
                paths: None,
            }),
            gjf: Some(crate::config::Gjf {
                version: Some(crate::config::VersionSpec::literal("1.28.0")),
                path: None,
                style: Default::default(),
                license_header: None,
                // Force jar-only so the keep set is deterministic across
                // host platforms (some auto-resolve to native on this host).
                native: NativeMode::Never,
                paths: None,
            }),
            ..Default::default()
        };
        let keep = keep_paths_for_config(&cfg, dir.path(), &cache);
        // ktfmt's in-repo jar shouldn't appear; gjf's jar path should.
        assert_eq!(keep.len(), 1);
        assert!(keep[0].ends_with("gjf-1.28.0.jar"));
    }

    #[test]
    fn keep_paths_includes_both_jar_and_native_when_native_auto() {
        // With native = auto on a platform that publishes a native build,
        // we keep both candidate paths since prune doesn't know which one
        // is actually downloaded.
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let cfg = Config {
            gjf: Some(crate::config::Gjf {
                version: Some(crate::config::VersionSpec::literal("1.28.0")),
                path: None,
                style: Default::default(),
                license_header: None,
                native: NativeMode::Auto,
                paths: None,
            }),
            ..Default::default()
        };
        let keep = keep_paths_for_config(&cfg, dir.path(), &cache);
        // At least the jar path is always present.
        assert!(keep.iter().any(|p| p.ends_with("gjf-1.28.0.jar")));
        // On hosts where the native asset is published, the native path is
        // also kept. Don't enforce platform specifics in the assertion.
        if crate::cache::current_native_asset().is_some() {
            assert!(keep.len() == 2);
        }
    }

    fn vendor_test_setup() -> (tempfile::TempDir, Cache, FakeDownloader) {
        let dir = tempfile::tempdir().unwrap();
        // Cache lives outside the repo so we can assert vendoring copies into the repo.
        let cache = Cache::new(dir.path().join(".cache"));
        let dl = FakeDownloader::new(b"jar-bytes".to_vec());
        (dir, cache, dl)
    }

    fn version_only_config() -> Config {
        Config {
            ktfmt: Some(crate::config::Ktfmt {
                version: Some(crate::config::VersionSpec::literal("0.56")),
                path: None,
                style: Default::default(),
                license_header: None,
                paths: None,
            }),
            gjf: Some(crate::config::Gjf {
                version: Some(crate::config::VersionSpec::literal("1.28.0")),
                path: None,
                style: Default::default(),
                license_header: None,
                // Force jar so vendor tests assert deterministic filenames
                // regardless of host platform.
                native: NativeMode::Never,
                paths: None,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn run_vendor_copies_jars_into_target_dir() {
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = version_only_config();

        let outcome =
            run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();

        assert_eq!(outcome.entries.len(), 2);
        assert!(outcome.skipped.is_empty());

        let ktfmt_dest = dir.path().join("config/bin/ktfmt-0.56.jar");
        let gjf_dest = dir.path().join("config/bin/gjf-1.28.0.jar");
        assert!(ktfmt_dest.exists(), "ktfmt jar should be copied");
        assert!(gjf_dest.exists(), "gjf jar should be copied");

        // Contents match the (faked) cache payload.
        assert_eq!(std::fs::read(&ktfmt_dest).unwrap(), b"jar-bytes");
        assert_eq!(std::fs::read(&gjf_dest).unwrap(), b"jar-bytes");
    }

    #[test]
    fn run_vendor_creates_target_dir() {
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = version_only_config();

        let nested = PathBuf::from("vendor/jars");
        assert!(!dir.path().join(&nested).exists());

        run_vendor(&cfg, dir.path(), &cache, &dl, &nested).unwrap();
        assert!(dir.path().join(&nested).exists());
    }

    #[test]
    fn run_vendor_skips_tools_already_using_path() {
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = Config {
            ktfmt: Some(crate::config::Ktfmt {
                version: None,
                path: Some(PathBuf::from("config/bin/ktfmt.jar")),
                style: Default::default(),
                license_header: None,
                paths: None,
            }),
            gjf: Some(crate::config::Gjf {
                version: Some(crate::config::VersionSpec::literal("1.28.0")),
                path: None,
                style: Default::default(),
                license_header: None,
                native: Default::default(),
                paths: None,
            }),
            ..Default::default()
        };

        let outcome =
            run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();

        assert_eq!(outcome.entries.len(), 1);
        assert_eq!(outcome.entries[0].tool, "gjf");
        assert_eq!(outcome.skipped, vec!["ktfmt"]);
    }

    #[test]
    fn run_vendor_config_value_uses_relative_target_for_relative_dir() {
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = version_only_config();
        let outcome =
            run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();
        let kt = outcome.entries.iter().find(|e| e.tool == "ktfmt").unwrap();
        assert_eq!(kt.config_value, PathBuf::from("config/bin/ktfmt-0.56.jar"));
    }

    #[test]
    fn run_vendor_is_idempotent() {
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = version_only_config();
        run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();
        // Second run should not error and should leave the files in place.
        let outcome =
            run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();
        assert_eq!(outcome.entries.len(), 2);
        assert!(dir.path().join("config/bin/ktfmt-0.56.jar").exists());
    }

    #[test]
    fn run_vendor_with_no_configured_tools_returns_empty() {
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = Config::default();
        let outcome =
            run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();
        assert!(outcome.entries.is_empty());
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn run_vendor_with_native_gjf_copies_native_binary() {
        // Only meaningful on hosts where a native asset is available; this
        // test is a no-op (still asserts) when running on, say, an Intel Mac.
        let Some(_asset) = crate::cache::current_native_asset() else {
            return;
        };
        let (dir, cache, dl) = vendor_test_setup();
        let cfg = Config {
            gjf: Some(crate::config::Gjf {
                version: Some(crate::config::VersionSpec::literal("1.28.0")),
                path: None,
                style: Default::default(),
                license_header: None,
                native: NativeMode::Always,
                paths: None,
            }),
            ..Default::default()
        };
        let outcome =
            run_vendor(&cfg, dir.path(), &cache, &dl, &PathBuf::from("config/bin")).unwrap();
        assert_eq!(outcome.entries.len(), 1);
        let entry = &outcome.entries[0];
        // Native filename: gjf-<v>-<asset>[.exe], no .jar extension.
        let dest_str = entry.dest.to_string_lossy();
        assert!(
            dest_str.contains("gjf-1.28.0-") && !dest_str.ends_with(".jar"),
            "expected native filename, got {dest_str}"
        );
    }

    // --- check summary ---

    fn outcome_with(changed: &[&str], parse_errors: &str) -> FormatOutcome {
        let mut o = FormatOutcome::default();
        for p in changed {
            o.changed.insert(PathBuf::from(p));
        }
        o.parse_errors = parse_errors.to_string();
        o
    }

    #[test]
    fn check_summary_clean_outcome_returns_no_lines() {
        let out = FormatOutcome::default();
        let summary = render_check_summary(&out, CheckContext::All);
        assert!(summary.is_empty());
    }

    #[test]
    fn check_summary_files_only_suggests_kempt_format_all() {
        let out = outcome_with(&["a.kt", "b.kt"], "");
        let summary = render_check_summary(&out, CheckContext::All);
        assert!(
            summary[0].contains("2 files need formatting. Run `kempt format --all` to apply."),
            "got: {}",
            summary[0]
        );
    }

    #[test]
    fn check_summary_singular_pluralizes_correctly() {
        let out = outcome_with(&["only.kt"], "");
        let summary = render_check_summary(&out, CheckContext::All);
        assert!(summary[0].contains("1 file needs formatting"));
    }

    #[test]
    fn check_summary_staged_scope_suggests_staged_command() {
        let out = outcome_with(&["a.kt"], "");
        let summary = render_check_summary(&out, CheckContext::Staged);
        assert!(summary[0].contains("kempt format --staged"));
    }

    #[test]
    fn check_summary_walk_scope_suggests_walk_command() {
        let out = outcome_with(&["a.kt"], "");
        let summary = render_check_summary(&out, CheckContext::Walk);
        assert!(summary[0].contains("kempt format --discovery=walk"));
    }

    #[test]
    fn check_summary_hook_context_uses_multi_line_guidance() {
        let out = outcome_with(&["a.kt"], "");
        let summary = render_check_summary(&out, CheckContext::Hook);
        assert!(summary.iter().any(|l| l.contains("staged file")));
        assert!(summary.iter().any(|l| l.contains("kempt format --staged")));
        assert!(summary.iter().any(|l| l.contains("--no-verify")));
    }

    #[test]
    fn check_summary_parse_errors_only_describes_them() {
        let out = outcome_with(&[], "Bad.kt:3:11: error: Expecting ')'");
        let summary = render_check_summary(&out, CheckContext::All);
        assert_eq!(
            summary,
            vec!["kempt: syntax errors prevent formatting (see above).".to_string()]
        );
    }

    #[test]
    fn check_summary_mixed_failures_lists_both_steps() {
        let out = outcome_with(&["a.kt", "b.kt"], "Bad.kt:3:11: error: bad");
        let summary = render_check_summary(&out, CheckContext::All);
        assert!(summary[0].contains("2 files need formatting"));
        assert!(summary[0].contains("syntax errors"));
        assert!(summary.iter().any(|l| l.contains("Fix the syntax errors")));
        assert!(summary.iter().any(|l| l.contains("kempt format")));
    }

    #[test]
    fn check_summary_includes_per_file_command_when_under_threshold() {
        let out = outcome_with(&["src/Foo.kt", "src/Bar.kt"], "");
        let summary = render_check_summary(&out, CheckContext::All);
        // Path list is included as a copy/pasteable command.
        let joined = summary.join("\n");
        assert!(
            joined.contains("kempt format src/Bar.kt src/Foo.kt"),
            "expected per-file command, got:\n{joined}"
        );
        assert!(joined.contains("Or to format just these files:"));
    }

    #[test]
    fn check_summary_omits_per_file_command_above_threshold() {
        let many: Vec<String> = (0..MAX_PER_FILE_SUGGESTION + 1)
            .map(|i| format!("file_{i}.kt"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let out = outcome_with(&refs, "");
        let summary = render_check_summary(&out, CheckContext::All);
        let joined = summary.join("\n");
        assert!(
            !joined.contains("Or to format just these files"),
            "should suppress per-file command above threshold, got:\n{joined}"
        );
    }

    #[test]
    fn check_summary_hook_context_skips_per_file_suggestion() {
        let out = outcome_with(&["a.kt", "b.kt"], "");
        let summary = render_check_summary(&out, CheckContext::Hook);
        let joined = summary.join("\n");
        assert!(!joined.contains("Or to format just these files"));
    }

    #[test]
    fn check_summary_explicit_context_tells_user_to_swap_check_for_format() {
        let out = outcome_with(&["a.kt", "b.kt"], "");
        let summary = render_check_summary(&out, CheckContext::Explicit);
        let joined = summary.join("\n");
        assert!(joined.contains("Re-run with `format` instead of `check`"));
        assert!(
            !joined.contains("Or to format just these files"),
            "explicit context shouldn't list files back: {joined}"
        );
    }

    #[test]
    fn check_summary_quotes_paths_with_spaces() {
        let out = outcome_with(&["src/has space.kt", "src/Plain.kt"], "");
        let summary = render_check_summary(&out, CheckContext::All);
        let joined = summary.join("\n");
        assert!(joined.contains("'src/has space.kt'"));
        assert!(joined.contains(" src/Plain.kt"));
    }

    #[test]
    fn check_summary_does_not_print_paths_in_main_summary_line() {
        // The first line is the "N files need formatting" summary; that
        // line shouldn't include path text. A dedicated subsequent line
        // does, but only when allowed.
        let out = outcome_with(&["a/very/long/path/Foo.kt"], "");
        let summary = render_check_summary(&out, CheckContext::All);
        assert!(!summary[0].contains("Foo.kt"));
    }

    // --- shell_escape ---

    #[test]
    fn shell_escape_leaves_simple_paths_alone() {
        assert_eq!(shell_escape(Path::new("src/Foo.kt")), "src/Foo.kt");
        assert_eq!(shell_escape(Path::new("a/b/c/Bar.java")), "a/b/c/Bar.java");
    }

    #[test]
    fn shell_escape_quotes_paths_with_spaces() {
        assert_eq!(shell_escape(Path::new("with space.kt")), "'with space.kt'");
    }

    #[test]
    fn shell_escape_escapes_embedded_single_quotes() {
        // POSIX-safe: end the quote, escape the apostrophe, restart the quote.
        assert_eq!(shell_escape(Path::new("ain't.kt")), r#"'ain'\''t.kt'"#);
    }

    #[test]
    fn shell_escape_quotes_glob_metachars() {
        assert_eq!(shell_escape(Path::new("foo*.kt")), "'foo*.kt'");
        assert_eq!(shell_escape(Path::new("foo?.kt")), "'foo?.kt'");
    }
}
