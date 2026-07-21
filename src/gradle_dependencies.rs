// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Gradle Dependencies Sorter CLI integration.

use crate::formatters::{self, Invoker};
use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const TOOL: &str = "Gradle Dependencies Sorter";

#[derive(Debug, Default)]
pub struct RunOutcome {
    pub changed: BTreeSet<PathBuf>,
    pub errors: String,
}

/// Run the sorter on repository-relative `files`.
///
/// Check mode mirrors the selected files into a temporary tree and sorts the
/// copies. Comparing those copies gives Kempt exact changed paths without
/// parsing the upstream CLI's human-oriented check output.
pub fn run(
    invoker: &Invoker,
    insert_blank_lines: bool,
    repo_root: &Path,
    files: &[PathBuf],
    check: bool,
) -> Result<RunOutcome> {
    if files.is_empty() {
        return Ok(RunOutcome::default());
    }
    if check {
        run_check(invoker, insert_blank_lines, repo_root, files)
    } else {
        run_format(invoker, insert_blank_lines, repo_root, files)
    }
}

fn run_format(
    invoker: &Invoker,
    insert_blank_lines: bool,
    repo_root: &Path,
    files: &[PathBuf],
) -> Result<RunOutcome> {
    let before = read_files(repo_root, files)?;
    let absolute: Vec<PathBuf> = files.iter().map(|p| repo_root.join(p)).collect();
    let errors = run_cli(invoker, insert_blank_lines, repo_root, &absolute)?;
    if !errors.is_empty() {
        return Err(anyhow!("{TOOL} failed:\n{errors}"));
    }
    Ok(RunOutcome {
        changed: compare_files(repo_root, files, &before)?,
        errors: String::new(),
    })
}

fn run_check(
    invoker: &Invoker,
    insert_blank_lines: bool,
    repo_root: &Path,
    files: &[PathBuf],
) -> Result<RunOutcome> {
    let temp = tempfile::Builder::new()
        .prefix("kempt-gradle-dependencies-")
        .tempdir()
        .context("create Gradle Dependencies Sorter check directory")?;
    for rel in files {
        let source = repo_root.join(rel);
        let target = temp.path().join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(&source, &target)
            .with_context(|| format!("copy {} for checking", source.display()))?;
    }

    let before = read_files(temp.path(), files)?;
    let absolute: Vec<PathBuf> = files.iter().map(|p| temp.path().join(p)).collect();
    let errors = run_cli(invoker, insert_blank_lines, temp.path(), &absolute)?.replace(
        &temp.path().display().to_string(),
        &repo_root.display().to_string(),
    );
    Ok(RunOutcome {
        changed: compare_files(temp.path(), files, &before)?,
        errors,
    })
}

fn run_cli(
    invoker: &Invoker,
    insert_blank_lines: bool,
    current_dir: &Path,
    files: &[PathBuf],
) -> Result<String> {
    let base = args(insert_blank_lines);
    let base_size: usize = base.iter().map(|a| a.len() + 1).sum();
    let budget = formatters::MAX_ARG_BYTES.saturating_sub(base_size).max(1);
    let mut errors = Vec::new();

    for chunk in formatters::chunk_files(files, budget) {
        let mut invocation = base.clone();
        invocation.extend(chunk.iter().map(|p| p.as_os_str().to_os_string()));
        let output = formatters::run_output(TOOL, invoker, &invocation, current_dir)?;
        if output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = formatters::filter_jvm_noise(&String::from_utf8_lossy(&output.stderr));
        let details = [stdout.trim(), stderr.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if details.is_empty() {
            errors.push(format!("exit {}", output.status.code().unwrap_or(-1)));
        } else {
            errors.push(details);
        }
    }
    Ok(errors.join("\n"))
}

fn args(insert_blank_lines: bool) -> Vec<OsString> {
    if insert_blank_lines {
        Vec::new()
    } else {
        vec![OsString::from("--no-blank-lines")]
    }
}

fn read_files(root: &Path, files: &[PathBuf]) -> Result<Vec<Vec<u8>>> {
    files
        .iter()
        .map(|rel| {
            let path = root.join(rel);
            std::fs::read(&path).with_context(|| format!("read {}", path.display()))
        })
        .collect()
}

fn compare_files(root: &Path, files: &[PathBuf], before: &[Vec<u8>]) -> Result<BTreeSet<PathBuf>> {
    let mut changed = BTreeSet::new();
    for (rel, old) in files.iter().zip(before) {
        let path = root.join(rel);
        let new = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        if &new != old {
            changed.insert(rel.clone());
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_sorter(root: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join("fake-gradle-dependencies-sorter");
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             status=0\n\
             for file in \"$@\"; do\n\
               [ \"$file\" = \"--no-blank-lines\" ] && continue\n\
               if grep -q PARSE_ERROR \"$file\"; then\n\
                 echo \"Parsing error: $file\" >&2\n\
                 status=3\n\
               elif grep -q UNSORTED \"$file\"; then\n\
                 printf '\\n// sorted\\n' >> \"$file\"\n\
               fi\n\
             done\n\
             exit $status\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[test]
    fn blank_lines_enabled_needs_no_flag() {
        assert!(args(true).is_empty());
    }

    #[test]
    fn blank_lines_disabled_passes_upstream_flag() {
        assert_eq!(args(false), vec![OsString::from("--no-blank-lines")]);
    }

    #[cfg(unix)]
    #[test]
    fn format_changes_only_unsorted_build_scripts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("build.gradle.kts"), "UNSORTED\n").unwrap();
        std::fs::write(dir.path().join("clean.gradle"), "SORTED\n").unwrap();
        let invoker = Invoker::Native(fake_sorter(dir.path()));
        let files = vec![
            PathBuf::from("build.gradle.kts"),
            PathBuf::from("clean.gradle"),
        ];

        let outcome = run(&invoker, true, dir.path(), &files, false).unwrap();

        assert_eq!(
            outcome.changed,
            BTreeSet::from([PathBuf::from("build.gradle.kts")])
        );
        assert!(outcome.errors.is_empty());
        assert!(std::fs::read_to_string(dir.path().join("build.gradle.kts"))
            .unwrap()
            .contains("// sorted"));
    }

    #[cfg(unix)]
    #[test]
    fn check_reports_exact_changes_without_writing_repository_files() {
        let dir = tempfile::tempdir().unwrap();
        let original = "UNSORTED\n";
        std::fs::write(dir.path().join("build.gradle.kts"), original).unwrap();
        std::fs::write(dir.path().join("clean.gradle"), "SORTED\n").unwrap();
        let invoker = Invoker::Native(fake_sorter(dir.path()));
        let files = vec![
            PathBuf::from("build.gradle.kts"),
            PathBuf::from("clean.gradle"),
        ];

        let outcome = run(&invoker, true, dir.path(), &files, true).unwrap();

        assert_eq!(
            outcome.changed,
            BTreeSet::from([PathBuf::from("build.gradle.kts")])
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("build.gradle.kts")).unwrap(),
            original
        );
    }

    #[cfg(unix)]
    #[test]
    fn check_preserves_parse_errors_and_rewrites_temporary_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.gradle"), "PARSE_ERROR\n").unwrap();
        let invoker = Invoker::Native(fake_sorter(dir.path()));

        let outcome = run(
            &invoker,
            true,
            dir.path(),
            &[PathBuf::from("broken.gradle")],
            true,
        )
        .unwrap();

        assert!(outcome.changed.is_empty());
        assert!(outcome
            .errors
            .contains(&dir.path().join("broken.gradle").display().to_string()));
        assert!(!outcome.errors.contains("kempt-gradle-dependencies-"));
    }
}
