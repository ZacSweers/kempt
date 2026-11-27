// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "cache")]

use anyhow::{bail, Result};
use kempt_core::cache::{ArtifactFormat, ArtifactRequest, Cache, Downloader};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex};

struct FakeDownloader {
    payload: Vec<u8>,
    calls: Mutex<Vec<PathBuf>>,
}
impl FakeDownloader {
    fn new(payload: Vec<u8>) -> Self {
        Self {
            payload,
            calls: Mutex::new(Vec::new()),
        }
    }
}
impl Downloader for FakeDownloader {
    fn download(&self, _url: &str, dest: &Path) -> Result<()> {
        self.calls.lock().unwrap().push(dest.into());
        std::fs::write(dest, &self.payload)?;
        Ok(())
    }
}

fn request(key: &str, format: ArtifactFormat) -> ArtifactRequest {
    ArtifactRequest {
        key: key.into(),
        url: "https://example.invalid/tool".into(),
        format,
    }
}

#[test]
fn namespaces_and_versioned_keys_are_independent() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let first = cache.namespace("first-tool").unwrap();
    let second = cache.namespace("second-tool").unwrap();
    let downloader = FakeDownloader::new(b"artifact".to_vec());
    let artifact = request("tool-1-linux", ArtifactFormat::File);
    let path = first.ensure(&artifact, &downloader).unwrap();
    assert_eq!(first.ensure(&artifact, &downloader).unwrap(), path);
    let other_version = first
        .ensure(&request("tool-2-linux", ArtifactFormat::File), &downloader)
        .unwrap();
    let other_platform = first
        .ensure(&request("tool-1-macos", ArtifactFormat::File), &downloader)
        .unwrap();
    let other_namespace = second.ensure(&artifact, &downloader).unwrap();
    assert_ne!(path, other_version);
    assert_ne!(path, other_platform);
    assert_ne!(path, other_namespace);
    assert_eq!(downloader.calls.lock().unwrap().len(), 4);
    let removed = first.prune(std::slice::from_ref(&artifact.key)).unwrap();
    assert_eq!(removed.len(), 2);
    assert!(path.exists());
    assert!(other_namespace.exists());
    assert_eq!(first.list_entries().unwrap()[0].size, 8);
}

#[test]
fn missing_namespace_lists_and_prunes_without_creating_it() {
    let temp = tempfile::tempdir().unwrap();
    let namespace = Cache::new(temp.path().into()).namespace("absent").unwrap();
    assert!(namespace.list_entries().unwrap().is_empty());
    assert!(namespace.prune(&[]).unwrap().is_empty());
    assert!(!namespace.root().exists());
}

#[test]
fn invalid_names_cannot_escape_the_cache() {
    let temp = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    let namespace = cache.namespace("tool").unwrap();
    let downloader = FakeDownloader::new(vec![]);
    for name in [
        "",
        ".",
        "..",
        ".hidden",
        "../outside",
        "/absolute",
        "a/b",
        "a\\b",
        "C:drive",
        "null\0byte",
    ] {
        assert!(cache.namespace(name).is_err(), "namespace: {name:?}");
        assert!(
            namespace
                .ensure(&request(name, ArtifactFormat::File), &downloader)
                .is_err(),
            "key: {name:?}"
        );
        assert!(namespace.prune(&[name.into()]).is_err());
    }
    assert!(downloader.calls.lock().unwrap().is_empty());
}

#[test]
fn incomplete_downloads_are_cleaned_up_and_can_be_retried() {
    struct Interrupted;
    impl Downloader for Interrupted {
        fn download(&self, _url: &str, dest: &Path) -> Result<()> {
            std::fs::write(dest, b"partial")?;
            bail!("interrupted")
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
    let artifact = request("tool-1", ArtifactFormat::Executable);
    assert!(namespace.ensure(&artifact, &Interrupted).is_err());
    assert_eq!(std::fs::read_dir(namespace.root()).unwrap().count(), 0);
    let complete = FakeDownloader::new(b"complete".to_vec());
    let path = namespace.ensure(&artifact, &complete).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"complete");
}

#[test]
fn concurrent_misses_publish_one_completed_artifact() {
    struct ConcurrentDownloader {
        barrier: Barrier,
        paths: Mutex<Vec<PathBuf>>,
    }
    impl Downloader for ConcurrentDownloader {
        fn download(&self, _url: &str, dest: &Path) -> Result<()> {
            self.paths.lock().unwrap().push(dest.into());
            std::fs::write(dest, b"partial")?;
            self.barrier.wait();
            std::fs::write(dest, b"completed artifact")?;
            Ok(())
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
    let downloader = Arc::new(ConcurrentDownloader {
        barrier: Barrier::new(2),
        paths: Mutex::new(Vec::new()),
    });
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let namespace = namespace.clone();
            let downloader = downloader.clone();
            std::thread::spawn(move || {
                namespace
                    .ensure(
                        &request("tool-1", ArtifactFormat::Executable),
                        downloader.as_ref(),
                    )
                    .unwrap()
            })
        })
        .collect();
    let paths: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(paths[0], paths[1]);
    assert_eq!(std::fs::read(&paths[0]).unwrap(), b"completed artifact");
    let downloads = downloader.paths.lock().unwrap();
    assert_ne!(downloads[0], downloads[1]);
    assert_eq!(std::fs::read_dir(namespace.root()).unwrap().count(), 1);
}

fn tar_payload(names: &[&str]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for name in names {
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, name, &b"binary"[..])
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

fn zip_payload(names: &[&str]) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for name in names {
        archive
            .start_file(*name, zip::write::FileOptions::default())
            .unwrap();
        archive.write_all(b"binary").unwrap();
    }
    archive.finish().unwrap().into_inner()
}

#[test]
fn archives_retain_only_the_selected_executable() {
    for (format, payload) in [
        (
            ArtifactFormat::TarGzExecutable {
                executable: "tool".into(),
            },
            tar_payload(&["release/tool", "readme"]),
        ),
        (
            ArtifactFormat::ZipExecutable {
                executable: "tool".into(),
            },
            zip_payload(&["release/tool", "readme"]),
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
        let path = namespace
            .ensure(&request("tool-1", format), &FakeDownloader::new(payload))
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"binary");
        assert_eq!(std::fs::read_dir(namespace.root()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_ne!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o111,
                0
            );
        }
    }
}

#[test]
fn missing_ambiguous_and_invalid_archives_never_become_cache_hits() {
    for format in [
        ArtifactFormat::TarGzExecutable {
            executable: "tool".into(),
        },
        ArtifactFormat::ZipExecutable {
            executable: "tool".into(),
        },
    ] {
        for names in [&["other"][..], &["first/tool", "second/tool"][..]] {
            let payload = match format {
                ArtifactFormat::TarGzExecutable { .. } => tar_payload(names),
                _ => zip_payload(names),
            };
            let temp = tempfile::tempdir().unwrap();
            let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
            assert!(namespace
                .ensure(
                    &request("tool-1", format.clone()),
                    &FakeDownloader::new(payload)
                )
                .is_err());
            assert_eq!(std::fs::read_dir(namespace.root()).unwrap().count(), 0);
        }
        let temp = tempfile::tempdir().unwrap();
        let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
        assert!(namespace
            .ensure(
                &request("tool-1", format),
                &FakeDownloader::new(b"not an archive".to_vec())
            )
            .is_err());
        assert_eq!(std::fs::read_dir(namespace.root()).unwrap().count(), 0);
    }
}

#[test]
fn zip_paths_and_tar_links_are_not_extracted() {
    let temp = tempfile::tempdir().unwrap();
    let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
    let format = ArtifactFormat::ZipExecutable {
        executable: "tool".into(),
    };
    assert!(namespace
        .ensure(
            &request("tool-1", format),
            &FakeDownloader::new(zip_payload(&["../tool"]))
        )
        .is_err());
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_link_name("outside").unwrap();
    header.set_cksum();
    archive
        .append_data(&mut header, "tool", std::io::empty())
        .unwrap();
    let payload = archive.into_inner().unwrap().finish().unwrap();
    assert!(namespace
        .ensure(
            &request(
                "tool-1",
                ArtifactFormat::TarGzExecutable {
                    executable: "tool".into()
                }
            ),
            &FakeDownloader::new(payload)
        )
        .is_err());
    assert_eq!(std::fs::read_dir(namespace.root()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn linked_namespaces_and_artifacts_are_rejected() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let cache = Cache::new(temp.path().into());
    symlink(outside.path(), temp.path().join("linked")).unwrap();
    assert!(cache.namespace("linked").is_err());
    let namespace = cache.namespace("tool").unwrap();
    std::fs::create_dir_all(namespace.root()).unwrap();
    std::fs::write(outside.path().join("file"), b"external").unwrap();
    symlink(outside.path().join("file"), namespace.root().join("tool-1")).unwrap();
    assert!(namespace
        .ensure(
            &request("tool-1", ArtifactFormat::File),
            &FakeDownloader::new(vec![])
        )
        .is_err());
    assert!(namespace.list_entries().unwrap().is_empty());
    assert!(namespace.prune(&[]).unwrap().is_empty());
    assert!(outside.path().join("file").exists());
}

#[cfg(unix)]
#[test]
fn raw_executables_are_executable_on_download_and_cache_hits() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let namespace = Cache::new(temp.path().into()).namespace("tool").unwrap();
    let downloader = FakeDownloader::new(b"binary".to_vec());
    let artifact = request("tool-1", ArtifactFormat::Executable);
    let path = namespace.ensure(&artifact, &downloader).unwrap();
    assert_ne!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
        0
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    namespace.ensure(&artifact, &downloader).unwrap();
    assert_ne!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o111,
        0
    );
    assert_eq!(downloader.calls.lock().unwrap().len(), 1);
}
