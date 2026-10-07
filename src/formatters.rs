// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Build java command lines for ktfmt and gjf, and execute them.
//!
//! ktfmt and GJF both support argument files, but with different parsing
//! rules. ktfmt treats each line as one argument. GJF splits on whitespace,
//! so paths containing whitespace fall back to direct, platform-aware batches.
//!
//! The [`Invoker`] enum abstracts over jar-via-JVM and native-binary modes
//! so callers don't have to branch.

use crate::command_args;
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
            cmd.args(jar_args(jar));
            Ok(cmd)
        }
        Invoker::Native(bin) => Ok(Command::new(bin)),
    }
}

const JVM_FLAGS: &[&str] = &[
    "-Xmx512m",
    "--disable-@files",
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

fn jar_args(jar: &std::path::Path) -> Vec<OsString> {
    let mut args = jvm_flags();
    args.push("-jar".into());
    args.push(jar.as_os_str().to_os_string());
    args
}

/// Static ktfmt flags (style, check mode). Does NOT include the file list;
/// callers include these with the paths passed to [`run_ktfmt_argfile`].
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

/// Drop known JVM warning and environment-option announcement lines from
/// captured stderr. Those are noise on every invocation and crowd out the
/// actual formatter diagnostic.
pub(crate) fn filter_jvm_noise(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            !line.starts_with("WARNING:")
                && !line.starts_with("Picked up JAVA_TOOL_OPTIONS:")
                && !line.starts_with("Picked up _JAVA_OPTIONS:")
                && !line.starts_with("NOTE: Picked up JDK_JAVA_OPTIONS:")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Run an invoker with captured output from `current_dir`. This is used by
/// formatters whose exit status and diagnostics need tool-specific handling.
pub(crate) fn run_output(
    tool: &str,
    invoker: &Invoker,
    args: &[OsString],
    current_dir: &std::path::Path,
) -> Result<std::process::Output> {
    let mut cmd = build_command(invoker)?;
    cmd.args(args)
        .current_dir(current_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn {tool} failed"))
}

fn run_batched(
    tool: &str,
    invoker: &Invoker,
    base_args: &[OsString],
    files: &[PathBuf],
) -> Result<()> {
    for chunk in command_args::path_chunks(base_args, files) {
        let mut args = base_args.to_vec();
        args.extend(chunk.iter().map(|path| path.as_os_str().to_os_string()));
        run(tool, invoker, args)?;
    }
    Ok(())
}

/// Captured output from a check-mode jar invocation. The caller decides what
/// to do based on exit status; we don't surface non-zero as an error here
/// because non-zero is expected when `--set-exit-if-changed` finds diffs.
#[derive(Debug, Default)]
pub struct CheckRun {
    /// True if the formatter exited zero (nothing to format, no parse errors).
    pub success: bool,
    /// File paths printed to stdout (one per line, trimmed). For ktfmt/gjf
    /// in `--dry-run` mode, these are the files that need reformatting.
    pub paths: Vec<String>,
    /// Stderr after JVM noise filtering. Typically parse errors when present.
    pub stderr: String,
}

impl CheckRun {
    fn merge(&mut self, other: CheckRun) {
        self.success &= other.success;
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

fn run_batched_check(
    tool: &str,
    invoker: &Invoker,
    base_args: &[OsString],
    files: &[PathBuf],
) -> Result<CheckRun> {
    let mut result = CheckRun {
        success: true,
        ..Default::default()
    };
    for chunk in command_args::path_chunks(base_args, files) {
        let mut args = base_args.to_vec();
        args.extend(chunk.iter().map(|path| path.as_os_str().to_os_string()));
        result.merge(run_check(tool, invoker, args)?);
    }
    Ok(result)
}

/// Run ktfmt once with every option and source path in an argument file.
/// ktfmt requires `@<path>` to be its only command-line argument.
pub fn run_ktfmt_argfile(
    tool: &str,
    invoker: &Invoker,
    base_args: Vec<OsString>,
    files: &[PathBuf],
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let argfile = write_argfile(&base_args, files)?;
    let result = run(tool, invoker, vec![argfile.0]);
    drop(argfile.1);
    result
}

/// Check-mode counterpart to [`run_ktfmt_argfile`].
pub fn run_ktfmt_argfile_check(
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
    let argfile = write_argfile(&base_args, files)?;
    let result = run_check(tool, invoker, vec![argfile.0]);
    drop(argfile.1);
    result
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
    if files.iter().any(|path| path_requires_direct_args(path)) {
        return run_batched_check(tool, invoker, &base_args, files);
    }
    let argfile_arg = write_argfile(&base_args, files)?;
    let result = run_check(tool, invoker, vec![argfile_arg.0]);
    drop(argfile_arg.1); // keep tempfile alive until run_check returns
    result
}

/// Run GJF once with its options and files passed via `@<tempfile>`. GJF
/// splits argument-file contents on whitespace, so paths that cannot be
/// represented safely use direct, platform-aware batches instead.
pub fn run_argfile(
    tool: &str,
    invoker: &Invoker,
    base_args: Vec<OsString>,
    files: &[PathBuf],
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    if files.iter().any(|path| path_requires_direct_args(path)) {
        return run_batched(tool, invoker, &base_args, files);
    }
    let argfile_arg = write_argfile(&base_args, files)?;
    let result = run(tool, invoker, vec![argfile_arg.0]);
    drop(argfile_arg.1);
    result
}

fn path_requires_direct_args(path: &std::path::Path) -> bool {
    match path.to_str() {
        Some(path) => path.chars().any(char::is_whitespace),
        None => true,
    }
}

/// Write `args` and `files` to a tempfile, one item per line. ktfmt preserves
/// line boundaries. GJF callers ensure no item contains whitespace because
/// its parser treats all breaking whitespace as a separator.
fn write_argfile(
    args: &[OsString],
    files: &[PathBuf],
) -> Result<(OsString, tempfile::NamedTempFile)> {
    let mut tmp = tempfile::Builder::new()
        .prefix("kempt-files-")
        .suffix(".txt")
        .tempfile()
        .context("create argfile tempfile")?;
    for arg in args {
        writeln!(tmp, "{}", arg.to_string_lossy()).context("write option to argfile")?;
    }
    for f in files {
        writeln!(tmp, "{}", f.display())
            .with_context(|| format!("write {} to argfile", f.display()))?;
    }
    tmp.flush().context("flush argfile")?;
    let mut at_arg = OsString::from("@");
    at_arg.push(tmp.path().as_os_str());
    Ok((at_arg, tmp))
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

    #[test]
    fn jar_invocation_disables_java_launcher_argfile_expansion() {
        let args = jar_args(std::path::Path::new("formatter.jar"));
        let strs: Vec<&str> = args.iter().map(s).collect();
        let disable_argfiles = strs
            .iter()
            .position(|arg| *arg == "--disable-@files")
            .unwrap();
        let jar = strs.iter().position(|arg| *arg == "-jar").unwrap();

        assert!(disable_argfiles < jar);
        assert_eq!(strs[jar + 1], "formatter.jar");
    }

    #[test]
    fn run_ktfmt_argfile_with_no_files_does_not_spawn() {
        // Pass a jar path that doesn't exist. If the function spawned, we'd
        // get a different error; with empty files it should return Ok(())
        // without touching the JVM.
        let invoker = Invoker::Jar(PathBuf::from("/definitely/not/a/jar.jar"));
        let result = run_ktfmt_argfile(
            "ktfmt",
            &invoker,
            vec![OsString::from("--google-style")],
            &[],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn ktfmt_argfile_contains_options_and_paths() {
        let args = vec![OsString::from("--google-style"), OsString::from("--quiet")];
        let files = vec![
            PathBuf::from("src/Foo.kt"),
            PathBuf::from("src/Foo Bar.kts"),
        ];

        let (at_arg, tempfile) = write_argfile(&args, &files).unwrap();

        assert!(at_arg.to_string_lossy().starts_with('@'));
        assert_eq!(
            std::fs::read_to_string(tempfile.path()).unwrap(),
            "--google-style\n--quiet\nsrc/Foo.kt\nsrc/Foo Bar.kts\n"
        );
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

    #[cfg(unix)]
    #[test]
    fn gjf_path_with_whitespace_uses_direct_arguments() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Foo Bar.java");
        std::fs::write(&source, "class Foo {}\n").unwrap();
        let formatter = dir.path().join("fake-gjf");
        std::fs::write(
            &formatter,
            "#!/bin/sh\n\
             for arg in \"$@\"; do\n\
               case \"$arg\" in\n\
                 @*) exit 9 ;;\n\
                 *.java) printf '// formatted\\n' >> \"$arg\" ;;\n\
               esac\n\
             done\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&formatter).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&formatter, permissions).unwrap();

        run_argfile(
            "gjf",
            &Invoker::Native(formatter),
            vec![OsString::from("--replace")],
            std::slice::from_ref(&source),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(source).unwrap(),
            "class Foo {}\n// formatted\n"
        );
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

    #[test]
    fn filter_jvm_noise_strips_environment_option_announcements() {
        let raw = "Picked up JAVA_TOOL_OPTIONS: -Dfile.encoding=UTF-8\n\
                   Picked up _JAVA_OPTIONS: -Xmx1g\n\
                   NOTE: Picked up JDK_JAVA_OPTIONS: --enable-preview\n";
        assert_eq!(filter_jvm_noise(raw), "");
    }

    #[test]
    fn filter_jvm_noise_preserves_errors_alongside_environment_announcements() {
        let raw = "Picked up JAVA_TOOL_OPTIONS: -XX:BadOption\n\
                   Unrecognized VM option 'BadOption'\n\
                   Error: Could not create the Java Virtual Machine.\n";
        assert_eq!(
            filter_jvm_noise(raw),
            "Unrecognized VM option 'BadOption'\nError: Could not create the Java Virtual Machine."
        );
    }
}
