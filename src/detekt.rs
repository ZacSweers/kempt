// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0

use crate::cache::{Cache, Downloader};
use crate::config::{Detekt, ResolvedDetektTarget, ToolSource};
use anyhow::{anyhow, Context, Result};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

#[derive(Debug, Clone)]
pub enum Invoker {
    Jar(PathBuf),
    Executable(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliMajor {
    V1,
    V2,
}

#[derive(Debug)]
pub struct RunResult {
    pub findings: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn resolve_invoker(
    detekt: &Detekt,
    repo_root: &Path,
    cache: &Cache,
    downloader: &dyn Downloader,
) -> Result<(Invoker, CliMajor)> {
    match detekt.source(repo_root) {
        ToolSource::Cached(version) => {
            let major = parse_major(&version)?;
            let jar = cache.ensure_detekt(&version, downloader)?;
            Ok((Invoker::Jar(jar), major))
        }
        ToolSource::Local(path) => {
            if !path.exists() {
                anyhow::bail!("detekt binary not found: {}", path.display());
            }
            let invoker = if path.extension().and_then(OsStr::to_str) == Some("jar") {
                Invoker::Jar(path)
            } else {
                Invoker::Executable(path)
            };
            let major = detect_major(&invoker)?;
            Ok((invoker, major))
        }
    }
}

pub fn run(
    invoker: &Invoker,
    major: CliMajor,
    target: &ResolvedDetektTarget,
    repo_root: &Path,
    files: &[PathBuf],
) -> Result<RunResult> {
    let args = build_args(major, target, repo_root, files)?;
    let mut command = build_command(invoker)?;
    let output = command
        .args(args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn detekt target `{}`", target.name))?;
    classify_output(target, output)
}

pub(crate) fn build_args(
    major: CliMajor,
    target: &ResolvedDetektTarget,
    repo_root: &Path,
    files: &[PathBuf],
) -> Result<Vec<OsString>> {
    let mut args: Vec<OsString> = target.args.iter().map(OsString::from).collect();
    args.push("--base-path".into());
    args.push(repo_root.into());

    if target.build_upon_default_config {
        args.push("--build-upon-default-config".into());
    }
    push_joined(&mut args, "--config", &target.configs, major)?;
    push_joined_strings(
        &mut args,
        "--config-resource",
        &target.config_resources,
        major,
    )?;
    if let Some(baseline) = &target.baseline {
        args.push("--baseline".into());
        args.push(baseline.into());
    }
    push_joined(&mut args, "--plugins", &target.plugins, major)?;
    for report in &target.reports {
        args.push("--report".into());
        args.push(report.into());
    }
    if !target.classpath.is_empty() {
        args.push("--classpath".into());
        args.push(join_platform_paths(&target.classpath)?);
    }
    if let Some(jvm_target) = &target.jvm_target {
        args.push("--jvm-target".into());
        args.push(jvm_target.into());
    }
    if major == CliMajor::V2 {
        args.push("--analysis-mode".into());
        args.push(if target.classpath.is_empty() {
            "light".into()
        } else {
            "full".into()
        });
    }

    let inputs: Vec<PathBuf> = files.iter().map(|file| repo_root.join(file)).collect();
    push_joined(&mut args, "--input", &inputs, major)?;
    Ok(args)
}

fn push_joined(
    args: &mut Vec<OsString>,
    flag: &str,
    paths: &[PathBuf],
    major: CliMajor,
) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    args.push(flag.into());
    args.push(join_cli_paths(paths, major)?);
    Ok(())
}

fn push_joined_strings(
    args: &mut Vec<OsString>,
    flag: &str,
    values: &[String],
    major: CliMajor,
) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    let paths: Vec<PathBuf> = values.iter().map(PathBuf::from).collect();
    push_joined(args, flag, &paths, major)
}

fn join_cli_paths(paths: &[PathBuf], major: CliMajor) -> Result<OsString> {
    match major {
        CliMajor::V1 => Ok(paths
            .iter()
            .map(|path| path.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join(",")
            .into()),
        CliMajor::V2 => join_platform_paths(paths),
    }
}

fn join_platform_paths(paths: &[PathBuf]) -> Result<OsString> {
    std::env::join_paths(paths).map_err(|error| anyhow!("join detekt path list failed: {error}"))
}

fn classify_output(target: &ResolvedDetektTarget, output: Output) -> Result<RunResult> {
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = filter_jvm_noise(&String::from_utf8_lossy(&output.stderr));
    match code {
        0 | 2 => Ok(RunResult {
            findings: code == 2,
            stdout,
            stderr,
        }),
        _ => {
            let details = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            let kind = match code {
                1 => "unexpected error",
                3 => "invalid configuration",
                _ => "failed",
            };
            if details.is_empty() {
                Err(anyhow!(
                    "detekt target `{}` {kind} (exit {code})",
                    target.name
                ))
            } else {
                Err(anyhow!(
                    "detekt target `{}` {kind} (exit {code}):\n{details}",
                    target.name
                ))
            }
        }
    }
}

fn filter_jvm_noise(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.trim_start().starts_with("WARNING:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn detect_major(invoker: &Invoker) -> Result<CliMajor> {
    let output = build_command(invoker)?
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("run detekt --version")?;
    if !output.status.success() {
        return Err(anyhow!("detekt --version failed"));
    }
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version = text
        .split_whitespace()
        .find(|part| part.trim_start_matches('v').starts_with(['1', '2']))
        .ok_or_else(|| {
            anyhow!(
                "could not determine detekt CLI version from `{}`",
                text.trim()
            )
        })?;
    parse_major(version)
}

fn parse_major(version: &str) -> Result<CliMajor> {
    match version.trim_start_matches('v').split('.').next() {
        Some("1") => Ok(CliMajor::V1),
        Some("2") => Ok(CliMajor::V2),
        _ => Err(anyhow!("unsupported detekt CLI version `{version}`")),
    }
}

fn build_command(invoker: &Invoker) -> Result<Command> {
    match invoker {
        Invoker::Jar(path) => {
            let java = which::which("java").map_err(|_| {
                anyhow!("`java` not found on PATH (set JAVA_HOME or install a JDK)")
            })?;
            let mut command = Command::new(java);
            command.arg("-jar").arg(path);
            Ok(command)
        }
        Invoker::Executable(path) => Ok(Command::new(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedPaths;

    fn target() -> ResolvedDetektTarget {
        ResolvedDetektTarget {
            name: "main".into(),
            configs: vec![
                PathBuf::from("/repo/config/a.yml"),
                PathBuf::from("/repo/config/b.yml"),
            ],
            config_resources: vec![],
            baseline: Some(PathBuf::from("/repo/config/baseline.xml")),
            build_upon_default_config: true,
            plugins: vec![PathBuf::from("/repo/config/rules.jar")],
            reports: vec!["html:build/reports/detekt.html".into()],
            paths: ResolvedPaths {
                include: vec!["**/*.kt".into()],
                exclude: vec![],
            },
            classpath: vec![
                PathBuf::from("/repo/build/classes"),
                PathBuf::from("/repo/lib/a.jar"),
            ],
            jvm_target: Some("17".into()),
            args: vec!["--parallel".into()],
        }
    }

    fn arg_after<'a>(args: &'a [OsString], flag: &str) -> &'a OsStr {
        let index = args.iter().position(|arg| arg == flag).unwrap();
        &args[index + 1]
    }

    #[test]
    fn v1_uses_comma_for_multi_value_cli_arguments() {
        let args = build_args(
            CliMajor::V1,
            &target(),
            Path::new("/repo"),
            &[PathBuf::from("src/A.kt"), PathBuf::from("src/B.kt")],
        )
        .unwrap();
        assert_eq!(
            arg_after(&args, "--config"),
            OsStr::new("/repo/config/a.yml,/repo/config/b.yml")
        );
        assert_eq!(
            arg_after(&args, "--input"),
            OsStr::new("/repo/src/A.kt,/repo/src/B.kt")
        );
        assert!(!args.iter().any(|arg| arg == "--analysis-mode"));
    }

    #[test]
    fn v2_uses_platform_paths_and_enables_full_analysis() {
        let args = build_args(
            CliMajor::V2,
            &target(),
            Path::new("/repo"),
            &[PathBuf::from("src/A.kt"), PathBuf::from("src/B.kt")],
        )
        .unwrap();
        let configs: Vec<PathBuf> = std::env::split_paths(arg_after(&args, "--config")).collect();
        assert_eq!(
            configs,
            vec![
                PathBuf::from("/repo/config/a.yml"),
                PathBuf::from("/repo/config/b.yml")
            ]
        );
        assert_eq!(arg_after(&args, "--analysis-mode"), OsStr::new("full"));
    }

    #[test]
    fn v2_without_classpath_uses_light_analysis() {
        let mut target = target();
        target.classpath.clear();
        target.jvm_target = None;
        let args = build_args(
            CliMajor::V2,
            &target,
            Path::new("/repo"),
            &[PathBuf::from("A.kt")],
        )
        .unwrap();
        assert_eq!(arg_after(&args, "--analysis-mode"), OsStr::new("light"));
        assert!(!args.iter().any(|arg| arg == "--classpath"));
    }

    #[test]
    fn jvm_warnings_are_removed_without_hiding_detekt_errors() {
        let stderr = "WARNING: deprecated JVM API\ne: invalid config\nWARNING: remove later\n";
        assert_eq!(filter_jvm_noise(stderr), "e: invalid config");
    }
}
