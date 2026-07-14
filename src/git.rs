// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Git wrapper. Trait-based so paths/hook logic can be tested without a repo.

use anyhow::{anyhow, Context, Result};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Operations kempt needs from git. Paths returned are always relative to the
/// repo root.
pub trait GitContext {
    fn root(&self) -> &Path;
    fn ls_files(&self) -> Result<Vec<PathBuf>>;
    fn staged_files(&self) -> Result<Vec<PathBuf>>;
    fn touched_files(&self, base: Option<&str>) -> Result<Vec<PathBuf>>;
    fn unstaged_modified_files(&self) -> Result<Vec<PathBuf>>;
    /// Stage paths, including tracked files hidden by ignore rules.
    fn add(&self, paths: &[PathBuf]) -> Result<()>;
    fn staged_diff(&self, _path: &Path, _context: u32) -> Result<String> {
        Err(anyhow!("staged diff is not supported by this git context"))
    }
    fn read_staged_file(&self, _path: &Path) -> Result<Vec<u8>> {
        Err(anyhow!(
            "reading staged files is not supported by this git context"
        ))
    }
    fn update_staged_file(&self, _path: &Path, _contents: &[u8]) -> Result<()> {
        Err(anyhow!(
            "updating staged files is not supported by this git context"
        ))
    }
}

pub struct RealGit {
    root: PathBuf,
}

impl RealGit {
    /// Discover the repo root by running `git rev-parse --show-toplevel` in
    /// `start`.
    pub fn discover(start: &Path) -> Result<Self> {
        let out = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(start)
            .output()
            .context("failed to run git rev-parse")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!("not in a git repo: {}", stderr.trim()));
        }
        let root_str = String::from_utf8(out.stdout)
            .context("git rev-parse output not utf8")?
            .trim()
            .to_string();
        Ok(Self {
            root: PathBuf::from(root_str),
        })
    }

    fn run(&self, args: &[&str]) -> Result<Vec<PathBuf>> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git {} failed to spawn", args.join(" ")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!("git {} failed: {}", args.join(" "), stderr.trim()));
        }
        Ok(String::from_utf8(out.stdout)
            .context("git output not utf8")?
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect())
    }

    fn symbolic_ref_target(&self, reference: &str) -> Result<Option<String>> {
        let out = Command::new("git")
            .args(["symbolic-ref", "--quiet", "--short", reference])
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git symbolic-ref {reference} failed to spawn"))?;
        if !out.status.success() {
            return Ok(None);
        }
        Ok(Some(
            String::from_utf8(out.stdout)
                .context("git symbolic-ref output not utf8")?
                .trim()
                .to_string(),
        ))
    }

    fn ref_exists(&self, reference: &str) -> Result<bool> {
        let output = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", reference])
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git rev-parse {reference} failed to spawn"))?;
        Ok(output.status.success())
    }

    fn infer_touched_base(&self) -> Result<String> {
        if let Some(reference) = self.symbolic_ref_target("refs/remotes/origin/HEAD")? {
            return Ok(reference);
        }

        let remote_heads = Command::new("git")
            .args(["for-each-ref", "--format=%(refname)", "refs/remotes/*/HEAD"])
            .current_dir(&self.root)
            .output()
            .context("git for-each-ref failed to spawn")?;
        if remote_heads.status.success() {
            for reference in String::from_utf8(remote_heads.stdout)
                .context("git for-each-ref output not utf8")?
                .lines()
            {
                if let Some(target) = self.symbolic_ref_target(reference)? {
                    return Ok(target);
                }
            }
        }

        for candidate in [
            "origin/main",
            "origin/master",
            "origin/trunk",
            "main",
            "master",
            "trunk",
        ] {
            if self.ref_exists(candidate)? {
                return Ok(candidate.to_string());
            }
        }

        Err(anyhow!(
            "could not determine the base branch for --touched; pass --base <ref>"
        ))
    }
}

impl GitContext for RealGit {
    fn root(&self) -> &Path {
        &self.root
    }

    fn ls_files(&self) -> Result<Vec<PathBuf>> {
        self.run(&["ls-files"])
    }

    fn staged_files(&self) -> Result<Vec<PathBuf>> {
        self.run(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--cached",
            "--name-only",
            "--diff-filter=ACMR",
        ])
    }

    fn touched_files(&self, base: Option<&str>) -> Result<Vec<PathBuf>> {
        let base = match base {
            Some(base) => base.to_string(),
            None => self.infer_touched_base()?,
        };
        let out = Command::new("git")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--name-only",
                "--diff-filter=ACMR",
                "--merge-base",
            ])
            .arg(&base)
            .arg("--")
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git diff from {base} failed to spawn"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "could not determine files touched since {base}: {}. Ensure the base ref and its history are available, or pass --base <ref>",
                stderr.trim()
            ));
        }

        let mut files: BTreeSet<PathBuf> = String::from_utf8(out.stdout)
            .context("git diff output not utf8")?
            .lines()
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .collect();
        files.extend(self.run(&["ls-files", "--others", "--exclude-standard"])?);
        Ok(files.into_iter().collect())
    }

    fn unstaged_modified_files(&self) -> Result<Vec<PathBuf>> {
        self.run(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--name-only",
            "--diff-filter=ACMR",
        ])
    }

    fn add(&self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let mut cmd = Command::new("git");
        cmd.arg("add")
            .arg("--force")
            .arg("--")
            .current_dir(&self.root);
        for p in paths {
            cmd.arg(p);
        }
        let out = cmd.output().context("git add failed to spawn")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!("git add failed: {}", stderr.trim()));
        }
        Ok(())
    }

    fn staged_diff(&self, path: &Path, context: u32) -> Result<String> {
        let unified = format!("-U{context}");
        let out = Command::new("git")
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--cached",
                &unified,
                "--",
            ])
            .arg(path)
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git diff --cached failed to spawn for {}", path.display()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "git diff --cached failed for {}: {}",
                path.display(),
                stderr.trim()
            ));
        }
        String::from_utf8(out.stdout).context("git diff output not utf8")
    }

    fn read_staged_file(&self, path: &Path) -> Result<Vec<u8>> {
        let (_mode, sha) = self.index_entry(path)?;
        let out = Command::new("git")
            .args(["cat-file", "-p", &sha])
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git cat-file failed to spawn for {}", path.display()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "git cat-file failed for {}: {}",
                path.display(),
                stderr.trim()
            ));
        }
        Ok(out.stdout)
    }

    fn update_staged_file(&self, path: &Path, contents: &[u8]) -> Result<()> {
        let (mode, _old_sha) = self.index_entry(path)?;
        let mut tmp = tempfile::Builder::new()
            .prefix("kempt-index-")
            .tempfile()
            .context("create staged-file tempfile")?;
        std::io::Write::write_all(&mut tmp, contents)
            .with_context(|| format!("write staged tempfile for {}", path.display()))?;
        std::io::Write::flush(&mut tmp)
            .with_context(|| format!("flush staged tempfile for {}", path.display()))?;

        let mut path_arg = OsString::from("--path=");
        path_arg.push(path.as_os_str());
        let out = Command::new("git")
            .arg("hash-object")
            .arg("-w")
            .arg(path_arg)
            .arg(tmp.path())
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git hash-object failed to spawn for {}", path.display()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "git hash-object failed for {}: {}",
                path.display(),
                stderr.trim()
            ));
        }
        let sha = String::from_utf8(out.stdout)
            .context("git hash-object output not utf8")?
            .trim()
            .to_string();

        let status = Command::new("git")
            .arg("update-index")
            .arg("--cacheinfo")
            .arg(mode)
            .arg(sha)
            .arg(path)
            .current_dir(&self.root)
            .status()
            .with_context(|| format!("git update-index failed to spawn for {}", path.display()))?;
        if !status.success() {
            return Err(anyhow!("git update-index failed for {}", path.display()));
        }
        Ok(())
    }
}

impl RealGit {
    fn index_entry(&self, path: &Path) -> Result<(String, String)> {
        let out = Command::new("git")
            .args(["ls-files", "-s", "--"])
            .arg(path)
            .current_dir(&self.root)
            .output()
            .with_context(|| format!("git ls-files -s failed to spawn for {}", path.display()))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow!(
                "git ls-files -s failed for {}: {}",
                path.display(),
                stderr.trim()
            ));
        }
        let stdout = String::from_utf8(out.stdout).context("git ls-files output not utf8")?;
        let line = stdout
            .lines()
            .next()
            .ok_or_else(|| anyhow!("{} is not in the git index", path.display()))?;
        let mut parts = line.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| anyhow!("missing index mode for {}", path.display()))?
            .to_string();
        let sha = parts
            .next()
            .ok_or_else(|| anyhow!("missing index blob for {}", path.display()))?
            .to_string();
        Ok((mode, sha))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
    }

    fn git_cmd(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
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

    fn init_repo(root: &Path, branch: &str) {
        git_cmd(root, &["init", "-b", branch]);
        git_cmd(root, &["config", "user.email", "test@example.com"]);
        git_cmd(root, &["config", "user.name", "Test User"]);
    }

    #[test]
    fn add_restages_tracked_file_under_ignored_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root, "main");
        write(root, ".gitignore", "ignored/\n");
        write(root, "ignored/file.txt", "before\n");
        git_cmd(root, &["add", ".gitignore"]);
        git_cmd(root, &["add", "--force", "ignored/file.txt"]);
        git_cmd(root, &["commit", "-m", "initial"]);

        write(root, "ignored/file.txt", "after\n");
        let git = RealGit::discover(root).unwrap();

        git.add(&[PathBuf::from("ignored/file.txt")]).unwrap();

        let staged = git_cmd(root, &["diff", "--cached", "--name-only"]);
        assert_eq!(staged, "ignored/file.txt\n");
    }

    #[test]
    fn touched_files_include_branch_worktree_and_untracked_changes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root, "main");
        write(root, ".gitignore", "ignored/\n");
        write(root, "committed.kt", "before\n");
        write(root, "staged.kt", "before\n");
        write(root, "unstaged.kt", "before\n");
        write(root, "deleted.kt", "before\n");
        git_cmd(root, &["add", "."]);
        git_cmd(root, &["commit", "-m", "initial"]);

        git_cmd(root, &["switch", "-c", "feature"]);
        write(root, "committed.kt", "committed on branch\n");
        write(root, "branch-added.kt", "committed on branch\n");
        git_cmd(root, &["add", "committed.kt", "branch-added.kt"]);
        git_cmd(root, &["commit", "-m", "branch changes"]);

        write(root, "staged.kt", "staged\n");
        write(root, "staged-new.kt", "staged new file\n");
        git_cmd(root, &["add", "staged.kt", "staged-new.kt"]);
        write(root, "staged.kt", "staged plus newer unstaged edits\n");
        write(root, "unstaged.kt", "unstaged\n");
        std::fs::remove_file(root.join("deleted.kt")).unwrap();
        write(root, "untracked.kt", "untracked\n");
        write(root, "ignored/ignored.kt", "ignored\n");

        let git = RealGit::discover(root).unwrap();
        let touched = git.touched_files(None).unwrap();

        assert_eq!(
            touched,
            vec![
                PathBuf::from("branch-added.kt"),
                PathBuf::from("committed.kt"),
                PathBuf::from("staged-new.kt"),
                PathBuf::from("staged.kt"),
                PathBuf::from("unstaged.kt"),
                PathBuf::from("untracked.kt"),
            ]
        );
    }

    #[test]
    fn touched_base_prefers_origin_head() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root, "main");
        write(root, "file.kt", "initial\n");
        git_cmd(root, &["add", "."]);
        git_cmd(root, &["commit", "-m", "initial"]);
        git_cmd(root, &["update-ref", "refs/remotes/origin/release", "HEAD"]);
        git_cmd(
            root,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/release",
            ],
        );

        let git = RealGit::discover(root).unwrap();

        assert_eq!(git.infer_touched_base().unwrap(), "origin/release");
    }

    #[test]
    fn touched_files_require_an_inferable_base() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root, "topic");
        write(root, "file.kt", "initial\n");
        git_cmd(root, &["add", "."]);
        git_cmd(root, &["commit", "-m", "initial"]);
        let git = RealGit::discover(root).unwrap();

        let err = git.touched_files(None).unwrap_err();

        assert!(format!("{err:#}").contains("pass --base <ref>"));
    }

    #[test]
    fn parsed_diffs_ignore_external_diff_textconv_and_color_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        init_repo(root, "main");
        write(root, ".gitattributes", "*.txt diff=hostile\n");
        write(root, "staged.txt", "before\n");
        write(root, "unstaged.txt", "before\n");
        git_cmd(root, &["add", "."]);
        git_cmd(root, &["commit", "-m", "initial"]);

        write(root, "staged.txt", "after\n");
        git_cmd(root, &["add", "staged.txt"]);
        write(root, "unstaged.txt", "after\n");
        git_cmd(root, &["config", "diff.external", "false"]);
        git_cmd(root, &["config", "diff.hostile.textconv", "false"]);
        git_cmd(root, &["config", "color.ui", "always"]);

        let git = RealGit::discover(root).unwrap();

        assert_eq!(
            git.staged_files().unwrap(),
            vec![PathBuf::from("staged.txt")]
        );
        assert_eq!(
            git.unstaged_modified_files().unwrap(),
            vec![PathBuf::from("unstaged.txt")]
        );
        assert_eq!(
            git.touched_files(Some("main")).unwrap(),
            vec![PathBuf::from("staged.txt"), PathBuf::from("unstaged.txt")]
        );
        let diff = git.staged_diff(Path::new("staged.txt"), 0).unwrap();
        assert!(diff.contains("@@ -1 +1 @@"), "got: {diff}");
        assert!(
            !diff.contains('\u{1b}'),
            "diff contained ANSI escapes: {diff:?}"
        );
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    /// Test double for [`GitContext`].
    pub struct FakeGit {
        pub root: PathBuf,
        pub tracked: Vec<PathBuf>,
        pub staged: Vec<PathBuf>,
        pub touched: Vec<PathBuf>,
        pub unstaged: Vec<PathBuf>,
        pub added: RefCell<Vec<PathBuf>>,
    }

    impl FakeGit {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self {
                root: root.into(),
                tracked: vec![],
                staged: vec![],
                touched: vec![],
                unstaged: vec![],
                added: RefCell::new(vec![]),
            }
        }

        pub fn with_tracked<I, S>(mut self, paths: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<PathBuf>,
        {
            self.tracked = paths.into_iter().map(Into::into).collect();
            self
        }

        pub fn with_staged<I, S>(mut self, paths: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<PathBuf>,
        {
            self.staged = paths.into_iter().map(Into::into).collect();
            self
        }

        pub fn with_unstaged<I, S>(mut self, paths: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<PathBuf>,
        {
            self.unstaged = paths.into_iter().map(Into::into).collect();
            self
        }

        pub fn with_touched<I, S>(mut self, paths: I) -> Self
        where
            I: IntoIterator<Item = S>,
            S: Into<PathBuf>,
        {
            self.touched = paths.into_iter().map(Into::into).collect();
            self
        }
    }

    impl GitContext for FakeGit {
        fn root(&self) -> &Path {
            &self.root
        }
        fn ls_files(&self) -> Result<Vec<PathBuf>> {
            Ok(self.tracked.clone())
        }
        fn staged_files(&self) -> Result<Vec<PathBuf>> {
            Ok(self.staged.clone())
        }
        fn touched_files(&self, _base: Option<&str>) -> Result<Vec<PathBuf>> {
            Ok(self.touched.clone())
        }
        fn unstaged_modified_files(&self) -> Result<Vec<PathBuf>> {
            Ok(self.unstaged.clone())
        }
        fn add(&self, paths: &[PathBuf]) -> Result<()> {
            self.added.borrow_mut().extend_from_slice(paths);
            Ok(())
        }
    }
}
