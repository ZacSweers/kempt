// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Formatter installation and namespaced immutable downloads.
//!
//! Use [`ktfmt`] for reviewed checksums, coordinated installs, and cache repair.
//!
//! Keys must change when content changes. Include the version and, for native
//! tools, the platform in the key. Different URLs using the same key refer to
//! the same artifact. Namespaced requests may download twice concurrently, but readers only
//! see completed artifacts.

use anyhow::{anyhow, bail, Context, Result};
pub mod ktfmt;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Writes a download to the supplied temporary destination.
///
/// The cache owns publication and cleanup. Implementations do not need to
/// perform atomic writes and must close the destination before returning.
pub trait Downloader {
    fn download(&self, url: &str, dest: &Path) -> Result<()>;
}

/// Blocking HTTP downloader. Available with the `http` feature.
#[cfg(feature = "http")]
pub struct UreqDownloader;

#[cfg(feature = "http")]
impl Downloader for UreqDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        let response = ureq::get(url).call().with_context(|| format!("download failed: {url}"))?;
        let mut reader = response.into_body().into_reader();
        let mut file = fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
        std::io::copy(&mut reader, &mut file)?;
        file.flush()?;
        Ok(())
    }
}

/// What to retain from a download. Archive members are matched by basename.
///
/// Archive executable names must be single filename components. Exactly one
/// matching regular file must exist. Directories and links are never extracted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactFormat {
    File,
    Executable,
    TarGzExecutable { executable: String },
    ZipExecutable { executable: String },
}

/// An immutable artifact. The key is its retained filename within a namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRequest {
    pub key: String,
    pub url: String,
    pub format: ArtifactFormat,
}

/// Root of a cache. Custom tools must select a namespace before using it.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self { Self { root } }

    /// `$KEMPT_CACHE_DIR`, otherwise `$HOME/.kempt/cache`.
    pub fn default_root() -> Result<PathBuf> {
        if let Some(custom) = std::env::var_os("KEMPT_CACHE_DIR") {
            return Ok(PathBuf::from(custom));
        }
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME not set"))?;
        Ok(PathBuf::from(home).join(".kempt/cache"))
    }

    pub fn root(&self) -> &Path { &self.root }

    /// Select an isolated namespace. Namespace names cannot contain paths.
    pub fn namespace(&self, name: &str) -> Result<CacheNamespace> {
        validate_component(name)?;
        let root = self.root.join(name);
        validate_namespace_directory(&root)?;
        Ok(CacheNamespace { root })
    }
}

/// A caller-owned namespace. Pruning never visits other namespaces.
#[derive(Debug, Clone)]
pub struct CacheNamespace {
    root: PathBuf,
}

impl CacheNamespace {
    pub fn root(&self) -> &Path { &self.root }

    pub fn path(&self, key: &str) -> Result<PathBuf> {
        validate_component(key)?;
        validate_namespace_directory(&self.root)?;
        Ok(self.root.join(key))
    }

    /// Download on a miss, retaining only a completed file.
    pub fn ensure(&self, request: &ArtifactRequest, downloader: &dyn Downloader) -> Result<PathBuf> {
        validate_namespace_directory(&self.root)?;
        ensure_artifact(&self.root, request, downloader)
    }

    /// List regular artifact files, excluding temporary directories and links.
    pub fn list_entries(&self) -> Result<Vec<CacheEntry>> {
        validate_namespace_directory(&self.root)?;
        list_files(&self.root)
    }

    /// Remove entries whose keys are absent from `keep`.
    pub fn prune(&self, keep: &[String]) -> Result<Vec<PathBuf>> {
        for key in keep { validate_component(key)?; }
        let mut removed = Vec::new();
        for entry in self.list_entries()? {
            let key = entry.path.file_name().and_then(|name| name.to_str());
            if !keep.iter().any(|name| Some(name.as_str()) == key) {
                fs::remove_file(&entry.path).with_context(|| format!("remove {}", entry.path.display()))?;
                removed.push(entry.path);
            }
        }
        Ok(removed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub path: PathBuf,
    pub size: u64,
}

fn validate_namespace_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => bail!("cache namespace is not a directory: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

fn validate_component(value: &str) -> Result<()> {
    if value.is_empty() || value.starts_with('.') || value.contains(['/', '\\', ':', '\0']) {
        bail!("cache name must be a single non-hidden filename component: {value:?}");
    }
    Ok(())
}

pub(crate) fn list_files(root: &Path) -> Result<Vec<CacheEntry>> {
    if !root.exists() { return Ok(Vec::new()); }
    let mut entries = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("read cache dir {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        if entry.file_name().to_str().is_some_and(|name| !name.starts_with('.')) {
            entries.push(CacheEntry { path: entry.path(), size: entry.metadata()?.len() });
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

pub(crate) fn ensure_artifact(root: &Path, request: &ArtifactRequest, downloader: &dyn Downloader) -> Result<PathBuf> {
    validate_component(&request.key)?;
    match &request.format {
        ArtifactFormat::TarGzExecutable { executable } | ArtifactFormat::ZipExecutable { executable } => validate_component(executable)?,
        _ => {}
    }
    let dest = root.join(&request.key);
    if regular_file(&dest)? {
        if request.format != ArtifactFormat::File { ensure_executable(&dest)?; }
        return Ok(dest);
    }
    fs::create_dir_all(root).with_context(|| format!("create cache dir {}", root.display()))?;
    let staging = tempfile::Builder::new().prefix(".kempt-download-").tempdir_in(root)?;
    let download = staging.path().join("download");
    downloader.download(&request.url, &download)?;
    if !regular_file(&download)? { bail!("download did not produce a regular file"); }
    let completed = match &request.format {
        ArtifactFormat::File | ArtifactFormat::Executable => download,
        ArtifactFormat::TarGzExecutable { executable } => {
            let extracted = staging.path().join("executable");
            extract_tar(&download, executable, &extracted)?;
            extracted
        }
        ArtifactFormat::ZipExecutable { executable } => {
            let extracted = staging.path().join("executable");
            extract_zip(&download, executable, &extracted)?;
            extracted
        }
    };
    if request.format != ArtifactFormat::File { ensure_executable(&completed)?; }
    // A hard link publishes without replacing another caller's completed file.
    match fs::hard_link(&completed, &dest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && regular_file(&dest)? => {}
        Err(error) => return Err(error).with_context(|| format!("publish {}", dest.display())),
    }
    Ok(dest)
}

fn regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(true),
        Ok(_) => bail!("cache artifact is not a regular file: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

fn safe_member(path: &Path, executable: &str) -> bool {
    !path.is_absolute()
        && path.components().all(|component| matches!(component, std::path::Component::Normal(_) | std::path::Component::CurDir))
        && path.file_name().and_then(|name| name.to_str()) == Some(executable)
}

pub(crate) fn extract_tar(archive: &Path, executable: &str, dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(fs::File::open(archive)?);
    let mut archive = tar::Archive::new(decoder);
    let mut found = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file() && safe_member(&entry.path()?, executable) {
            if found { bail!("archive contains multiple files named {executable}"); }
            let mut output = fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut output)?;
            output.flush()?;
            found = true;
        }
    }
    if !found { bail!("archive did not contain {executable}"); }
    Ok(())
}

pub(crate) fn extract_zip(archive: &Path, executable: &str, dest: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(fs::File::open(archive)?)?;
    let mut found = false;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let is_regular = !entry.is_dir() && entry.unix_mode().map_or(true, |mode| mode & 0o170000 == 0 || mode & 0o170000 == 0o100000);
        if is_regular && entry.enclosed_name().is_some_and(|path| safe_member(path, executable)) {
            if found { bail!("archive contains multiple files named {executable}"); }
            let mut output = fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut output)?;
            output.flush()?;
            found = true;
        }
    }
    if !found { bail!("archive did not contain {executable}"); }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn ensure_executable(_path: &Path) -> Result<()> { Ok(()) }
