//! Git wrapper. Trait-based so paths/hook logic can be tested without a repo.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Operations kempt needs from git. Paths returned are always relative to the
/// repo root.
pub trait GitContext {
    fn root(&self) -> &Path;
    fn ls_files(&self) -> Result<Vec<PathBuf>>;
    fn staged_files(&self) -> Result<Vec<PathBuf>>;
    fn unstaged_modified_files(&self) -> Result<Vec<PathBuf>>;
    fn add(&self, paths: &[PathBuf]) -> Result<()>;
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
        cmd.arg("add").arg("--").current_dir(&self.root);
        for p in paths {
            cmd.arg(p);
        }
        let status = cmd.status().context("git add failed to spawn")?;
        if !status.success() {
            return Err(anyhow!("git add failed"));
        }
        Ok(())
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
