# kempt-core

`kempt-core` provides reusable formatter caches and in-process command APIs. The `kempt-fmt` CLI uses this library. Use the local checkout while developing against these APIs:

```toml
[dependencies]
anyhow = "1"
kempt-core = { path = "../kempt/crates/kempt-core" }
```

The default `cache` feature exposes ktfmt release resolution and verified installation. It includes no CLI, HTTP, or TLS dependencies. Supply your own `Downloader` to use your network policies. The optional `http` feature adds `UreqDownloader`. The `commands` feature adds typed command execution and HTTP support. The `cli` feature adds argument parsing and rendering for in-process callers.

## Embedding ktfmt

Resolve a release before looking up a reviewed checksum. Resolution reads no cache, environment settings, or network resources. Native selection fails for unsupported hosts or versions. The consumer decides whether to resolve `RuntimeKind::Jar` instead.

```rust
use kempt_core::cache::{ktfmt, Cache, Downloader};
use std::path::PathBuf;

fn install_ktfmt(
    cache_root: PathBuf,
    downloader: &dyn Downloader,
    reviewed_pin: impl FnOnce(&ktfmt::ReleaseArtifact) -> anyhow::Result<String>,
) -> anyhow::Result<ktfmt::InstalledArtifact> {
    let artifact = ktfmt::resolve("0.65", ktfmt::Platform::current(), ktfmt::RuntimeKind::Native)?;
    // The consumer can inspect asset_name(), source_url(), runtime(), and platform().
    let expected_sha256 = reviewed_pin(&artifact)?;
    artifact.ensure(&Cache::new(cache_root), &expected_sha256, downloader)
}
```

The expected SHA-256 must contain 64 hexadecimal digits and cover the release download. For native assets, pin the compressed archive. The JAR asset is `ktfmt-<version>-with-dependencies.jar`. Installation returns `InstalledArtifact::NativeExecutable` or `InstalledArtifact::StandaloneJar`. The consumer selects Java, arguments, files, and output handling.

Kempt locks each release across processes, checks the checksum before extraction, extracts one regular executable, sets permissions, and atomically publishes the artifact with its receipt. Download, checksum, and extraction errors are returned without changing the selected runtime. Concurrent callers with the same pin share one download. Different pins have distinct paths, so a changed pin cannot replace an earlier returned path.

Use the same root as the CLI, normally `Cache::default_root()`, to share installations. Canonical installs and receipts live inside that root. The CLI retains its flat formatter filenames, using hard links to canonical files where possible. Existing JARs can be adopted offline after verifying their bytes. An older native install without a receipt needs one verified download because the executable cannot prove the archive checksum. CLI cache pruning also evicts the corresponding canonical installs.

On every cache hit, kempt checks the receipt's release identity and expected checksum, then hashes the installed bytes. Valid hits work offline. Missing, partial, or corrupt entries are repaired on the next successful install. Receipts are trusted local metadata. This detects accidental damage and file changes, but does not protect against an attacker who can rewrite both receipts and installed files. Keep the cache writable only by trusted users. Locks coordinate cooperating processes. Pins remain the consumer's policy. The unpinned CLI continues to use its existing version and runtime policy.

`examples/ktfmt_embed.rs` demonstrates a caller-provided downloader with the library-only feature set. The older namespaced download API remains available for other artifacts and uses caller-owned immutable keys.

## Invoking typed commands

Enable `features = ["commands"]` for typed calls:

```rust
use kempt_core::commands::{execute, CheckOptions, Command, CommandRequest, ExecutionContext};

fn main() -> anyhow::Result<()> {
    let context = ExecutionContext::from_env(std::env::current_dir()?)?;
    let request = CommandRequest::new(Command::Check(CheckOptions::default()));
    let outcome = execute(&request, &context)?;
    println!("check status: {}", outcome.exit_code());
    Ok(())
}
```

Every existing subcommand has a typed request and result. File selection uses `FileScope` for tracked, staged, touched, walked, or explicit files. Explicit targets and configuration overrides resolve from the execution directory. Vendor destinations resolve from the repository root. Formatting retains the CLI's Git requirements.

Execution is quiet and never changes the process's working directory or environment. `CommandOutcome` contains command-specific data and captured subprocess output. Check failures return an outcome with status 1. Operational failures return `RunError` with the underlying error, captured output, and additional diagnostics. Check and dry-run modes leave source files unchanged, but may populate the tool cache.

`ExecutionContext::from_env` reads the existing cache and experimental hook defaults once. `ExecutionContext::new` accepts an explicit cache and disables experimental hook options by default. Construct a context directly to supply an absolute working directory, custom cache, downloader, version fetcher, year, and hook options.

## Invoking CLI arguments

Enable `features = ["cli"]`:

```rust
use kempt_core::{cli, commands};

fn main() -> anyhow::Result<()> {
    match cli::parse_from(["kempt", "check", "--staged"])? {
        cli::ParseOutcome::Command(request) => {
            let context = commands::ExecutionContext::from_env(std::env::current_dir()?)?;
            let outcome = commands::execute(&request, &context)?;
            cli::render(&outcome, &mut std::io::stdout(), &mut std::io::stderr())?;
        }
        cli::ParseOutcome::Display { text, .. } => print!("{text}"),
    }
    Ok(())
}
```

`parse_from` accepts arguments including the program name and preserves non-UTF-8 paths through `OsString`. It returns help and version text without printing or exiting. Parsing errors are returned to the caller. `render` and `render_error` write CLI text to caller-owned writers when terminal output is wanted.

The `examples/` directory contains standalone consumers for each feature level.

## License

Apache 2.0. See [LICENSE.txt](LICENSE.txt).
