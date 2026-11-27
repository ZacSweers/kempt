# Contributing

## Prerequisites

- Rust stable (`rustup default stable`)
- Java 17+ on `PATH` only if you want to manually exercise the formatters; the
  test suite does not need it

## Common commands

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

CI runs all four. Treat clippy warnings as errors locally too; the CI job
does.

## Local kempt setup

This repo has its own `.kempt.toml`. To use the same formatting pipeline before
commits, install the local binary and then install the hook:

```sh
cargo install --path .
kempt install-hook --force
kempt check --all
```

The installed hook lives at `.git/hooks/pre-commit` and runs `kempt hook` from
your `PATH`. Since git does not track files under `.git/hooks`, each clone opts in by
running `kempt install-hook`.

## Module layout

`kempt-fmt` is the CLI package at the repository root. `kempt-core` contains the library, its tests, and examples under `crates/kempt-core`. The module list below refers to `crates/kempt-core/src` unless stated otherwise. Each module has its own tests.

- `config.rs`: TOML schema and parser. Pure.
- `license.rs`: license header detection, year substitution, insertion-point
  logic. Pure string transforms.
- `whitespace.rs`: trailing whitespace and final-newline normalization. Pure.
- `git.rs`: `GitContext` trait, `RealGit` shell-out implementation, and a
  `FakeGit` test double under `#[cfg(test)] pub mod testing`.
- `paths.rs`: file collection and glob filtering. Tested with `FakeGit`.
- `cache.rs`: namespaced artifact caching and the public `Downloader` trait. Network is never touched in tests.
- `cache/ktfmt.rs`: pure ktfmt release resolution and checksum-bound installs, shared by the CLI and embedding callers.
- `formatter_cache.rs`: CLI formatter selection, other formatter URLs, and legacy cache management.
- `api.rs`: public typed command requests, execution contexts, and results.
- `process_output.rs`: capture of subprocess output for one execution.
- `formatters.rs`: command-line builders for ktfmt and gjf, plus the
  process runners for JVM jars and native binaries.
- `pipeline.rs`: orchestration of the in-process steps (license header,
  whitespace) on a single file's contents.
- `hook.rs`: partial-staging detection and pre-commit installer.
- `commands.rs`: subcommand implementations. Takes traits for everything
  external (`GitContext`, `Downloader`) so end-to-end tests run with fakes.
- `lib.rs`: feature-gated public APIs and private implementation modules.
- `cli.rs`: argument parsing and rendering to caller-owned writers.
- Root `src/main.rs`: executable adapter for the library APIs.
- Root `tests/cli.rs`: executable smoke tests.

## Testing approach

Tests use fakes, not mocks. The two key fakes:

- `crate::git::testing::FakeGit`: set staged, unstaged, and tracked file
  lists in tests; intercepts `git add` calls so tests can assert on them.
- `crate::formatter_cache::testing::FakeDownloader`: writes a fixed payload to the
  destination path instead of hitting the network. Records call URLs.

End-to-end tests for `run_format`, `run_hook`, and `run_init` run against
real tempdirs with the fakes wired in. No network, no JVM, no real git.

When adding a new feature, the order is usually:

1. Add a pure function (or trait method on an existing boundary).
2. Unit-test it.
3. Wire it into `commands.rs`.
4. Add a `commands.rs` test that exercises the new path with the fakes.

## Adding a new formatter

The pattern lives in `formatters.rs` and `cache.rs`:

1. Add a config section in `config.rs`.
2. Add formatter-specific paths and artifact requests to `formatter_cache.rs`.
3. Add `<tool>_args()` to `formatters.rs`.
4. Wire it into `apply_jvm_formatters` in `commands.rs`.

Don't add a new "runner" trait unless tests need to replace process
execution. The existing config/cache/formatter boundaries usually cover it.

## Manual smoke test

The unit tests cover logic with fakes; this recipe exercises the real binary
end-to-end against an actual Kotlin/Java repo. Useful before cutting a
release or after touching anything in the formatter, cache, or hook paths.

You'll need:

- A clone of any Kotlin/Java repo with no existing `.kempt.toml`.
- A working `java` (JDK 17+) on `PATH`.

```sh
# 1. Build the release binary.
cargo build --release
KEMPT=$(pwd)/target/release/kempt

# 2. Set up the target repo.
cd /path/to/some/kotlin-or-java-repo
$KEMPT init

# Edit .kempt.toml to set ktfmt/gjf versions matching what the repo
# already uses (or leave the starter values).

# 3. Read-only checks. None of these mutate the repo.
$KEMPT check                          # exits 1 if anything would change
$KEMPT format --dry-run               # equivalent
$KEMPT update                         # populates ~/.kempt/cache
$KEMPT cache list                     # lists cached formatter artifacts
$KEMPT vendor --dir /tmp/kempt-out    # copies artifacts + prints `path = ...` snippet
```

**Capture exit codes correctly:** `$KEMPT check | tail` gives you `tail`'s
exit code, not kempt's. Use `$KEMPT check; echo $?` or `${PIPESTATUS[0]}`.

**End-to-end `path = ...` test** in a throwaway repo:

```sh
mkdir /tmp/kempt-test && cd /tmp/kempt-test && git init -q
$KEMPT vendor --dir vendor             # populates vendor/ with artifacts
# Paste the printed snippet into .kempt.toml.
mkdir src && printf 'package x\n\nclass    Foo\n' > src/Foo.kt
git add -A && git commit -q -m init
$KEMPT check; echo $?                  # exit 1 (Foo.kt has bad spacing)
```

**What to verify when changing exclude or path-collection logic:**

- `[paths].exclude` patterns actually filter (e.g., `**/build/**` keeps
  build outputs out of the file list).
- License-header excludes use glob matching (regression: `**/Foo.kt` once
  failed to match `a/b/c/Foo.kt`).
- Adding a `[ktfmt.license-header].excludes` entry stops header insertion
  on that file but still runs ktfmt on it.

**Mutating commands** (skip on a working tree you care about):

```sh
$KEMPT format                          # mass-modifies in place
$KEMPT install-hook                    # writes .git/hooks/pre-commit
git commit -m "test"                   # exercises the hook
```

**Cleanup after testing:**

```sh
rm -rf ~/.kempt/cache                  # formatter artifacts, regeneratable
rm /path/to/test-repo/.kempt.toml      # if you scaffolded into a real repo
```

## Releasing

`dist-workspace.toml` is configured for `cargo dist`. Tagging a release pushes
binaries to the GitHub release and updates the Homebrew tap. See
[axodotdev/cargo-dist](https://github.com/axodotdev/cargo-dist) for the full
workflow. CI runs the dist plan on PRs.

## Style

- No emoji in code or docs.
- Comments only when the why is non-obvious.
- Tests get descriptive names, not `test_1` / `test_2`.
- Clippy warnings are errors. If a lint does not fit a case, allow it inline
  with a one-line comment explaining why.

## Library feature validation

CI builds, tests, and runs clippy for the workspace, the library defaults without HTTP/TLS, and commands-only library consumers. Run library feature checks with `-p kempt-core` so the CLI dependency does not enable extra features. Examples live in `crates/kempt-core/examples`.

```sh
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo build -p kempt-core --all-targets
cargo test -p kempt-core --all-targets
cargo clippy -p kempt-core --all-targets -- -D warnings
cargo build -p kempt-core --all-targets --no-default-features --features commands
cargo test -p kempt-core --all-targets --no-default-features --features commands
cargo clippy -p kempt-core --all-targets --no-default-features --features commands -- -D warnings
```
