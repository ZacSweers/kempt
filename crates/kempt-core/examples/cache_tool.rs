// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use kempt_core::cache::{ArtifactFormat, ArtifactRequest, Cache, UreqDownloader};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or_else(|| anyhow::anyhow!("usage: cache_tool URL IMMUTABLE_KEY"))?;
    let key = args.next().ok_or_else(|| anyhow::anyhow!("provide a versioned artifact key"))?;
    let namespace = Cache::new(Cache::default_root()?).namespace("example-tool")?;
    let path = namespace.ensure(&ArtifactRequest { key, url, format: ArtifactFormat::Executable }, &UreqDownloader)?;
    println!("{}", path.display());
    Ok(())
}
