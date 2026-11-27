// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Embed ktfmt with a caller-owned downloader and a reviewed release checksum.
use kempt_core::cache::{ktfmt, Cache, Downloader};
use std::path::Path;

struct LocalDownload(std::path::PathBuf);

impl Downloader for LocalDownload {
    fn download(&self, _url: &str, destination: &Path) -> anyhow::Result<()> {
        // Replace this with the consumer's proxy, certificate, and retry policies.
        std::fs::copy(&self.0, destination)?;
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.len() == 3,
        "usage: ktfmt_embed CACHE_ROOT REVIEWED_SHA256 RELEASE_FILE"
    );
    let artifact = ktfmt::resolve(
        "0.65",
        ktfmt::Platform::current(),
        ktfmt::RuntimeKind::Native,
    )?;
    println!(
        "reviewed asset: {} ({})",
        artifact.asset_name(),
        artifact.source_url()
    );
    let checksum = args[1]
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("checksum must be hexadecimal"))?;
    let installed = artifact.ensure(
        &Cache::new(args[0].clone().into()),
        checksum,
        &LocalDownload(args[2].clone().into()),
    )?;
    println!("{}", installed.path().display());
    Ok(())
}
