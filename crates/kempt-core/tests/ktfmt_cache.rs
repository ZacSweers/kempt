// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "cache")]

use anyhow::{bail, Result};
use kempt_core::cache::{ktfmt::{self, InstalledArtifact, Platform, RuntimeKind}, Cache, Downloader};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{atomic::{AtomicUsize, Ordering}, Arc, Barrier};

struct FakeDownloader {
    payload: Vec<u8>,
    calls: AtomicUsize,
}

impl FakeDownloader {
    fn new(payload: Vec<u8>) -> Self { Self { payload, calls: AtomicUsize::new(0) } }
}

impl Downloader for FakeDownloader {
    fn download(&self, _url: &str, destination: &Path) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::fs::write(destination, &self.payload)?;
        Ok(())
    }
}

struct Offline;
impl Downloader for Offline {
    fn download(&self, _url: &str, _destination: &Path) -> Result<()> { bail!("offline") }
}

fn checksum(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn platform(os: &str, arch: &str) -> Platform { Platform { os: os.into(), arch: arch.into() } }
fn native() -> ktfmt::ReleaseArtifact {
    ktfmt::resolve("0.65", platform("linux", "x86_64"), RuntimeKind::Native).unwrap()
}
fn jar() -> ktfmt::ReleaseArtifact {
    ktfmt::resolve("0.65", Platform::current(), RuntimeKind::Jar).unwrap()
}

fn tar_payload(names: &[&str], bytes: &[u8]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for name in names {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive.append_data(&mut header, name, bytes).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

fn zip_payload() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive.start_file("release/ktfmt.exe", zip::write::FileOptions::default()).unwrap();
    archive.write_all(b"windows binary").unwrap();
    archive.finish().unwrap().into_inner()
}

#[test]
fn resolution_exposes_reviewable_assets_for_each_supported_native_host() {
    for (os, arch, name) in [
        ("macos", "aarch64", "ktfmt-macos-aarch64-0.65.tar.gz"),
        ("linux", "x86_64", "ktfmt-linux-x86_64-0.65.tar.gz"),
        ("windows", "x86_64", "ktfmt-windows-x86_64-0.65.zip"),
    ] {
        let host = platform(os, arch);
        let artifact = ktfmt::resolve("0.65", host.clone(), RuntimeKind::Native).unwrap();
        assert_eq!(artifact.asset_name(), name);
        assert_eq!(artifact.source_url(), format!("https://github.com/kotlin/ktfmt/releases/download/v0.65/{name}"));
        assert_eq!(artifact.runtime(), RuntimeKind::Native);
        assert_eq!(artifact.platform(), &host);
        assert_eq!(artifact.version(), "0.65");
    }
    for host in [platform("macos", "x86_64"), platform("linux", "aarch64"), platform("unknown", "unknown")] {
        assert!(ktfmt::resolve("0.65", host.clone(), RuntimeKind::Native).is_err());
        let fallback = ktfmt::resolve("0.65", host, RuntimeKind::Jar).unwrap();
        assert_eq!(fallback.runtime(), RuntimeKind::Jar);
        assert_eq!(fallback.asset_name(), "ktfmt-0.65-with-dependencies.jar");
    }
    assert!(ktfmt::resolve("0.64", Platform::current(), RuntimeKind::Native).is_err());
    for version in ["", "../0.65", "0.65\n", "v0.65"] {
        assert!(ktfmt::resolve(version, Platform::current(), RuntimeKind::Jar).is_err());
    }
}

#[test]
fn cold_native_and_jar_installs_are_typed_and_warm_installs_are_offline() {
    let archive = tar_payload(&["release/ktfmt", "README"], b"native binary");
    let windows = ktfmt::resolve("0.65", platform("windows", "x86_64"), RuntimeKind::Native).unwrap();
    for (artifact, payload, installed_bytes) in [
        (native(), archive, b"native binary".to_vec()),
        (windows, zip_payload(), b"windows binary".to_vec()),
        (jar(), b"standalone jar".to_vec(), b"standalone jar".to_vec()),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cache = Cache::new(temp.path().into());
        let expected = checksum(&payload);
        let downloader = FakeDownloader::new(payload);
        let installed = artifact.ensure(&cache, &expected.to_uppercase(), &downloader).unwrap();
        assert_eq!(std::fs::read(installed.path()).unwrap(), installed_bytes);
        match (&installed, artifact.runtime()) {
            (InstalledArtifact::NativeExecutable(_), RuntimeKind::Native) => {}
            (InstalledArtifact::StandaloneJar(_), RuntimeKind::Jar) => {}
            _ => panic!("runtime changed"),
        }
        assert_eq!(artifact.ensure(&cache, &expected, &Offline).unwrap(), installed);
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
        #[cfg(unix)]
        if artifact.runtime() == RuntimeKind::Native {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(std::fs::metadata(installed.path()).unwrap().permissions().mode() & 0o111, 0);
            std::fs::set_permissions(installed.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
            artifact.ensure(&cache, &expected, &Offline).unwrap();
            assert_ne!(std::fs::metadata(installed.path()).unwrap().permissions().mode() & 0o111, 0);
        }
    }
}

#[test]
fn wrong_checksum_is_reported_before_archive_extraction_and_publishes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let downloader = FakeDownloader::new(b"not even an archive".to_vec());
    let error = native().ensure(&cache, &"0".repeat(64), &downloader).unwrap_err();
    assert!(error.to_string().contains("SHA-256 mismatch"));
    assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
    assert!(!temp.path().join("ktfmt-0.65-linux-x86_64").exists());
    assert!(!temp.path().join("ktfmt-0.65.jar").exists());
}

#[test]
fn invalid_pin_is_rejected_before_cache_or_network_access() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().join("absent"));
    for expected in ["", "sha256:abcd", &"g".repeat(64), &"0".repeat(63)] {
        assert!(native().ensure(&cache, expected, &Offline).unwrap_err().to_string().contains("64-digit"));
    }
    assert!(!cache.root().exists());
}

#[test]
fn changed_pins_cannot_reuse_or_replace_a_previously_returned_path() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let artifact = native();
    let first_archive = tar_payload(&["ktfmt"], b"first binary");
    let first = artifact.ensure(&cache, &checksum(&first_archive), &FakeDownloader::new(first_archive.clone())).unwrap();
    let wrong_pin = checksum(b"wrong reviewed pin");
    assert!(artifact.ensure(&cache, &wrong_pin, &FakeDownloader::new(first_archive.clone())).is_err());
    assert_eq!(std::fs::read(first.path()).unwrap(), b"first binary");
    let next_archive = tar_payload(&["ktfmt"], b"next binary");
    let next = artifact.ensure(&cache, &checksum(&next_archive), &FakeDownloader::new(next_archive.clone())).unwrap();
    assert_ne!(first.path(), next.path());
    assert_eq!(std::fs::read(first.path()).unwrap(), b"first binary");
    assert_eq!(std::fs::read(next.path()).unwrap(), b"next binary");
    assert_eq!(artifact.ensure(&cache, &checksum(&first_archive), &Offline).unwrap(), first);
    assert_eq!(artifact.ensure(&cache, &checksum(&next_archive), &Offline).unwrap(), next);
}

#[test]
fn corrupt_missing_and_partial_native_entries_are_repaired() {
    let payload = tar_payload(&["ktfmt"], b"valid binary");
    for damage in ["bytes", "missing file", "missing receipt", "wrong binding", "receipt directory", "entry directory"] {
        let temp = tempfile::tempdir().unwrap();
        let cache = Cache::new(temp.path().into());
        let artifact = native();
        let downloader = FakeDownloader::new(payload.clone());
        let expected = checksum(&payload);
        let installed = artifact.ensure(&cache, &expected, &downloader).unwrap();
        let receipt = installed.path().parent().unwrap().join("receipt");
        match damage {
            "bytes" => std::fs::write(installed.path(), b"truncated").unwrap(),
            "missing file" => std::fs::remove_file(installed.path()).unwrap(),
            "missing receipt" => std::fs::remove_file(&receipt).unwrap(),
            "wrong binding" => {
                let contents = std::fs::read_to_string(&receipt).unwrap().replace(&expected, &"0".repeat(64));
                std::fs::write(&receipt, contents).unwrap();
            }
            "receipt directory" => { std::fs::remove_file(&receipt).unwrap(); std::fs::create_dir(&receipt).unwrap(); }
            "entry directory" => {
                let entry = installed.path().parent().unwrap();
                std::fs::remove_dir_all(entry).unwrap();
                std::fs::write(entry, b"interrupted entry").unwrap();
            }
            _ => unreachable!(),
        }
        let repaired = artifact.ensure(&cache, &expected, &downloader).unwrap();
        assert_eq!(repaired, installed, "{damage}");
        assert_eq!(std::fs::read(repaired.path()).unwrap(), b"valid binary", "{damage}");
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 2, "{damage}");
        artifact.ensure(&cache, &expected, &Offline).unwrap();
    }
}

#[test]
fn corrupt_jar_and_unreceipted_jar_are_verified_before_reuse() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let artifact = jar();
    let payload = b"reviewed standalone jar";
    let expected = checksum(payload);
    let legacy = temp.path().join("ktfmt-0.65.jar");
    std::fs::write(&legacy, payload).unwrap();
    let installed = artifact.ensure(&cache, &expected, &Offline).unwrap();
    assert_eq!(std::fs::read(installed.path()).unwrap(), payload);
    std::fs::write(installed.path(), b"partial jar").unwrap();
    std::fs::write(&legacy, b"partial jar").unwrap();
    let downloader = FakeDownloader::new(payload.to_vec());
    artifact.ensure(&cache, &expected, &downloader).unwrap();
    assert_eq!(std::fs::read(installed.path()).unwrap(), payload);
    assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn unreceipted_native_installs_are_not_trusted_as_archive_proof() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    std::fs::write(temp.path().join("ktfmt-0.65-linux-x86_64"), b"unreviewed executable").unwrap();
    let payload = tar_payload(&["ktfmt"], b"reviewed executable");
    let downloader = FakeDownloader::new(payload.clone());
    let installed = native().ensure(&cache, &checksum(&payload), &downloader).unwrap();
    assert_eq!(std::fs::read(installed.path()).unwrap(), b"reviewed executable");
    assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn download_and_extraction_failures_can_be_retried_without_runtime_fallback() {
    struct Interrupted;
    impl Downloader for Interrupted {
        fn download(&self, url: &str, destination: &Path) -> Result<()> {
            assert!(url.ends_with(".tar.gz"));
            std::fs::write(destination, b"partial")?;
            bail!("interrupted download")
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let artifact = native();
    let valid = tar_payload(&["ktfmt"], b"complete");
    assert!(artifact.ensure(&cache, &checksum(&valid), &Interrupted).unwrap_err().to_string().contains("interrupted"));
    for invalid in [b"invalid archive".to_vec(), tar_payload(&["missing"], b"content"), tar_payload(&["a/ktfmt", "b/ktfmt"], b"ambiguous")] {
        let downloader = FakeDownloader::new(invalid.clone());
        assert!(artifact.ensure(&cache, &checksum(&invalid), &downloader).is_err());
        assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
        assert!(!temp.path().join("ktfmt-0.65.jar").exists());
    }
    let installed = artifact.ensure(&cache, &checksum(&valid), &FakeDownloader::new(valid.clone())).unwrap();
    assert_eq!(std::fs::read(installed.path()).unwrap(), b"complete");
}

#[test]
fn concurrent_threads_share_one_install() {
    struct SlowDownload(FakeDownloader);
    impl Downloader for SlowDownload {
        fn download(&self, url: &str, destination: &Path) -> Result<()> {
            std::thread::sleep(std::time::Duration::from_millis(30));
            self.0.download(url, destination)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let downloader = Arc::new(SlowDownload(FakeDownloader::new(tar_payload(&["ktfmt"], b"concurrent binary"))));
    let expected = checksum(&downloader.0.payload);
    let barrier = Arc::new(Barrier::new(8));
    let workers: Vec<_> = (0..8).map(|_| {
        let cache = Cache::new(temp.path().into());
        let downloader = downloader.clone();
        let expected = expected.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            native().ensure(&cache, &expected, downloader.as_ref()).unwrap()
        })
    }).collect();
    let paths: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert!(paths.iter().all(|path| path == &paths[0]));
    assert_eq!(downloader.0.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn process_install_child() {
    let Some(root) = std::env::var_os("KEMPT_TEST_INSTALL_ROOT") else { return; };
    let root = PathBuf::from(root);
    std::fs::write(root.join(format!("ready-{}", std::process::id())), b"ready").unwrap();
    wait_for(|| root.join("start").exists());
    struct ProcessDownload(PathBuf);
    impl Downloader for ProcessDownload {
        fn download(&self, _url: &str, destination: &Path) -> Result<()> {
            let mut calls = std::fs::OpenOptions::new().create(true).append(true).open(self.0.join("calls"))?;
            calls.write_all(b"download\n")?;
            std::thread::sleep(std::time::Duration::from_millis(30));
            std::fs::write(destination, b"process-shared jar")?;
            Ok(())
        }
    }
    jar().ensure(&Cache::new(root.join("cache")), &checksum(b"process-shared jar"), &ProcessDownload(root)).unwrap();
}

fn wait_for(condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for child processes");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn concurrent_processes_share_one_install() {
    let temp = tempfile::tempdir().unwrap();
    let mut children: Vec<_> = (0..2).map(|_| std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_install_child", "--nocapture"])
        .env("KEMPT_TEST_INSTALL_ROOT", temp.path())
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()).collect();
    wait_for(|| std::fs::read_dir(temp.path()).unwrap().filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("ready-")).count() == 2);
    std::fs::write(temp.path().join("start"), b"start").unwrap();
    for child in children.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }
    assert_eq!(std::fs::read_to_string(temp.path().join("calls")).unwrap(), "download\n");
}

#[cfg(feature = "commands")]
#[test]
fn cli_and_embedding_use_the_same_installer_in_both_orders() {
    use kempt_core::commands::{self, Command, CommandRequest, ExecutionContext, HookOptions, VersionFetcher};
    struct Versions;
    impl VersionFetcher for Versions {
        fn latest_ktfmt(&self) -> Result<String> { unreachable!() }
        fn latest_gjf(&self) -> Result<String> { unreachable!() }
        fn latest_gradle_dependencies_sorter(&self) -> Result<String> { unreachable!() }
    }
    let mut artifacts = vec![(jar(), b"shared jar".to_vec())];
    if let Ok(artifact) = ktfmt::resolve("0.65", Platform::current(), RuntimeKind::Native) {
        let payload = if artifact.asset_name().ends_with(".zip") { zip_payload() }
            else { tar_payload(&["ktfmt"], b"shared native") };
        artifacts.push((artifact, payload));
    }
    for (artifact, payload) in artifacts {
        for cli_first in [true, false] {
            let temp = tempfile::tempdir().unwrap();
            let git = std::process::Command::new("git").args(["init", "-q"]).current_dir(temp.path()).output().unwrap();
            assert!(git.status.success());
            let mode = if artifact.runtime() == RuntimeKind::Jar { "never" } else { "always" };
            std::fs::write(temp.path().join(".kempt.toml"), format!("[ktfmt]\nversion = \"0.65\"\nnative = \"{mode}\"\n")).unwrap();
            let downloader = FakeDownloader::new(payload.clone());
            let context = ExecutionContext {
                working_directory: temp.path().into(), cache: Cache::new(temp.path().join("cache")),
                downloader: &downloader, version_fetcher: &Versions, year: 2026, hook_options: HookOptions::default(),
            };
            let update = CommandRequest::new(Command::Update);
            let expected = checksum(&payload);
            if cli_first { commands::execute(&update, &context).unwrap(); }
            let installed = artifact.ensure(&context.cache, &expected, &downloader).unwrap();
            commands::execute(&update, &context).unwrap();
            assert_eq!(downloader.calls.load(Ordering::SeqCst), 1);
            assert!(installed.path().is_file());
            commands::execute(&CommandRequest::new(Command::CachePrune(commands::PruneOptions { all: true })), &context).unwrap();
            assert!(!installed.path().exists());
        }
    }
}

#[test]
fn a_damaged_cli_index_can_be_repaired_offline() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let payload = tar_payload(&["ktfmt"], b"valid binary");
    let expected = checksum(&payload);
    let artifact = native();
    let installed = artifact.ensure(&cache, &expected, &FakeDownloader::new(payload)).unwrap();
    let state = installed.path().parent().unwrap().parent().unwrap();
    let pointer = state.join("current");
    std::fs::remove_file(&pointer).unwrap();
    std::fs::create_dir(&pointer).unwrap();
    let abandoned = state.join(".stage-interrupted");
    std::fs::create_dir(&abandoned).unwrap();
    std::fs::write(abandoned.join("download"), b"partial").unwrap();
    let repaired = artifact.ensure(&cache, &expected, &Offline).unwrap();
    assert_eq!(repaired, installed);
    assert!(pointer.is_file());
    assert!(!abandoned.exists());
}

#[cfg(unix)]
#[test]
fn a_linked_cache_entry_is_repaired_without_modifying_its_target() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let payload = tar_payload(&["ktfmt"], b"verified binary");
    let downloader = FakeDownloader::new(payload.clone());
    let expected = checksum(&payload);
    let artifact = native();
    let installed = artifact.ensure(&cache, &expected, &downloader).unwrap();
    let external = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(external.path(), b"external file").unwrap();
    std::fs::remove_file(installed.path()).unwrap();
    symlink(external.path(), installed.path()).unwrap();
    artifact.ensure(&cache, &expected, &downloader).unwrap();
    assert_eq!(std::fs::read(installed.path()).unwrap(), b"verified binary");
    assert_eq!(std::fs::read(external.path()).unwrap(), b"external file");
}

#[test]
fn jar_receipts_cannot_substitute_another_installed_checksum_for_the_reviewed_pin() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let payload = b"reviewed jar";
    let expected = checksum(payload);
    let downloader = FakeDownloader::new(payload.to_vec());
    let artifact = jar();
    let installed = artifact.ensure(&cache, &expected, &downloader).unwrap();
    let receipt = installed.path().parent().unwrap().join("receipt");
    let mut fields: Vec<String> = std::fs::read_to_string(&receipt).unwrap().lines().map(String::from).collect();
    fields[3] = checksum(b"different jar");
    std::fs::write(&receipt, format!("{}\n", fields.join("\n"))).unwrap();
    std::fs::write(installed.path(), b"different jar").unwrap();
    std::fs::write(temp.path().join("ktfmt-0.65.jar"), b"different jar").unwrap();
    artifact.ensure(&cache, &expected, &downloader).unwrap();
    assert_eq!(std::fs::read(installed.path()).unwrap(), payload);
    assert_eq!(downloader.calls.load(Ordering::SeqCst), 2);
}
