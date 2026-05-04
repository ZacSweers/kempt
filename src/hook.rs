//! Pre-commit hook safety checks and installer.
//!
//! The "format on commit and re-stage" flow is safe only when the staged
//! files have no further unstaged modifications. If a user has partial
//! staging on any file we'd format, we'd silently pull unstaged hunks into
//! the commit when we re-add. We bail in that case.

use crate::git::GitContext;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Result of checking for partial staging on the candidate file set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagingCheck {
    Safe,
    PartialStage { files: Vec<PathBuf> },
}

/// Files that are both in the staged set and the unstaged-modified set.
/// `candidates` should be the files we're about to format (already filtered
/// by glob).
pub fn check_partial_staging(git: &dyn GitContext, candidates: &[PathBuf]) -> Result<StagingCheck> {
    if candidates.is_empty() {
        return Ok(StagingCheck::Safe);
    }
    let unstaged: HashSet<PathBuf> = git
        .unstaged_modified_files()
        .context("query unstaged-modified files")?
        .into_iter()
        .collect();
    let mut conflicts: Vec<PathBuf> = candidates
        .iter()
        .filter(|p| unstaged.contains(*p))
        .cloned()
        .collect();
    conflicts.sort();
    if conflicts.is_empty() {
        Ok(StagingCheck::Safe)
    } else {
        Ok(StagingCheck::PartialStage { files: conflicts })
    }
}

/// Format the partial-stage error message into something actionable.
pub fn format_partial_stage_error(files: &[PathBuf]) -> String {
    let mut s = String::from("kempt: partial staging detected on files we'd format:\n");
    for f in files {
        s.push_str("  ");
        s.push_str(&f.display().to_string());
        s.push('\n');
    }
    s.push_str(
        "formatting and re-staging would pull unstaged hunks into the commit.\n\
         options:\n  - stage the rest (git add ...)\n  - stash unstaged (git stash -k)\n  - bypass the hook (git commit --no-verify)\n",
    );
    s
}

const HOOK_BODY: &str = "#!/bin/sh\nexec kempt hook \"$@\"\n";

/// Write a `.git/hooks/pre-commit` that delegates to `kempt hook`.
/// Refuses to overwrite an existing hook unless `force` is true.
pub fn install_pre_commit(git_dir: &Path, force: bool) -> Result<PathBuf> {
    let hooks_dir = git_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)
        .with_context(|| format!("create {}", hooks_dir.display()))?;
    let dest = hooks_dir.join("pre-commit");
    if dest.exists() && !force {
        anyhow::bail!(
            "pre-commit hook already exists at {} (re-run with --force to overwrite)",
            dest.display()
        );
    }
    std::fs::write(&dest, HOOK_BODY).with_context(|| format!("write {}", dest.display()))?;
    set_executable(&dest)?;
    Ok(dest)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::FakeGit;

    #[test]
    fn check_partial_staging_safe_when_no_overlap() {
        let git = FakeGit::new("/r")
            .with_staged(vec!["a.kt"])
            .with_unstaged(vec!["b.kt"]);
        let candidates = vec![PathBuf::from("a.kt")];
        assert_eq!(
            check_partial_staging(&git, &candidates).unwrap(),
            StagingCheck::Safe
        );
    }

    #[test]
    fn check_partial_staging_flags_overlapping_files() {
        let git = FakeGit::new("/r")
            .with_staged(vec!["a.kt", "b.kt"])
            .with_unstaged(vec!["a.kt"]);
        let candidates = vec![PathBuf::from("a.kt"), PathBuf::from("b.kt")];
        match check_partial_staging(&git, &candidates).unwrap() {
            StagingCheck::PartialStage { files } => {
                assert_eq!(files, vec![PathBuf::from("a.kt")]);
            }
            other => panic!("expected PartialStage, got {other:?}"),
        }
    }

    #[test]
    fn check_partial_staging_ignores_unstaged_files_outside_candidates() {
        // file is unstaged-modified but not in our candidate set → not a conflict
        let git = FakeGit::new("/r").with_unstaged(vec!["unrelated.kt"]);
        let candidates = vec![PathBuf::from("a.kt")];
        assert_eq!(
            check_partial_staging(&git, &candidates).unwrap(),
            StagingCheck::Safe
        );
    }

    #[test]
    fn check_partial_staging_empty_candidates_is_safe() {
        let git = FakeGit::new("/r").with_unstaged(vec!["a.kt"]);
        assert_eq!(
            check_partial_staging(&git, &[]).unwrap(),
            StagingCheck::Safe
        );
    }

    #[test]
    fn check_partial_staging_results_are_sorted_for_stable_output() {
        let git = FakeGit::new("/r")
            .with_staged(vec!["c.kt", "a.kt", "b.kt"])
            .with_unstaged(vec!["c.kt", "a.kt", "b.kt"]);
        let candidates = vec![
            PathBuf::from("c.kt"),
            PathBuf::from("a.kt"),
            PathBuf::from("b.kt"),
        ];
        match check_partial_staging(&git, &candidates).unwrap() {
            StagingCheck::PartialStage { files } => {
                assert_eq!(
                    files,
                    vec![
                        PathBuf::from("a.kt"),
                        PathBuf::from("b.kt"),
                        PathBuf::from("c.kt")
                    ]
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn format_partial_stage_error_lists_files_and_options() {
        let msg = format_partial_stage_error(&[PathBuf::from("a.kt"), PathBuf::from("b.kt")]);
        assert!(msg.contains("a.kt"));
        assert!(msg.contains("b.kt"));
        assert!(msg.contains("--no-verify"));
        assert!(msg.contains("git stash"));
    }

    #[test]
    fn install_pre_commit_writes_executable_hook() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let path = install_pre_commit(&git_dir, false).unwrap();
        assert!(path.exists());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("kempt hook"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "hook should be executable");
        }
    }

    #[test]
    fn install_pre_commit_refuses_to_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        let hooks = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let existing = hooks.join("pre-commit");
        std::fs::write(&existing, "# my custom hook\n").unwrap();
        let err = install_pre_commit(&git_dir, false).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));
        // existing content preserved
        assert_eq!(
            std::fs::read_to_string(&existing).unwrap(),
            "# my custom hook\n"
        );
    }

    #[test]
    fn install_pre_commit_force_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        let hooks = git_dir.join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let existing = hooks.join("pre-commit");
        std::fs::write(&existing, "# old\n").unwrap();
        install_pre_commit(&git_dir, true).unwrap();
        let body = std::fs::read_to_string(&existing).unwrap();
        assert!(body.contains("kempt hook"));
    }
}
