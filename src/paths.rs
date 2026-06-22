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
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// All tracked files in the repo (`git ls-files`).
    All,
    /// Files in the index (staged for commit).
    Staged,
    /// Filesystem walk from the repo root. Ignore files (`.gitignore` etc.)
    /// are NOT consulted. The `.git/` directory is always pruned.
    Walk,
    /// User-supplied list of paths. Bypasses every filter; the literal list
    /// is what gets processed.
    Explicit(Vec<PathBuf>),
}

/// Collect the candidate file set for `scope` without applying any filters.
pub fn collect_universe(git: &dyn GitContext, scope: Scope) -> Result<Vec<PathBuf>> {
    match scope {
        Scope::All => git.ls_files(),
        Scope::Staged => git.staged_files(),
        Scope::Walk => walk_tree(git.root()),
        Scope::Explicit(files) => Ok(files),
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

/// Walk the working tree below `root`, returning paths relative to `root`.
/// Always prunes `.git/` to avoid descending into git internals.
fn walk_tree(root: &Path) -> Result<Vec<PathBuf>> {
    let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
        // Hardcoded structural prune; never user-overridable.
        !(e.depth() > 0 && e.file_type().is_dir() && e.file_name() == std::ffi::OsStr::new(".git"))
    });
    let mut out = Vec::new();
    for entry in walker {
        let entry = entry.context("walk error")?;
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
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
        let out = collect_universe(&git, Scope::Explicit(explicit.clone())).unwrap();
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
}
