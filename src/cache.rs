// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Binary cache management.
//!
//! Cached files are version-suffixed so repos with different pins coexist:
//! `ktfmt-<version>.jar`, `gjf-<version>.jar`, `detekt-<version>.jar`, and
//! native gjf binaries named `gjf-<version>-<asset>[.exe]`.
//!
//! Downloads are abstracted behind [`Downloader`] so tests can fake them.

use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const KTFMT_URL: &str =
    "https://repo1.maven.org/maven2/com/facebook/ktfmt/{v}/ktfmt-{v}-with-dependencies.jar";

pub const GJF_URL: &str =
    "https://github.com/google/google-java-format/releases/download/v{v}/google-java-format-{v}-all-deps.jar";

pub const GJF_NATIVE_URL: &str =
    "https://github.com/google/google-java-format/releases/download/v{v}/google-java-format_{asset}{ext}";

pub const DETEKT_URL: &str =
    "https://github.com/detekt/detekt/releases/download/v{v}/detekt-cli-{v}-all.jar";

/// gjf's published native asset for a particular `(os, arch)` combo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeAsset {
    /// The asset segment, e.g. `darwin-arm64` or `linux-x86-64`.
    pub asset: &'static str,
    /// Empty on unix, `.exe` on Windows.
    pub exe_suffix: &'static str,
}

/// Native asset for the current host, if gjf publishes one for this combo.
/// Returns `None` for Intel macOS and unknown platforms.
pub fn current_native_asset() -> Option<NativeAsset> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some(NativeAsset {
            asset: "darwin-arm64",
            exe_suffix: "",
        }),
        ("linux", "x86_64") => Some(NativeAsset {
            asset: "linux-x86-64",
            exe_suffix: "",
        }),
        ("linux", "aarch64") => Some(NativeAsset {
            asset: "linux-arm64",
            exe_suffix: "",
        }),
        ("windows", "x86_64") => Some(NativeAsset {
            asset: "windows-x86-64",
            exe_suffix: ".exe",
        }),
        _ => None,
    }
}

/// Whether gjf publishes the given native asset for `version`. Native builds
/// started in 1.20.0; linux-arm64 was added in 1.26.0.
pub fn native_supported_for_version(version: &str, asset: &NativeAsset) -> bool {
    let Some((maj, min, _)) = parse_version(version) else {
        return false;
    };
    if (maj, min) < (1, 20) {
        return false;
    }
    if asset.asset == "linux-arm64" && (maj, min) < (1, 26) {
        return false;
    }
    true
}

/// Outcome of choosing between native and jar for a particular gjf install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GjfFlavor {
    Jar,
    Native(NativeAsset),
}

/// Decide whether to use the native binary or the jar for `version`,
/// considering the user's preference and what gjf publishes for this host.
///
/// `prefer_native = false` always picks the jar.
/// `prefer_native = true` picks native when published, falls back to jar
/// unless `require_native = true` (in which case we error).
pub fn resolve_gjf_flavor(
    version: &str,
    prefer_native: bool,
    require_native: bool,
) -> Result<GjfFlavor> {
    if prefer_native {
        if let Some(asset) = current_native_asset() {
            if native_supported_for_version(version, &asset) {
                return Ok(GjfFlavor::Native(asset));
            }
        }
        if require_native {
            return Err(anyhow!(
                "native gjf is not available for version {version} on this platform"
            ));
        }
    }
    Ok(GjfFlavor::Jar)
}

fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

pub trait Downloader {
    fn download(&self, url: &str, dest: &Path) -> Result<()>;
}

pub struct UreqDownloader;

impl Downloader for UreqDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let resp = ureq::get(url)
            .call()
            .with_context(|| format!("download failed: {url}"))?;
        let mut reader = resp.into_body().into_reader();
        atomic_write(dest, |f| std::io::copy(&mut reader, f).map(|_| ()))
            .with_context(|| format!("write failed: {}", dest.display()))?;
        Ok(())
    }
}

pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `$KEMPT_CACHE_DIR` if set, else `~/.kempt/cache`.
    pub fn default_root() -> Result<PathBuf> {
        if let Ok(custom) = std::env::var("KEMPT_CACHE_DIR") {
            return Ok(PathBuf::from(custom));
        }
        let home = std::env::var("HOME").map_err(|_| anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home).join(".kempt").join("cache"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ktfmt_path(&self, version: &str) -> PathBuf {
        self.root.join(format!("ktfmt-{version}.jar"))
    }

    /// Path for the gjf JVM jar (no platform/arch suffix).
    pub fn gjf_jar_path(&self, version: &str) -> PathBuf {
        self.root.join(format!("gjf-{version}.jar"))
    }

    pub fn detekt_path(&self, version: &str) -> PathBuf {
        self.root.join(format!("detekt-{version}.jar"))
    }

    /// Path for the gjf native binary on the given platform asset.
    pub fn gjf_native_path(&self, version: &str, asset: &NativeAsset) -> PathBuf {
        self.root
            .join(format!("gjf-{version}-{}{}", asset.asset, asset.exe_suffix))
    }

    /// Ensure the ktfmt jar for `version` is present, downloading if needed.
    pub fn ensure_ktfmt(&self, version: &str, downloader: &dyn Downloader) -> Result<PathBuf> {
        let dest = self.ktfmt_path(version);
        let url = KTFMT_URL.replace("{v}", version);
        self.ensure(&dest, &url, downloader)
    }

    /// Ensure the gjf jar for `version` is present.
    pub fn ensure_gjf_jar(&self, version: &str, downloader: &dyn Downloader) -> Result<PathBuf> {
        let dest = self.gjf_jar_path(version);
        let url = GJF_URL.replace("{v}", version);
        self.ensure(&dest, &url, downloader)
    }

    pub fn ensure_detekt(&self, version: &str, downloader: &dyn Downloader) -> Result<PathBuf> {
        let dest = self.detekt_path(version);
        let url = DETEKT_URL.replace("{v}", version);
        self.ensure(&dest, &url, downloader)
    }

    /// Ensure the gjf native binary for `version` + `asset` is present and
    /// executable. Caller is responsible for checking that the combo is
    /// supported (see [`native_supported_for_version`]).
    pub fn ensure_gjf_native(
        &self,
        version: &str,
        asset: &NativeAsset,
        downloader: &dyn Downloader,
    ) -> Result<PathBuf> {
        let dest = self.gjf_native_path(version, asset);
        let url = GJF_NATIVE_URL
            .replace("{v}", version)
            .replace("{asset}", asset.asset)
            .replace("{ext}", asset.exe_suffix);
        let path = self.ensure(&dest, &url, downloader)?;
        ensure_executable(&path)?;
        Ok(path)
    }

    fn ensure(&self, dest: &Path, url: &str, downloader: &dyn Downloader) -> Result<PathBuf> {
        if dest.exists() {
            return Ok(dest.to_path_buf());
        }
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create cache dir {}", self.root.display()))?;
        downloader.download(url, dest)?;
        Ok(dest.to_path_buf())
    }

    /// List kempt-managed files currently in the cache. Returns an empty list
    /// if the cache directory does not yet exist.
    pub fn list_entries(&self) -> Result<Vec<CacheEntry>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for dirent in fs::read_dir(&self.root)
            .with_context(|| format!("read cache dir {}", self.root.display()))?
        {
            let dirent = dirent?;
            let path = dirent.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            // Hidden / temp files (e.g. atomic-write `.foo.tmp`) shouldn't show up.
            if name.starts_with('.') {
                continue;
            }
            if !is_kempt_artifact_name(name) {
                continue;
            }
            let size = dirent.metadata()?.len();
            entries.push(CacheEntry { path, size });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// Remove every managed cache entry not in `keep`. Returns the
    /// removed paths in sorted order.
    pub fn prune(&self, keep: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let entries = self.list_entries()?;
        let keep_set: std::collections::HashSet<&Path> =
            keep.iter().map(PathBuf::as_path).collect();
        let mut removed = Vec::new();
        for entry in entries {
            if !keep_set.contains(entry.path.as_path()) {
                fs::remove_file(&entry.path)
                    .with_context(|| format!("remove {}", entry.path.display()))?;
                removed.push(entry.path);
            }
        }
        Ok(removed)
    }
}

fn is_kempt_artifact_name(name: &str) -> bool {
    if let Some(rest) = name.strip_prefix("ktfmt-") {
        return starts_with_digit(rest) && rest.ends_with(".jar");
    }
    if let Some(rest) = name.strip_prefix("detekt-") {
        return starts_with_digit(rest) && rest.ends_with(".jar");
    }
    let Some(rest) = name.strip_prefix("gjf-") else {
        return false;
    };
    starts_with_digit(rest)
        && (rest.ends_with(".jar")
            || rest.ends_with(".exe")
            || ["-darwin-arm64", "-linux-x86-64", "-linux-arm64"]
                .iter()
                .any(|suffix| rest.ends_with(suffix)))
}

fn starts_with_digit(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub path: PathBuf,
    pub size: u64,
}

/// chmod +x on unix; no-op on Windows (where executability is by extension).
#[cfg(unix)]
fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms).with_context(|| format!("chmod +x {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Write to `dest` atomically by writing to a sibling tempfile and renaming.
fn atomic_write<F>(dest: &Path, write: F) -> Result<()>
where
    F: FnOnce(&mut std::fs::File) -> std::io::Result<()>,
{
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("destination has no parent: {}", dest.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp",
        dest.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("kempt-download")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        write(&mut f).with_context(|| format!("write {}", tmp.display()))?;
        f.flush()?;
    }
    fs::rename(&tmp, dest)
        .with_context(|| format!("rename {} -> {}", tmp.display(), dest.display()))?;
    Ok(())
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::cell::RefCell;

    pub struct FakeDownloader {
        pub payload: Vec<u8>,
        pub calls: RefCell<Vec<(String, PathBuf)>>,
    }

    impl FakeDownloader {
        pub fn new(payload: impl Into<Vec<u8>>) -> Self {
            Self {
                payload: payload.into(),
                calls: RefCell::new(vec![]),
            }
        }
    }

    impl Downloader for FakeDownloader {
        fn download(&self, url: &str, dest: &Path) -> Result<()> {
            self.calls
                .borrow_mut()
                .push((url.to_string(), dest.to_path_buf()));
            atomic_write(dest, |f| f.write_all(&self.payload))?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FakeDownloader;
    use super::*;

    #[test]
    fn paths_are_version_suffixed() {
        let c = Cache::new(PathBuf::from("/c"));
        assert_eq!(c.ktfmt_path("0.56"), PathBuf::from("/c/ktfmt-0.56.jar"));
        assert_eq!(c.gjf_jar_path("1.28.0"), PathBuf::from("/c/gjf-1.28.0.jar"));
        assert_eq!(
            c.detekt_path("2.0.0-alpha.5"),
            PathBuf::from("/c/detekt-2.0.0-alpha.5.jar")
        );
    }

    #[test]
    fn ensure_ktfmt_downloads_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"jarbytes".to_vec());

        let path = cache.ensure_ktfmt("0.56", &dl).unwrap();
        assert!(path.exists());
        assert_eq!(fs::read(&path).unwrap(), b"jarbytes");
        let calls = dl.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.contains("0.56"));
        assert!(calls[0].0.contains("ktfmt"));
    }

    #[test]
    fn ensure_ktfmt_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());

        cache.ensure_ktfmt("0.56", &dl).unwrap();
        cache.ensure_ktfmt("0.56", &dl).unwrap();
        cache.ensure_ktfmt("0.56", &dl).unwrap();
        assert_eq!(dl.calls.borrow().len(), 1, "should only download once");
    }

    #[test]
    fn ensure_gjf_jar_uses_correct_url() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());

        cache.ensure_gjf_jar("1.28.0", &dl).unwrap();
        let calls = dl.calls.borrow();
        assert!(calls[0].0.contains("v1.28.0"));
        assert!(calls[0]
            .0
            .contains("google-java-format-1.28.0-all-deps.jar"));
    }

    #[test]
    fn ensure_detekt_uses_release_fat_jar() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());
        cache.ensure_detekt("2.0.0-alpha.5", &dl).unwrap();
        let calls = dl.calls.borrow();
        assert!(calls[0].0.contains("/v2.0.0-alpha.5/"));
        assert!(calls[0].0.ends_with("detekt-cli-2.0.0-alpha.5-all.jar"));
    }

    #[test]
    fn ensure_gjf_native_uses_correct_url_and_chmods() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"native-bytes".to_vec());
        let asset = NativeAsset {
            asset: "darwin-arm64",
            exe_suffix: "",
        };
        let path = cache.ensure_gjf_native("1.28.0", &asset, &dl).unwrap();
        let calls = dl.calls.borrow();
        assert!(calls[0].0.contains("google-java-format_darwin-arm64"));
        assert!(path.ends_with("gjf-1.28.0-darwin-arm64"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "expected +x");
        }
    }

    #[test]
    fn ensure_gjf_native_windows_includes_exe_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());
        let asset = NativeAsset {
            asset: "windows-x86-64",
            exe_suffix: ".exe",
        };
        let path = cache.ensure_gjf_native("1.28.0", &asset, &dl).unwrap();
        assert!(path
            .to_str()
            .unwrap()
            .ends_with("gjf-1.28.0-windows-x86-64.exe"));
        let calls = dl.calls.borrow();
        assert!(calls[0]
            .0
            .ends_with("google-java-format_windows-x86-64.exe"));
    }

    #[test]
    fn native_supported_for_version_uses_correct_cutoffs() {
        let darwin = NativeAsset {
            asset: "darwin-arm64",
            exe_suffix: "",
        };
        let linux_arm = NativeAsset {
            asset: "linux-arm64",
            exe_suffix: "",
        };
        // Pre-1.20: nothing.
        assert!(!native_supported_for_version("1.19.0", &darwin));
        // 1.20+: most platforms.
        assert!(native_supported_for_version("1.20.0", &darwin));
        assert!(native_supported_for_version("1.28.0", &darwin));
        // linux-arm64 only from 1.26.
        assert!(!native_supported_for_version("1.22.0", &linux_arm));
        assert!(native_supported_for_version("1.26.0", &linux_arm));
    }

    #[test]
    fn native_supported_handles_malformed_versions() {
        let asset = NativeAsset {
            asset: "darwin-arm64",
            exe_suffix: "",
        };
        assert!(!native_supported_for_version("garbage", &asset));
        assert!(!native_supported_for_version("1", &asset));
    }

    #[test]
    fn resolve_gjf_flavor_returns_jar_when_native_disabled() {
        let flavor = resolve_gjf_flavor("1.28.0", false, false).unwrap();
        assert_eq!(flavor, GjfFlavor::Jar);
    }

    #[test]
    fn resolve_gjf_flavor_falls_back_to_jar_for_old_version() {
        // Pre-1.20: even with prefer_native, falls back.
        let flavor = resolve_gjf_flavor("1.19.0", true, false).unwrap();
        assert_eq!(flavor, GjfFlavor::Jar);
    }

    #[test]
    fn resolve_gjf_flavor_require_errors_when_unavailable() {
        // Force a version with no native build; require_native should error.
        let err = resolve_gjf_flavor("1.19.0", true, true).unwrap_err();
        assert!(format!("{err:#}").contains("not available"));
    }

    #[test]
    fn ensure_creates_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c");
        assert!(!nested.exists());
        let cache = Cache::new(nested.clone());
        let dl = FakeDownloader::new(b"x".to_vec());
        cache.ensure_ktfmt("0.56", &dl).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn different_versions_are_cached_separately() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());
        let a = cache.ensure_ktfmt("0.56", &dl).unwrap();
        let b = cache.ensure_ktfmt("0.57", &dl).unwrap();
        assert_ne!(a, b);
        assert!(a.exists() && b.exists());
        assert_eq!(dl.calls.borrow().len(), 2);
    }

    #[test]
    fn list_entries_returns_empty_when_cache_dir_absent() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("does-not-exist"));
        assert!(cache.list_entries().unwrap().is_empty());
    }

    #[test]
    fn list_entries_returns_artifacts_with_sizes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"abcdef".to_vec());
        cache.ensure_ktfmt("0.56", &dl).unwrap();
        cache.ensure_gjf_jar("1.28.0", &dl).unwrap();
        cache.ensure_detekt("1.23.8", &dl).unwrap();
        let entries = cache.list_entries().unwrap();
        assert_eq!(entries.len(), 3);
        for e in &entries {
            assert_eq!(e.size, 6);
        }
    }

    #[test]
    fn list_entries_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        fs::create_dir_all(cache.root()).unwrap();
        fs::write(cache.root().join("notes.txt"), b"x").unwrap();
        fs::write(cache.root().join("notes.jar"), b"x").unwrap();
        fs::write(cache.root().join("gjf-notes.txt"), b"x").unwrap();
        fs::write(cache.root().join("ktfmt-notes.txt"), b"x").unwrap();
        fs::write(cache.root().join(".hidden.tmp"), b"x").unwrap();
        fs::write(cache.root().join("ktfmt-0.56.jar"), b"x").unwrap();
        let entries = cache.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].path.ends_with("ktfmt-0.56.jar"));
    }

    #[test]
    fn list_entries_includes_native_gjf_binary() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        fs::create_dir_all(cache.root()).unwrap();
        fs::write(cache.root().join("gjf-1.28.0-darwin-arm64"), b"x").unwrap();
        fs::write(cache.root().join("gjf-1.28.0.jar"), b"x").unwrap();
        let entries = cache.list_entries().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn prune_removes_only_unkept_jars() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());
        cache.ensure_ktfmt("0.55", &dl).unwrap();
        cache.ensure_ktfmt("0.56", &dl).unwrap();
        cache.ensure_gjf_jar("1.28.0", &dl).unwrap();

        let keep = vec![cache.ktfmt_path("0.56"), cache.gjf_jar_path("1.28.0")];
        let removed = cache.prune(&keep).unwrap();

        assert_eq!(removed.len(), 1);
        assert!(removed[0].ends_with("ktfmt-0.55.jar"));
        assert!(cache.ktfmt_path("0.56").exists());
        assert!(cache.gjf_jar_path("1.28.0").exists());
        assert!(!cache.ktfmt_path("0.55").exists());
    }

    #[test]
    fn prune_with_empty_keep_removes_everything() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().to_path_buf());
        let dl = FakeDownloader::new(b"x".to_vec());
        cache.ensure_ktfmt("0.56", &dl).unwrap();
        cache.ensure_gjf_jar("1.28.0", &dl).unwrap();
        let removed = cache.prune(&[]).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(cache.list_entries().unwrap().is_empty());
    }

    #[test]
    fn prune_on_empty_cache_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::new(dir.path().join("absent"));
        assert!(cache.prune(&[]).unwrap().is_empty());
    }
}
