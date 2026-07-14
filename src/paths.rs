// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! File collection.
//!
//! The collect step happens in two phases:
//! 1. [`collect_universe`] returns every candidate path for the chosen
//!    [`Scope`] (git tracked, staged, walk, or an explicit list). No
//!    filtering yet.
//! 2. [`apply_global_excludes`] applies the universal `[paths].exclude`,
//!    yielding the set of paths kempt is allowed to touch.
//!
//! Per-tool include/exclude is then applied at the use site via
//! [`tool_globset`] and a simple match check.

use crate::config::ResolvedPaths;
use crate::git::GitContext;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// All tracked files in the repo (`git ls-files`).
    All,
    /// Files in the index (staged for commit).
    Staged,
    /// Filesystem walk from the repo root. Ignore files (`.gitignore` etc.)
    /// are NOT consulted. The `.git/` directory is always pruned.
    Walk,
    /// Files expanded from user-supplied positional targets. Global and
    /// per-tool path excludes apply unless `force` is set; per-tool includes
    /// always apply later.
    Explicit { files: Vec<PathBuf>, force: bool },
}

/// Resolve positional CLI targets to repo-relative files.
///
/// Literal files are kept as-is, directories are walked recursively, and
/// glob patterns are matched against the working tree. Relative targets are
/// interpreted against `cwd`; targets outside `repo_root` are rejected.
pub fn resolve_explicit_targets(
    targets: &[PathBuf],
    cwd: &Path,
    repo_root: &Path,
) -> Result<Vec<PathBuf>> {
    let canonical_root = repo_root
        .canonicalize()
        .with_context(|| format!("canonicalize {}", repo_root.display()))?;
    let canonical_cwd = cwd
        .canonicalize()
        .with_context(|| format!("canonicalize {}", cwd.display()))?;
    canonical_cwd
        .strip_prefix(&canonical_root)
        .with_context(|| {
            format!(
                "current directory {} is outside the repo root",
                cwd.display()
            )
        })?;

    let mut resolved = BTreeSet::new();
    let mut working_tree_files: Option<Vec<PathBuf>> = None;

    for target in targets {
        let absolute = if target.is_absolute() {
            target.clone()
        } else {
            canonical_cwd.join(target)
        };

        if absolute.exists() {
            let canonical = absolute
                .canonicalize()
                .with_context(|| format!("canonicalize {}", target.display()))?;
            let relative = canonical
                .strip_prefix(&canonical_root)
                .with_context(|| format!("path {} is outside the repo root", target.display()))?
                .to_path_buf();

            if canonical.is_file() {
                resolved.insert(relative);
            } else if canonical.is_dir() {
                resolved.extend(walk_tree(&canonical, &canonical_root)?);
            } else {
                anyhow::bail!("path is not a file or directory: {}", target.display());
            }
            continue;
        }

        if !contains_glob_meta(target) {
            anyhow::bail!("path not found: {}", target.display());
        }

        let normalized = normalize_pattern(&absolute)?;
        let relative_pattern = normalized
            .strip_prefix(&canonical_root)
            .with_context(|| format!("pattern {} is outside the repo root", target.display()))?;
        let pattern = relative_pattern
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is not valid UTF-8: {}", target.display()))?;
        let matcher = Glob::new(pattern)
            .with_context(|| format!("invalid target pattern: {}", target.display()))?
            .compile_matcher();
        let files = match &working_tree_files {
            Some(files) => files,
            None => working_tree_files.insert(walk_tree(&canonical_root, &canonical_root)?),
        };
        let mut matched = false;
        for file in files {
            if matcher.is_match(file) {
                matched = true;
                resolved.insert(file.clone());
            }
        }
        if !matched {
            anyhow::bail!("pattern matched no files: {}", target.display());
        }
    }

    Ok(resolved.into_iter().collect())
}

fn contains_glob_meta(path: &Path) -> bool {
    path.to_string_lossy()
        .chars()
        .any(|c| matches!(c, '*' | '?' | '[' | '{'))
}

/// Normalize `.` and `..` without requiring the globbed path to exist.
fn normalize_pattern(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    anyhow::bail!("pattern escapes the filesystem root: {}", path.display());
                }
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

/// Collect the candidate file set for `scope` without applying any filters.
pub fn collect_universe(git: &dyn GitContext, scope: Scope) -> Result<Vec<PathBuf>> {
    match scope {
        Scope::All => git.ls_files(),
        Scope::Staged => git.staged_files(),
        Scope::Walk => walk_tree(git.root(), git.root()),
        Scope::Explicit { files, .. } => Ok(files),
    }
}

/// Apply the universal `[paths].exclude` globset, returning the surviving
/// paths.
pub fn apply_global_excludes(files: Vec<PathBuf>, exclude: &GlobSet) -> Vec<PathBuf> {
    files.into_iter().filter(|p| !exclude.is_match(p)).collect()
}

/// Build (include, exclude) globsets from a [`ResolvedPaths`].
pub fn tool_globset(rp: &ResolvedPaths) -> Result<(GlobSet, GlobSet)> {
    let include = build_globset(&rp.include).context("invalid include glob")?;
    let exclude = build_globset(&rp.exclude).context("invalid exclude glob")?;
    Ok((include, exclude))
}

/// Walk below `root`, returning paths relative to `repo_root`.
/// Always prunes `.git/` to avoid descending into git internals.
fn walk_tree(root: &Path, repo_root: &Path) -> Result<Vec<PathBuf>> {
    let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
        // Hardcoded structural prune; never user-overridable.
        !(e.file_type().is_dir() && e.file_name() == std::ffi::OsStr::new(".git"))
    });
    let mut out = Vec::new();
    for entry in walker {
        let entry = entry.context("walk error")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(repo_root)
            .with_context(|| format!("strip_prefix on {}", entry.path().display()))?;
        out.push(rel.to_path_buf());
    }
    Ok(out)
}

pub fn build_globset(patterns: &[String]) -> Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let glob = Glob::new(p).with_context(|| format!("bad glob pattern: {p}"))?;
        b.add(glob);
    }
    b.build().context("failed to build globset")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::FakeGit;

    fn globs(patterns: &[&str]) -> GlobSet {
        let strs: Vec<String> = patterns.iter().map(|s| (*s).to_string()).collect();
        build_globset(&strs).unwrap()
    }

    #[test]
    fn collect_universe_returns_all_tracked_for_all_scope() {
        let git = FakeGit::new("/repo").with_tracked(vec!["a.kt", "b.java", "README.md"]);
        let mut out = collect_universe(&git, Scope::All).unwrap();
        out.sort();
        assert_eq!(
            out,
            vec![
                PathBuf::from("README.md"),
                PathBuf::from("a.kt"),
                PathBuf::from("b.java")
            ]
        );
    }

    #[test]
    fn collect_universe_returns_staged_for_staged_scope() {
        let git = FakeGit::new("/repo")
            .with_tracked(vec!["a.kt", "b.kt"])
            .with_staged(vec!["a.kt"]);
        let out = collect_universe(&git, Scope::Staged).unwrap();
        assert_eq!(out, vec![PathBuf::from("a.kt")]);
    }

    #[test]
    fn collect_universe_returns_explicit_unmodified() {
        let git = FakeGit::new("/repo");
        let explicit = vec![PathBuf::from("a.kt"), PathBuf::from("not-source.txt")];
        let out = collect_universe(
            &git,
            Scope::Explicit {
                files: explicit.clone(),
                force: false,
            },
        )
        .unwrap();
        assert_eq!(out, explicit);
    }

    #[test]
    fn apply_global_excludes_drops_matching_paths() {
        let exclude = globs(&["**/build/**", "**/target/**"]);
        let files = vec![
            PathBuf::from("src/Foo.kt"),
            PathBuf::from("build/Generated.kt"),
            PathBuf::from("target/classes/Bar.java"),
        ];
        let out = apply_global_excludes(files, &exclude);
        assert_eq!(out, vec![PathBuf::from("src/Foo.kt")]);
    }

    #[test]
    fn apply_global_excludes_no_match_keeps_everything() {
        let exclude = globs(&["**/build/**"]);
        let files = vec![PathBuf::from("a.kt"), PathBuf::from("b.kt")];
        let out = apply_global_excludes(files.clone(), &exclude);
        assert_eq!(out, files);
    }

    #[test]
    fn tool_globset_builds_include_and_exclude() {
        let rp = ResolvedPaths {
            include: vec!["**/*.kt".into(), "**/*.kts".into()],
            exclude: vec!["**/Skip.kt".into()],
        };
        let (inc, exc) = tool_globset(&rp).unwrap();
        assert!(inc.is_match("a/b/Foo.kt"));
        assert!(inc.is_match("build.gradle.kts"));
        assert!(!inc.is_match("Bar.java"));
        assert!(exc.is_match("a/Skip.kt"));
        assert!(!exc.is_match("a/Keep.kt"));
    }

    #[test]
    fn build_globset_rejects_invalid_pattern() {
        let r = build_globset(&["[bad".to_string()]);
        assert!(r.is_err());
    }

    fn write_file(root: &Path, rel: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, "").unwrap();
    }

    #[test]
    fn walk_returns_files_regardless_of_git_state() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/Foo.kt");
        write_file(dir.path(), "README.md");
        let git = FakeGit::new(dir.path().to_path_buf());
        let mut out = collect_universe(&git, Scope::Walk).unwrap();
        out.sort();
        assert_eq!(
            out,
            vec![PathBuf::from("README.md"), PathBuf::from("src/Foo.kt")]
        );
    }

    #[test]
    fn walk_prunes_dot_git_directory() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), ".git/HEAD");
        write_file(dir.path(), ".git/hooks/Skipped.kt");
        write_file(dir.path(), "Real.kt");
        let git = FakeGit::new(dir.path().to_path_buf());
        let out = collect_universe(&git, Scope::Walk).unwrap();
        assert_eq!(out, vec![PathBuf::from("Real.kt")]);
    }

    #[test]
    fn walk_does_not_consult_gitignore() {
        // The point of walk mode: kempt does NOT respect .gitignore. A user
        // who wants to exclude something uses [paths].exclude or per-tool
        // exclude.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        write_file(dir.path(), "ignored/Foo.kt");
        write_file(dir.path(), "Bar.kt");
        let git = FakeGit::new(dir.path().to_path_buf());
        let out = collect_universe(&git, Scope::Walk).unwrap();
        // Both Bar.kt and ignored/Foo.kt are present (gitignore not consulted).
        assert!(out.iter().any(|p| p == &PathBuf::from("Bar.kt")));
        assert!(out.iter().any(|p| p == &PathBuf::from("ignored/Foo.kt")));
    }

    #[test]
    fn explicit_directory_recurses_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/Foo.kt");
        write_file(dir.path(), "src/nested/Bar.java");
        write_file(dir.path(), "src/.git/Hidden.kt");
        write_file(dir.path(), "other/Baz.kt");

        let out = resolve_explicit_targets(
            &[PathBuf::from("src"), PathBuf::from("src/Foo.kt")],
            dir.path(),
            dir.path(),
        )
        .unwrap();

        assert_eq!(
            out,
            vec![
                PathBuf::from("src/Foo.kt"),
                PathBuf::from("src/nested/Bar.java")
            ]
        );
    }

    #[test]
    fn explicit_file_resolves_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/Foo.kt");

        let out = resolve_explicit_targets(
            &[PathBuf::from("Foo.kt")],
            &dir.path().join("src"),
            dir.path(),
        )
        .unwrap();

        assert_eq!(out, vec![PathBuf::from("src/Foo.kt")]);
    }

    #[test]
    fn explicit_pattern_matches_files_relative_to_cwd() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/Foo.kt");
        write_file(dir.path(), "src/nested/Bar.kt");
        write_file(dir.path(), "src/nested/Bar.java");
        std::fs::create_dir_all(dir.path().join("module")).unwrap();

        let out = resolve_explicit_targets(
            &[PathBuf::from("../src/**/*.kt")],
            &dir.path().join("module"),
            dir.path(),
        )
        .unwrap();

        assert_eq!(
            out,
            vec![
                PathBuf::from("src/Foo.kt"),
                PathBuf::from("src/nested/Bar.kt")
            ]
        );
    }

    #[test]
    fn explicit_pattern_errors_when_nothing_matches() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/Foo.kt");

        let err =
            resolve_explicit_targets(&[PathBuf::from("src/**/*.java")], dir.path(), dir.path())
                .unwrap_err();

        assert!(format!("{err:#}").contains("pattern matched no files"));
    }

    #[test]
    fn explicit_target_rejects_paths_outside_repo() {
        let repo = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();

        let err =
            resolve_explicit_targets(&[outside.path().to_path_buf()], repo.path(), repo.path())
                .unwrap_err();

        assert!(format!("{err:#}").contains("outside the repo root"));
    }
}
