// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Git wrapper. Trait-based so paths/hook logic can be tested without a repo.

use anyhow::{anyhow, Context, Result};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Operations kempt needs from git. Paths returned are always relative to the
/// repo root.
pub trait GitContext {
    fn root(&self) -> &Path;
    fn ls_files(&self) -> Result<Vec<PathBuf>>;
    fn staged_files(&self) -> Result<Vec<PathBuf>>;
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
}

impl GitContext for RealGit {
    fn root(&self) -> &Path {
        &self.root
    }

    fn ls_files(&self) -> Result<Vec<PathBuf>> {
        self.run(&["ls-files"])
    }

    fn staged_files(&self) -> Result<Vec<PathBuf>> {
        self.run(&["diff", "--cached", "--name-only", "--diff-filter=ACMR"])
    }

    fn unstaged_modified_files(&self) -> Result<Vec<PathBuf>> {
        self.run(&["diff", "--name-only", "--diff-filter=ACMR"])
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
            .args(["diff", "--cached", &unified, "--"])
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

    #[test]
    fn add_restages_tracked_file_under_ignored_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git_cmd(root, &["init"]);
        git_cmd(root, &["config", "user.email", "test@example.com"]);
        git_cmd(root, &["config", "user.name", "Test User"]);
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
        pub unstaged: Vec<PathBuf>,
        pub added: RefCell<Vec<PathBuf>>,
    }

    impl FakeGit {
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self {
                root: root.into(),
                tracked: vec![],
                staged: vec![],
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
        fn unstaged_modified_files(&self) -> Result<Vec<PathBuf>> {
            Ok(self.unstaged.clone())
        }
        fn add(&self, paths: &[PathBuf]) -> Result<()> {
            self.added.borrow_mut().extend_from_slice(paths);
            Ok(())
        }
    }
}
