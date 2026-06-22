// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Build java command lines for ktfmt and gjf, and execute them.
//!
//! Two helpers handle large file lists:
//! - [`run_batched`] chunks the argv (used for ktfmt, which doesn't support
//!   `@file`).
//! - [`run_argfile`] writes the file list to a tempfile and passes it as
//!   `@<path>` (used for gjf).
//!
//! The [`Invoker`] enum abstracts over jar-via-JVM and native-binary modes
//! so callers don't have to branch.

use crate::config::{GjfStyle, KtfmtStyle};
use anyhow::{anyhow, Context, Result};
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// How to spawn a formatter binary.
#[derive(Debug, Clone)]
pub enum Invoker {
    /// Run via `java -jar <path>`. JVM `--add-opens` flags are added.
    Jar(PathBuf),
    /// Run the binary directly (e.g. a GraalVM native image).
    Native(PathBuf),
}

fn build_command(invoker: &Invoker) -> Result<Command> {
    match invoker {
        Invoker::Jar(jar) => {
            let java = which::which("java").map_err(|_| {
                anyhow!("`java` not found on PATH (set JAVA_HOME or install a JDK)")
            })?;
            let mut cmd = Command::new(&java);
            cmd.args(jvm_flags());
            cmd.arg("-jar").arg(jar);
            Ok(cmd)
        }
        Invoker::Native(bin) => Ok(Command::new(bin)),
    }
}

/// Conservative argv budget per process. `ARG_MAX` on macOS is 1 MiB, Linux
/// is 2 MiB+. Picking 100 KiB leaves plenty of headroom for env vars and the
/// fixed flags. Higher values would mean fewer JVM starts but more risk on
/// constrained systems.
pub const MAX_ARG_BYTES: usize = 100 * 1024;

const JVM_FLAGS: &[&str] = &[
    "-Xmx512m",
    "--add-opens=java.base/java.lang=ALL-UNNAMED",
    "--add-opens=java.base/java.util=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.comp=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.file=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.jvm=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.main=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.model=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.parser=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.processing=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
    "--add-opens=jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED",
];

pub fn jvm_flags() -> Vec<OsString> {
    JVM_FLAGS.iter().map(OsString::from).collect()
}

/// Static ktfmt flags (style, check mode). Does NOT include the file list;
/// callers append files via [`run_batched`].
pub fn ktfmt_args(style: KtfmtStyle, check: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::with_capacity(4);
    args.push(
        match style {
            KtfmtStyle::Google => "--google-style",
            KtfmtStyle::Kotlinlang => "--kotlinlang-style",
            KtfmtStyle::Meta => "--meta-style",
        }
        .into(),
    );
    args.push("--quiet".into());
    if check {
        args.push("--dry-run".into());
        args.push("--set-exit-if-changed".into());
    }
    args
}

/// Static gjf flags (style, check mode). Does NOT include the file list;
/// callers pass files via [`run_argfile`].
pub fn gjf_args(style: GjfStyle, check: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::with_capacity(3);
    if style == GjfStyle::Aosp {
        args.push("--aosp".into());
    }
    if check {
        args.push("--dry-run".into());
        args.push("--set-exit-if-changed".into());
    } else {
        args.push("--replace".into());
    }
    args
}

/// Spawn the invoker. Stdout is inherited so the user sees the formatter's
/// per-file diagnostic output. Stderr is captured and surfaced in the error
/// on non-zero exit; on success it's discarded (this hides JVM `--add-opens`
/// deprecation warnings during normal operation).
///
/// `tool` is the user-facing label ("ktfmt", "gjf") used in error messages.
pub fn run(tool: &str, invoker: &Invoker, args: Vec<OsString>) -> Result<()> {
    let mut cmd = build_command(invoker)?;
    cmd.args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped());
    let output = cmd
        .spawn()
        .with_context(|| format!("spawn {tool} failed"))?
        .wait_with_output()
        .with_context(|| format!("wait for {tool}"))?;
    if output.status.success() {
        return Ok(());
    }
    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let filtered = filter_jvm_noise(&stderr);
    if filtered.is_empty() {
        Err(anyhow!("{tool} failed (exit {code})"))
    } else {
        Err(anyhow!("{tool} failed (exit {code}):\n{filtered}"))
    }
}

/// Drop JVM "WARNING:" lines (e.g. sun.misc.Unsafe deprecations) from
/// captured stderr. Those are noise on every invocation and crowd out the
/// actual formatter diagnostic.
fn filter_jvm_noise(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| !l.trim_start().starts_with("WARNING:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Run `tool` against `files` in batches. `base_args` is the static argument
/// prefix (style, check, etc); each batch appends files until adding another
/// would exceed `budget` bytes of argv. Suitable for tools that don't support
/// `@file`.
pub fn run_batched(
    tool: &str,
    invoker: &Invoker,
    base_args: &[OsString],
    files: &[PathBuf],
    budget: usize,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let base_size: usize = base_args.iter().map(|a| a.len() + 1).sum();
    let chunk_budget = budget.saturating_sub(base_size).max(1);
    for chunk in chunk_files(files, chunk_budget) {
        let mut args = base_args.to_vec();
        for f in chunk {
            args.push(f.into());
        }
        run(tool, invoker, args)?;
    }
    Ok(())
}

/// Captured output from a check-mode jar invocation. The caller decides what
/// to do based on exit status; we don't surface non-zero as an error here
/// because non-zero is expected when `--set-exit-if-changed` finds diffs.
#[derive(Debug, Default)]
pub struct CheckRun {
    /// True if every batch exited zero (nothing to format, no parse errors).
    pub success: bool,
    /// File paths printed to stdout (one per line, trimmed). For ktfmt/gjf
    /// in `--dry-run` mode, these are the files that need reformatting.
    pub paths: Vec<String>,
    /// Stderr after JVM noise filtering. Typically parse errors when present.
    pub stderr: String,
}

impl CheckRun {
    fn merge(&mut self, other: CheckRun) {
        if !other.success {
            self.success = false;
        }
        self.paths.extend(other.paths);
        if !other.stderr.is_empty() {
            if !self.stderr.is_empty() {
                self.stderr.push('\n');
            }
            self.stderr.push_str(&other.stderr);
        }
    }
}

/// Check-mode counterpart to [`run`]. Captures stdout and stderr instead of
/// inheriting, returns the captured content along with exit status. Spawn
/// failures are still surfaced as `Err`.
pub fn run_check(tool: &str, invoker: &Invoker, args: Vec<OsString>) -> Result<CheckRun> {
    let mut cmd = build_command(invoker)?;
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    let output = cmd
        .output()
        .with_context(|| format!("spawn {tool} failed"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let paths: Vec<String> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    let stderr_filtered = filter_jvm_noise(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() && paths.is_empty() && stderr_filtered.is_empty() {
        return Err(anyhow!(
            "{tool} failed (exit {})",
            output.status.code().unwrap_or(-1)
        ));
    }
    Ok(CheckRun {
        success: output.status.success(),
        paths,
        stderr: stderr_filtered,
    })
}

/// Check-mode counterpart to [`run_batched`]. Aggregates captured output
/// across all chunks.
pub fn run_batched_check(
    tool: &str,
    invoker: &Invoker,
    base_args: &[OsString],
    files: &[PathBuf],
    budget: usize,
) -> Result<CheckRun> {
    let mut acc = CheckRun {
        success: true,
        ..Default::default()
    };
    if files.is_empty() {
        return Ok(acc);
    }
    let base_size: usize = base_args.iter().map(|a| a.len() + 1).sum();
    let chunk_budget = budget.saturating_sub(base_size).max(1);
    for chunk in chunk_files(files, chunk_budget) {
        let mut args = base_args.to_vec();
        for f in chunk {
            args.push(f.into());
        }
        let run = run_check(tool, invoker, args)?;
        acc.merge(run);
    }
    Ok(acc)
}

/// Check-mode counterpart to [`run_argfile`].
pub fn run_argfile_check(
    tool: &str,
    invoker: &Invoker,
    base_args: Vec<OsString>,
    files: &[PathBuf],
) -> Result<CheckRun> {
    if files.is_empty() {
        return Ok(CheckRun {
            success: true,
            ..Default::default()
        });
    }
    let argfile_arg = write_argfile(files)?;
    let mut args = base_args;
    args.push(argfile_arg.0);
    let result = run_check(tool, invoker, args);
    drop(argfile_arg.1); // keep tempfile alive until run_check returns
    result
}

/// Run `tool` once with the file list passed via `@<tempfile>`. The tempfile
/// is auto-deleted when the function returns. Suitable for tools that support
/// the `@file` argument syntax (gjf does, ktfmt does not).
pub fn run_argfile(
    tool: &str,
    invoker: &Invoker,
    base_args: Vec<OsString>,
    files: &[PathBuf],
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let argfile_arg = write_argfile(files)?;
    let mut args = base_args;
    args.push(argfile_arg.0);
    let result = run(tool, invoker, args);
    drop(argfile_arg.1);
    result
}

/// Write `files` to a tempfile (one per line). Returns the `@<path>`
/// argument and the keep-alive `NamedTempFile` (must be held by the caller
/// until the subprocess finishes, otherwise the file is deleted).
fn write_argfile(files: &[PathBuf]) -> Result<(OsString, tempfile::NamedTempFile)> {
    let mut tmp = tempfile::Builder::new()
        .prefix("kempt-files-")
        .suffix(".txt")
        .tempfile()
        .context("create argfile tempfile")?;
    for f in files {
        writeln!(tmp, "{}", f.display())
            .with_context(|| format!("write {} to argfile", f.display()))?;
    }
    tmp.flush().context("flush argfile")?;
    let mut at_arg = OsString::from("@");
    at_arg.push(tmp.path().as_os_str());
    Ok((at_arg, tmp))
}

/// Split `files` into contiguous chunks whose serialized argv size stays
/// under `budget` bytes. Each path contributes `path.len() + 1` (path bytes
/// plus a separator). A single path larger than `budget` is still emitted as
/// its own chunk; the OS will reject it if too large, but that's preferable
/// to silently skipping.
fn chunk_files(files: &[PathBuf], budget: usize) -> Vec<&[PathBuf]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut size = 0usize;
    for (i, f) in files.iter().enumerate() {
        let s = f.as_os_str().len() + 1;
        if size + s > budget && i > start {
            out.push(&files[start..i]);
            start = i;
            size = 0;
        }
        size += s;
    }
    if start < files.len() {
        out.push(&files[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &OsString) -> &str {
        v.to_str().unwrap()
    }

    // --- arg builders ---

    #[test]
    fn ktfmt_args_default_style_is_google() {
        let a = ktfmt_args(KtfmtStyle::Google, false);
        assert_eq!(s(&a[0]), "--google-style");
        assert_eq!(s(&a[1]), "--quiet");
        assert_eq!(a.len(), 2);
    }

    #[test]
    fn ktfmt_args_kotlinlang_style() {
        let a = ktfmt_args(KtfmtStyle::Kotlinlang, false);
        assert_eq!(s(&a[0]), "--kotlinlang-style");
    }

    #[test]
    fn ktfmt_args_meta_style() {
        let a = ktfmt_args(KtfmtStyle::Meta, false);
        assert_eq!(s(&a[0]), "--meta-style");
    }

    #[test]
    fn ktfmt_args_check_adds_dry_run_and_exit_flag() {
        let a = ktfmt_args(KtfmtStyle::Google, true);
        let strs: Vec<&str> = a.iter().map(s).collect();
        assert!(strs.contains(&"--dry-run"));
        assert!(strs.contains(&"--set-exit-if-changed"));
    }

    #[test]
    fn ktfmt_args_format_mode_omits_dry_run() {
        let a = ktfmt_args(KtfmtStyle::Google, false);
        let strs: Vec<&str> = a.iter().map(s).collect();
        assert!(!strs.contains(&"--dry-run"));
    }

    #[test]
    fn gjf_args_default_style_omits_aosp_flag() {
        let a = gjf_args(GjfStyle::Google, false);
        let strs: Vec<&str> = a.iter().map(s).collect();
        assert!(!strs.contains(&"--aosp"));
        assert!(strs.contains(&"--replace"));
    }

    #[test]
    fn gjf_args_aosp_includes_flag() {
        let a = gjf_args(GjfStyle::Aosp, false);
        assert_eq!(s(&a[0]), "--aosp");
    }

    #[test]
    fn gjf_args_check_uses_dry_run_not_replace() {
        let a = gjf_args(GjfStyle::Google, true);
        let strs: Vec<&str> = a.iter().map(s).collect();
        assert!(strs.contains(&"--dry-run"));
        assert!(strs.contains(&"--set-exit-if-changed"));
        assert!(!strs.contains(&"--replace"));
    }

    #[test]
    fn jvm_flags_includes_xmx_and_add_opens() {
        let f = jvm_flags();
        let strs: Vec<&str> = f.iter().map(s).collect();
        assert!(strs.iter().any(|x| x.starts_with("-Xmx")));
        assert!(strs.iter().any(|x| x.contains("java.base/java.util")));
        assert!(strs
            .iter()
            .any(|x| x.contains("jdk.compiler/com.sun.tools.javac.api")));
    }

    // --- chunker ---

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn chunk_files_empty_input_yields_no_chunks() {
        let v: Vec<PathBuf> = vec![];
        assert!(chunk_files(&v, 100).is_empty());
    }

    #[test]
    fn chunk_files_fits_in_one_chunk_under_budget() {
        let v = paths(&["a.kt", "b.kt", "c.kt"]); // ~5 bytes each
        let chunks = chunk_files(&v, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 3);
    }

    #[test]
    fn chunk_files_splits_when_budget_exceeded() {
        // Each path is "aaaaaaaaaa" (10 chars) -> 11 bytes per file.
        // Budget 22 bytes fits 2 paths per chunk.
        let v: Vec<PathBuf> = (0..5).map(|_| PathBuf::from("aaaaaaaaaa")).collect();
        let chunks = chunk_files(&v, 22);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 2);
        assert_eq!(chunks[2].len(), 1);
    }

    #[test]
    fn chunk_files_single_file_larger_than_budget_gets_own_chunk() {
        let v = paths(&["this/is/a/very/long/path/that/exceeds/budget.kt"]);
        let chunks = chunk_files(&v, 5); // budget smaller than the path
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 1);
    }

    #[test]
    fn chunk_files_preserves_order_across_chunks() {
        let v = paths(&["a", "b", "c", "d", "e"]);
        // Each path is 2 bytes. Budget 4 fits 2 paths per chunk.
        let chunks = chunk_files(&v, 4);
        let flat: Vec<&PathBuf> = chunks.iter().flat_map(|c| c.iter()).collect();
        assert_eq!(flat.len(), 5);
        assert_eq!(flat[0].as_os_str(), "a");
        assert_eq!(flat[4].as_os_str(), "e");
    }

    // --- batched runner short-circuit ---

    #[test]
    fn run_batched_with_no_files_does_not_spawn() {
        // Pass a jar path that doesn't exist. If the function spawned, we'd
        // get a different error; with empty files it should return Ok(())
        // without touching the JVM.
        let invoker = Invoker::Jar(PathBuf::from("/definitely/not/a/jar.jar"));
        let result = run_batched(
            "ktfmt",
            &invoker,
            &[OsString::from("--google-style")],
            &[],
            MAX_ARG_BYTES,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn run_argfile_with_no_files_does_not_spawn() {
        let invoker = Invoker::Jar(PathBuf::from("/definitely/not/a/jar.jar"));
        let result = run_argfile("gjf", &invoker, vec![OsString::from("--replace")], &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn run_native_with_no_files_does_not_spawn() {
        let invoker = Invoker::Native(PathBuf::from("/definitely/not/a/binary"));
        let result = run_argfile("gjf", &invoker, vec![OsString::from("--replace")], &[]);
        assert!(result.is_ok());
    }

    // --- jvm noise filter ---

    #[test]
    fn filter_jvm_noise_strips_warning_lines() {
        let raw =
            "WARNING: sun.misc.Unsafe...\nWARNING: more noise\nFoo.kt:3:11: error: Expecting ')'\n";
        let filtered = filter_jvm_noise(raw);
        assert_eq!(filtered, "Foo.kt:3:11: error: Expecting ')'");
    }

    #[test]
    fn filter_jvm_noise_keeps_real_errors_when_no_warnings() {
        let raw = "/path/to/Foo.kt:5:10: error: something\n";
        assert_eq!(
            filter_jvm_noise(raw),
            "/path/to/Foo.kt:5:10: error: something"
        );
    }

    #[test]
    fn filter_jvm_noise_returns_empty_when_only_warnings() {
        let raw = "WARNING: a\nWARNING: b\n";
        assert_eq!(filter_jvm_noise(raw), "");
    }

    #[test]
    fn filter_jvm_noise_preserves_indented_warning_in_error_text() {
        // A line that just contains "WARNING:" somewhere in the middle is
        // kept; we only filter top-of-line JVM warnings.
        let raw = "error: thing went wrong (WARNING: do not retry)\n";
        assert_eq!(
            filter_jvm_noise(raw),
            "error: thing went wrong (WARNING: do not retry)"
        );
    }
}
