// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Resolve ktfmt releases without I/O, then install with a caller-reviewed SHA-256.
//!
//! Native and JAR selection is explicit. Download, integrity, and extraction
//! failures never change the selected runtime. The caller owns Java selection,
//! formatter arguments, and invocation.
//!
//! Cache hits rehash installed bytes against a receipt bound to the release and
//! expected download checksum. Receipts are trusted local metadata. This detects
//! damaged or incomplete entries, but does not authenticate a cache against an
//! attacker who can rewrite both receipts and installed files. Use a cache root
//! writable only by trusted users. File locks coordinate cooperating processes.

use super::{Cache, Downloader};
use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const RECEIPT_VERSION: &str = "kempt-ktfmt-v1";
const RELEASE_ROOT: &str = "https://github.com/kotlin/ktfmt/releases/download";

/// Rust host names, such as `macos` and `aarch64`. JARs are host-independent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub os: String,
    pub arch: String,
}

impl Platform {
    /// Read compile-time host constants without accessing the environment.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
        }
    }
}

/// The caller selects a runtime before obtaining its checksum pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeKind {
    Native,
    Jar,
}

/// A release description produced without cache or network access.
/// Fields stay private so installation cannot be redirected after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifact {
    version: String,
    platform: Platform,
    runtime: RuntimeKind,
    asset_name: String,
    source_url: String,
    cache_name: String,
    native: Option<NativeLayout>,
}

/// A completed installation. Neither variant selects Java or runs a formatter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledArtifact {
    NativeExecutable(PathBuf),
    StandaloneJar(PathBuf),
}

impl InstalledArtifact {
    /// Return the checksum-specific cached path.
    pub fn path(&self) -> &Path {
        match self {
            Self::NativeExecutable(path) | Self::StandaloneJar(path) => path,
        }
    }
}

impl ReleaseArtifact {
    /// Release version used in the source URL.
    pub fn version(&self) -> &str {
        &self.version
    }
    /// Requested host. Standalone JARs share one artifact across hosts.
    pub fn platform(&self) -> &Platform {
        &self.platform
    }
    /// Runtime selected before looking up a reviewed checksum.
    pub fn runtime(&self) -> RuntimeKind {
        self.runtime
    }
    /// Release asset name for the consumer's reviewed-pin lookup.
    pub fn asset_name(&self) -> &str {
        &self.asset_name
    }
    /// Source URL passed to the caller-provided downloader.
    pub fn source_url(&self) -> &str {
        &self.source_url
    }

    /// Verify the downloaded release before extraction and publish a complete
    /// installation. Concurrent callers share one install under an OS file lock.
    ///
    /// `expected_sha256` is the 64-digit hexadecimal hash of the release download,
    /// including the archive for native assets. Cache paths include its binding,
    /// so changing the pin cannot replace a path returned for an earlier pin.
    /// A valid cache hit requires no network. An unreceipted legacy JAR can be
    /// adopted by hashing it. An unreceipted native install must be downloaded
    /// once because the extracted executable cannot prove the archive checksum.
    pub fn ensure(
        &self,
        cache: &Cache,
        expected_sha256: &str,
        downloader: &dyn Downloader,
    ) -> Result<InstalledArtifact> {
        let expected = normalize_checksum(expected_sha256)?;
        let installation = install(self, cache.root(), Some(&expected), downloader)?;
        Ok(match self.runtime {
            RuntimeKind::Native => InstalledArtifact::NativeExecutable(installation.path),
            RuntimeKind::Jar => InstalledArtifact::StandaloneJar(installation.path),
        })
    }

    /// Bind receipts to the release identity. JARs share an identity across hosts.
    fn identity(&self) -> String {
        let mut hash = Sha256::new();
        for field in [
            RECEIPT_VERSION,
            &self.version,
            &self.asset_name,
            &self.source_url,
            &self.cache_name,
        ] {
            hash.update((field.len() as u64).to_le_bytes());
            hash.update(field.as_bytes());
        }
        hash.update([match self.runtime {
            RuntimeKind::Native => 0,
            RuntimeKind::Jar => 1,
        }]);
        if self.runtime == RuntimeKind::Native {
            for field in [&self.platform.os, &self.platform.arch] {
                hash.update((field.len() as u64).to_le_bytes());
                hash.update(field.as_bytes());
            }
        }
        format!("{:x}", hash.finalize())
    }
}

/// Select exactly the requested runtime using ktfmt's release naming rules.
/// Unsupported native hosts and versions return an error before any I/O.
/// The caller can then explicitly resolve a standalone JAR if its policy allows.
pub fn resolve(version: &str, platform: Platform, runtime: RuntimeKind) -> Result<ReleaseArtifact> {
    if version.is_empty()
        || !version.starts_with(|character: char| character.is_ascii_digit())
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte))
    {
        bail!("invalid ktfmt release version: {version:?}");
    }
    let (asset_name, cache_name, native) = match runtime {
        RuntimeKind::Jar => (
            format!("ktfmt-{version}-with-dependencies.jar"),
            format!("ktfmt-{version}.jar"),
            None,
        ),
        RuntimeKind::Native => {
            let layout = native_layout_for(&platform.os, &platform.arch)
                .filter(|_| native_supported_for_version(version))
                .ok_or_else(|| {
                    anyhow!(
                        "native ktfmt is not available for version {version} on {}-{}",
                        platform.os,
                        platform.arch
                    )
                })?;
            (
                format!("ktfmt-{}-{version}.{}", layout.asset, layout.archive_ext),
                format!("ktfmt-{version}-{}{}", layout.asset, layout.exe_suffix),
                Some(layout),
            )
        }
    };
    let source_url = format!("{RELEASE_ROOT}/v{version}/{asset_name}");
    Ok(ReleaseArtifact {
        version: version.into(),
        platform,
        runtime,
        asset_name,
        source_url,
        cache_name,
        native,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveFormat {
    TarGz,
    Zip,
}

/// Shared release layout used by the CLI's native-selection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeLayout {
    pub asset: &'static str,
    pub archive_ext: &'static str,
    pub executable_name: &'static str,
    pub exe_suffix: &'static str,
    pub format: ArchiveFormat,
}

/// Map Rust host names to the published native release layout.
pub(crate) fn native_layout_for(os: &str, arch: &str) -> Option<NativeLayout> {
    let (asset, archive_ext, executable_name, exe_suffix, format) = match (os, arch) {
        ("macos", "aarch64") => ("macos-aarch64", "tar.gz", "ktfmt", "", ArchiveFormat::TarGz),
        ("linux", "x86_64") => ("linux-x86_64", "tar.gz", "ktfmt", "", ArchiveFormat::TarGz),
        ("windows", "x86_64") => (
            "windows-x86_64",
            "zip",
            "ktfmt.exe",
            ".exe",
            ArchiveFormat::Zip,
        ),
        _ => return None,
    };
    Some(NativeLayout {
        asset,
        archive_ext,
        executable_name,
        exe_suffix,
        format,
    })
}

/// Native release assets started with ktfmt 0.65.
pub(crate) fn native_supported_for_version(version: &str) -> bool {
    let mut parts = version.split('.');
    match (
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    ) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 65),
        _ => false,
    }
}

/// CLI policy accepts an unpinned release, while sharing coordination, receipts,
/// and installation storage with callers that supply a reviewed checksum.
#[cfg(feature = "commands")]
pub(crate) fn ensure_for_cli(
    artifact: &ReleaseArtifact,
    root: &Path,
    downloader: &dyn Downloader,
) -> Result<PathBuf> {
    install(artifact, root, None, downloader)?;
    Ok(root.join(&artifact.cache_name))
}

/// Evict canonical entries when the CLI prunes their visible legacy filename.
/// Keep the lock file and its directory so waiting processes retain one lock inode.
#[cfg(feature = "commands")]
pub(crate) fn prune_for_cli(root: &Path, cache_name: &str) -> Result<()> {
    let state = state_directory(root, cache_name)?;
    let _lock = acquire_lock(&state)?;
    for entry in fs::read_dir(&state)? {
        let entry = entry?;
        if entry.file_name() != "install.lock" {
            remove_owned(&entry.path())?;
        }
    }
    remove_owned(&root.join(cache_name))
}

struct Installation {
    path: PathBuf,
    expected: String,
    installed: String,
}

/// All validation, repair, and publication happens while this key's lock is held.
fn install(
    artifact: &ReleaseArtifact,
    root: &Path,
    expected: Option<&str>,
    downloader: &dyn Downloader,
) -> Result<Installation> {
    let state = state_directory(root, &artifact.cache_name)?;
    let _lock = acquire_lock(&state)?;
    cleanup_staging(&state)?;
    let identity = artifact.identity();
    if let Some(expected) = expected {
        let entry = state.join(entry_key(&identity, expected));
        if let Some(installation) = validate_entry(artifact, &entry, Some(expected))? {
            publish_alias(artifact, root, &state, &installation, false)?;
            return Ok(installation);
        }
    } else {
        let mut managed = false;
        let mut candidates = Vec::new();
        if let Some(current) = read_small(&state.join("current"))? {
            managed = true;
            if normalize_checksum(current.trim()).is_ok() {
                candidates.push(state.join(current.trim()));
            }
        }
        for entry in fs::read_dir(&state)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| normalize_checksum(name).is_ok())
            {
                managed = true;
                if !candidates.contains(&entry.path()) {
                    candidates.push(entry.path());
                }
            }
        }
        for entry in candidates {
            if let Some(installation) = validate_entry(artifact, &entry, None)? {
                publish_alias(artifact, root, &state, &installation, false)?;
                return Ok(installation);
            }
        }
        let legacy = root.join(&artifact.cache_name);
        // Preserve use of pre-existing unpinned CLI entries. Reviewed native
        // callers do not adopt these because their archive checksum is unknown.
        if !managed && is_regular(&legacy)? {
            if artifact.runtime == RuntimeKind::Native {
                super::ensure_executable(&legacy)?;
            }
            return Ok(Installation {
                path: legacy,
                expected: String::new(),
                installed: String::new(),
            });
        }
    }

    let staging = tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(&state)?;
    let contents = staging.path().join("entry");
    fs::create_dir(&contents)?;
    let download = staging.path().join("download");
    let legacy = root.join(&artifact.cache_name);
    // A standalone JAR's installed bytes are the release bytes, so an old CLI
    // install can be verified and adopted offline without trusting its existence.
    let adopt = artifact.runtime == RuntimeKind::Jar
        && expected.is_some()
        && file_checksum(&legacy)?.as_deref() == expected;
    if adopt {
        fs::copy(&legacy, &download)?;
    } else {
        downloader.download(&artifact.source_url, &download)?;
    }
    let actual = file_checksum(&download)?
        .ok_or_else(|| anyhow!("download did not produce a regular file"))?;
    if expected.is_some_and(|expected| expected != actual) {
        bail!(
            "SHA-256 mismatch for {}: expected {}, got {actual}",
            artifact.asset_name,
            expected.unwrap()
        );
    }
    let destination = contents.join(&artifact.cache_name);
    match artifact.native {
        None => fs::rename(&download, &destination)?,
        Some(layout) => {
            // Only the verified download is passed to the archive reader.
            match layout.format {
                ArchiveFormat::TarGz => {
                    super::extract_tar(&download, layout.executable_name, &destination)?
                }
                ArchiveFormat::Zip => {
                    super::extract_zip(&download, layout.executable_name, &destination)?
                }
            }
            super::ensure_executable(&destination)?;
        }
    }
    let installed = file_checksum(&destination)?
        .ok_or_else(|| anyhow!("installation did not produce a regular file"))?;
    OpenOptions::new()
        .write(true)
        .open(&destination)?
        .sync_all()?;
    let receipt = format!("{RECEIPT_VERSION}\n{identity}\n{actual}\n{installed}\n");
    let mut file = File::create(contents.join("receipt"))?;
    file.write_all(receipt.as_bytes())?;
    file.sync_all()?;
    drop(file);
    let entry = state.join(entry_key(&identity, &actual));
    remove_owned(&entry)?;
    fs::rename(&contents, &entry)
        .with_context(|| format!("publish ktfmt installation {}", entry.display()))?;
    let installation = Installation {
        path: entry.join(&artifact.cache_name),
        expected: actual,
        installed,
    };
    publish_alias(artifact, root, &state, &installation, true)?;
    Ok(installation)
}

/// Verify identity, expected download pin, and current installed bytes on every hit.
fn validate_entry(
    artifact: &ReleaseArtifact,
    entry: &Path,
    expected: Option<&str>,
) -> Result<Option<Installation>> {
    if !is_directory(entry)? {
        return Ok(None);
    }
    let Some(receipt) = read_small(&entry.join("receipt"))? else {
        return Ok(None);
    };
    let fields: Vec<_> = receipt.lines().collect();
    let identity = artifact.identity();
    if fields.len() != 4
        || fields[0] != RECEIPT_VERSION
        || fields[1] != identity
        || normalize_checksum(fields[2]).is_err()
        || normalize_checksum(fields[3]).is_err()
        || expected.is_some_and(|expected| expected != fields[2])
        || (artifact.runtime == RuntimeKind::Jar && fields[2] != fields[3])
        || entry.file_name().and_then(|name| name.to_str())
            != Some(entry_key(&identity, fields[2]).as_str())
    {
        return Ok(None);
    }
    let path = entry.join(&artifact.cache_name);
    if file_checksum(&path)?.as_deref() != Some(fields[3]) {
        return Ok(None);
    }
    if artifact.runtime == RuntimeKind::Native {
        super::ensure_executable(&path)?;
    }
    Ok(Some(Installation {
        path,
        expected: fields[2].into(),
        installed: fields[3].into(),
    }))
}

/// Publish the CLI's original flat filename as a hard link where possible.
/// Canonical checksum-specific paths remain stable when another pin is installed.
fn publish_alias(
    artifact: &ReleaseArtifact,
    root: &Path,
    state: &Path,
    installation: &Installation,
    replace: bool,
) -> Result<()> {
    let alias = root.join(&artifact.cache_name);
    if replace || file_checksum(&alias)?.as_deref() != Some(installation.installed.as_str()) {
        let staging = tempfile::Builder::new()
            .prefix(".stage-")
            .tempdir_in(state)?;
        let link = staging.path().join("alias");
        if fs::hard_link(&installation.path, &link).is_err() {
            fs::copy(&installation.path, &link)?;
        }
        if !is_regular(&alias)? {
            remove_owned(&alias)?;
        }
        fs::rename(&link, &alias).with_context(|| format!("publish {}", alias.display()))?;
    }
    let mut pointer = tempfile::Builder::new()
        .prefix(".stage-")
        .tempfile_in(state)?;
    write!(
        pointer,
        "{}",
        entry_key(&artifact.identity(), &installation.expected)
    )?;
    pointer.as_file().sync_all()?;
    let current = state.join("current");
    if !is_regular(&current)? {
        remove_owned(&current)?;
    }
    pointer.persist(current)?;
    Ok(())
}

/// Keep each release-and-checksum binding at an immutable installation path.
fn entry_key(identity: &str, expected: &str) -> String {
    format!(
        "{:x}",
        Sha256::digest(format!("{identity}\n{expected}\n").as_bytes())
    )
}

/// Validate caller pins before creating cache state or making a download.
fn normalize_checksum(value: &str) -> Result<String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a 64-digit hexadecimal SHA-256 checksum");
    }
    Ok(value.to_ascii_lowercase())
}

/// Bound receipt reads and reject links before opening local cache metadata.
fn read_small(path: &Path) -> Result<Option<String>> {
    if !is_regular(path)? {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

/// Invalid file types are misses so repair never follows an entry symlink.
fn file_checksum(path: &Path) -> Result<Option<String>> {
    if !is_regular(path)? {
        return Ok(None);
    }
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

fn is_regular(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

fn is_directory(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.file_type().is_dir()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("stat {}", path.display())),
    }
}

/// Locate shared state under the supplied root without following internal links.
fn state_directory(root: &Path, cache_name: &str) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let parent = root.join(".ktfmt-install");
    create_owned_directory(&parent)?;
    let state = parent.join(cache_name);
    create_owned_directory(&state)?;
    Ok(state)
}

/// Internal state directories cannot redirect locks or installs through links.
fn create_owned_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && is_directory(path)? => {
            Ok(())
        }
        Err(error) => {
            Err(error).with_context(|| format!("create ktfmt state directory {}", path.display()))
        }
    }
}

/// The persistent lock file must never be unlinked while another process waits.
fn acquire_lock(state: &Path) -> Result<File> {
    let path = state.join("install.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            bail!("ktfmt lock is not a regular file: {}", path.display())
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    fs2::FileExt::lock_exclusive(&file).context("lock ktfmt installation")?;
    Ok(file)
}

/// Remove abandoned work only after acquiring this release's install lock.
fn cleanup_staging(state: &Path) -> Result<()> {
    for entry in fs::read_dir(state)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(".stage-"))
        {
            remove_owned(&entry.path())?;
        }
    }
    Ok(())
}

/// Remove only the managed entry itself when it is a link.
fn remove_owned(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(path).or_else(|_| fs::remove_dir(path))?;
        }
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
