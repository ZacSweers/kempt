# kempt

A pre-commit-friendly multi-language source formatter. Wraps
[ktfmt](https://github.com/facebook/ktfmt) and
[google-java-format](https://github.com/google/google-java-format), inserts
license headers, and normalizes trailing whitespace. Configured per repo via
`.kempt.toml`.

## Install

> The binary is named `kempt` everywhere. The crate is published as
> `kempt-fmt` because the shorter `kempt` name is already taken on
> crates.io by an unrelated project; that suffix shows up in the install
> URLs and the homebrew formula filename below.

### Homebrew (macOS, Linux)

```sh
brew install ZacSweers/tap/kempt-fmt
```

### Shell installer

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/ZacSweers/kempt/releases/latest/download/kempt-fmt-installer.sh | sh
```

### Cargo

```sh
cargo install kempt-fmt
```

### Notes

A working `java` (JDK 17+) on `PATH` is required to run ktfmt and gjf (unless using `native`).

## Quick start

In an existing repo:

```sh
kempt init             # writes .kempt.toml + config/license-header.txt
kempt install-hook     # writes .git/hooks/pre-commit
kempt format           # format everything once
```

`kempt init` scans the repo and tailors the starter config: `[ktfmt]` is
emitted only when `.kt`/`.kts` files exist, `[gjf]` only when `.java` files
exist. An empty repo gets both. The versions written into the starter are
the latest available at the time kempt was built; an automated workflow
keeps them current.

`kempt check` is the read-only variant. It exits non-zero if any file would
change. That's what you want in CI formatting checks.

## Configuration

`.kempt.toml` at the repo root. Every section is optional, missing sections
disable that step.

```toml
[ktfmt]
version = "0.62"
style = "google"           # google | kotlinlang | meta

[gjf]
version = "1.35.0"
style = "google"           # google | aosp
native = "auto"            # auto | always | never

[license-header]
file = "config/license-header.txt"             # supports ${YEAR}

# Optional per-tool overrides. Either field may be omitted; the global
# [license-header].file is the fallback for `file`.
[ktfmt.license-header]
excludes = "config/license-excludes-kt.txt"

[gjf.license-header]
file = "config/license-header-java.txt"        # overrides global for .java
excludes = "config/license-excludes-java.txt"

[paths]
# Universal exclude applied before any tool's own filter. Inline array OR
# a path to a text file (one glob per line, `#` comments).
exclude = ["**/build/**", "**/target/**"]

# Per-tool path scope. Each tool has its own include/exclude with sensible
# language defaults; you only set these to narrow further.
[ktfmt.paths]
# defaults: include = ["**/*.kt", "**/*.kts"], exclude = []
include = ["**/src/**/*.kt", "**/src/**/*.kts"]
exclude = "config/ktfmt-skip.txt"   # polymorphic: array or file path

[gjf.paths]
# defaults: include = ["**/*.java"], exclude = []
exclude = ["**/*Generated.java"]

[whitespace.paths]
# defaults: include = ["**/*.kt", "**/*.kts", "**/*.java"], exclude = []

[whitespace]
strip-trailing = true   # strip trailing space/tab/CR from every line
final-newline = true    # ensure files end with one trailing newline

[hook]
mode = "format"            # format | check
```

The license header file is a literal template. `${YEAR}` is expanded at write-time.

`[license-header]` sets the default template used by every language.
`[ktfmt.license-header]` and `[gjf.license-header]` override per tool: `file`
swaps in a different template for that tool's languages, and `excludes`
points at an exclude-list specific to that tool. Either field is optional.
If neither the global section nor the tool override supplies a `file`, no
header is inserted for that language.

The exclude files are plain text, one glob per line, `#` comments allowed.

### Two kinds of excludes

kempt has two exclude mechanisms because they answer different questions:

| Where                                                                              | Question it answers                          | Example use                                                          |
|------------------------------------------------------------------------------------|----------------------------------------------|----------------------------------------------------------------------|
| `[paths].exclude` (inline list)                                                    | "Should kempt touch this file at all?"       | Build output, test fixtures, generated code, vendored upstream files |
| `[ktfmt.license-header].excludes` / `[gjf.license-header].excludes` (file pointer) | "Should kempt insert a header in this file?" | Files with their own license header that should still be formatted   |

If a file matches `[paths].exclude`, kempt skips it completely, no formatter and
no header. If a file is in a license-header excludes file but not in
`[paths].exclude`, kempt still formats it (ktfmt or gjf), it just won't
prepend a header.

When in doubt, prefer `[paths].exclude`. Reach for the license-header excludes
only when you genuinely want the formatter to run but the header to stay off
(rare in practice).

### Per-tool path scope

Each tool has its own `paths.include` / `paths.exclude` with language
defaults so you only configure these when you need to narrow further:

| Tool                | Default include                       | Default exclude |
|---------------------|---------------------------------------|-----------------|
| `[ktfmt.paths]`     | `["**/*.kt", "**/*.kts"]`             | `[]`            |
| `[gjf.paths]`       | `["**/*.java"]`                       | `[]`            |
| `[whitespace.paths]`| `["**/*.kt", "**/*.kts", "**/*.java"]`| `[]`            |

The global `[paths].exclude` is applied first as a universal filter; each
tool's own `include` / `exclude` then narrows further. A file is processed
by a given tool iff:

- It is not matched by `[paths].exclude` (universal exclusion), AND
- It is matched by that tool's `paths.include`, AND
- It is not matched by that tool's `paths.exclude`.

License-header insertion is determined by file extension (`.kt`/`.kts`
get the kt header, `.java` gets the java header) plus the per-tool
`license-header.excludes` list. It is intentionally NOT gated on tool path
scope so you can configure `[license-header]` without configuring
`[ktfmt]` and still get headers on kt files.

### Polymorphic include / exclude

Every `include` and `exclude` field accepts either an inline array or a
path to a text file (one glob per line, `#` comments allowed):

```toml
[ktfmt.paths]
include = ["**/*.kt", "**/*.kts"]      # inline
exclude = "config/ktfmt-skip.txt"      # file path

[paths]
exclude = "config/global-excludes.txt" # file path also works for the global

[ktfmt.license-header]
excludes = "config/license-excludes-kt.txt"  # already file-path-only
```

File paths are resolved relative to the repo root. The format is:

```
# header comment
**/Generated.kt
**/build-cache/**
# another comment
**/Skip.kt
```

### File scope

`kempt format` and `kempt check` default to every **tracked** file matching
the include globs (via `git ls-files`). Two flags adjust the file set:

| Flag                     | Effect                                                                                                                                                                      |
|--------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| (none) or `--all`        | All tracked files. The default "format everything" mode. `--all` exists as an explicit alias for symmetry with the other scope flags and so suggestions can be unambiguous. |
| `--staged`               | Files in the index only. Used by the pre-commit hook.                                                                                                                       |
| `--discovery=walk`       | Filesystem walk from the repo root. Includes untracked files. Does NOT consult `.gitignore`. `[paths].exclude` is the only filter.                                          |
| `<file>...` (positional) | Operate on exactly the listed files. Bypasses scope flags and `[paths].include` / `[paths].exclude`. Useful for targeted runs (e.g. `kempt format src/Foo.kt`).             |

`--all`, `--staged`, `--discovery=walk`, and explicit positional paths are
all mutually exclusive.

When `kempt check` (or `kempt format --dry-run`) finds 30 or fewer files
needing formatting, it appends a copy-pasteable command listing those files
specifically. Useful for fixing a small subset locally after a CI failure
without touching the rest of the working tree.

`--discovery=walk` is the escape hatch when you have files git doesn't know
about (recently dropped in, never staged) and want kempt to format them
anyway. It deliberately skips `.gitignore` because if you're explicitly
opting out of VCS-driven discovery, deferring to a VCS-managed ignore file
is incoherent. Use `[paths].exclude` to filter out build outputs and the
like (the defaults already cover `**/build/**` and `**/target/**`). The
`.git/` directory is always pruned regardless of config.

### Config reference

Every option, with default. A `-` in the default column means "no built-in
default; the section that contains it is what enables the feature."

| Key                               | Default                                | Notes                                                                                                                    |
|-----------------------------------|----------------------------------------|--------------------------------------------------------------------------------------------------------------------------|
| `[ktfmt].version`                 | -                                      | Maven Central version. Either a literal `"0.62"` or a catalog reference `{ file, key }`. Mutually exclusive with `path`. |
| `[ktfmt].path`                    | -                                      | Path to a checked-in jar. Mutually exclusive with `version`.                                                             |
| `[ktfmt].style`                   | `"google"`                             | `google` / `kotlinlang` / `meta`                                                                                         |
| `[ktfmt.paths].include`           | `["**/*.kt", "**/*.kts"]`              | Inline array or path to a glob-list file.                                                                                |
| `[ktfmt.paths].exclude`           | `[]`                                   | Inline array or path to a glob-list file.                                                                                |
| `[ktfmt.license-header].file`     | inherits `[license-header].file`       | Per-tool template override.                                                                                              |
| `[ktfmt.license-header].excludes` | none                                   | Path to a glob list (one per line, `#` comments).                                                                        |
| `[gjf].version`                   | -                                      | GitHub release version. Either a literal or a catalog reference `{ file, key }`. Mutually exclusive with `path`.         |
| `[gjf].path`                      | -                                      | Path to a checked-in jar or native binary. Mutually exclusive with `version`.                                            |
| `[gjf].style`                     | `"google"`                             | `google` / `aosp`                                                                                                        |
| `[gjf].native`                    | `"auto"`                               | `auto` / `always` / `never`. See "Native gjf".                                                                           |
| `[gjf.paths].include`             | `["**/*.java"]`                        | Inline array or path to a glob-list file.                                                                                |
| `[gjf.paths].exclude`             | `[]`                                   | Inline array or path to a glob-list file.                                                                                |
| `[gjf.license-header].file`       | inherits `[license-header].file`       | Per-tool template override.                                                                                              |
| `[gjf.license-header].excludes`   | none                                   | Path to a glob list.                                                                                                     |
| `[license-header].file`           | -                                      | Default license header template, `${YEAR}` expanded at write time. Section absence = no header insertion.                |
| `[paths].exclude`                 | `["**/build/**", "**/target/**"]`      | Universal exclude, applied before any tool's filter. Inline array or path to a glob-list file.                           |
| `[whitespace].strip-trailing`     | `true`                                 | Strip trailing space/tab/CR on every line.                                                                               |
| `[whitespace].final-newline`      | `true`                                 | Ensure files end with exactly one `\n`.                                                                                  |
| `[whitespace.paths].include`      | `["**/*.kt", "**/*.kts", "**/*.java"]` | Inline array or path to a glob-list file.                                                                                |
| `[whitespace.paths].exclude`      | `[]`                                   | Inline array or path to a glob-list file.                                                                                |
| `[hook].mode`                     | `"format"`                             | `format` formats and re-stages. `check` fails the commit if changes are needed.                                          |

Sections that are entirely optional: `[ktfmt]`, `[gjf]`, `[license-header]`,
`[ktfmt.license-header]`, `[gjf.license-header]`. Omitting a section
disables that step. `[paths]`, `[whitespace]`, and `[hook]` are always
present (with the defaults above).

## Subcommands

| Command              | Behavior                                                                                                                                 |
|----------------------|------------------------------------------------------------------------------------------------------------------------------------------|
| `kempt format`       | Format files in place.                                                                                                                   |
| `kempt check`        | Dry-run; exits non-zero if changes are needed. Suitable for CI.                                                                          |
| `kempt init`         | Scaffold `.kempt.toml` plus a starter `config/license-header.txt`. Detects `.kt`/`.java` to decide which sections to write.              |
| `kempt install-hook` | Write a `.git/hooks/pre-commit` that calls `kempt hook`.                                                                                 |
| `kempt hook`         | Run as the pre-commit hook. Not normally invoked manually.                                                                               |
| `kempt update`       | Download formatter jars/binaries per config. Pre-warms the cache.                                                                        |
| `kempt upgrade`      | Bump tool versions in `.kempt.toml` to the latest upstream release. Preserves comments and formatting. `--dry-run` previews.             |
| `kempt vendor`       | Download and copy formatter binaries into the repo for check-in (default dir `config/bin/`). Prints the `path = "..."` snippet to paste. |
| `kempt cache list`   | Show cached artifacts and their sizes.                                                                                                   |
| `kempt cache prune`  | Remove cached artifacts not referenced by `.kempt.toml`.                                                                                 |

### Flags

`kempt format` and `kempt check` share the same flag set:

| Flag                        | Default                        | Effect                                                                        |
|-----------------------------|--------------------------------|-------------------------------------------------------------------------------|
| `--all`                     | (the implicit default)         | All tracked files. Explicit alias of the default for unambiguous suggestions. |
| `--staged`                  | off                            | Only files in the git index.                                                  |
| `--discovery=<vcs\|walk>`   | `vcs`                          | `walk` walks the filesystem; doesn't consult `.gitignore`.                    |
| `--dry-run` (`format` only) | off                            | Preview without writing. Equivalent to `kempt check`.                         |
| `<file>...` (positional)    | -                              | Process exactly the listed files. Bypasses scope flags and `[paths]` filters. |
| `--config <PATH>`           | `.kempt.toml` in the repo root | Override the config file path.                                                |

`--all`, `--staged`, `--discovery=walk`, and explicit positional paths are
mutually exclusive.

`kempt install-hook` takes `--force` to overwrite an existing pre-commit
hook (default refuses).

`kempt vendor` takes `--dir <PATH>` (default `config/bin`) for the target
directory.

`kempt cache prune` takes `--all` to wipe every cached artifact regardless
of config.

### Failure messages

When `kempt check` finds something wrong it prints the offending file paths
followed by an actionable trailer that's tailored to scope and content:

- Default scope, files need formatting:
  `kempt: 3 files need formatting. Run `kempt format --all` to apply.`
  When the count is `<= 30`, a copy-pasteable per-file command is appended.
- `--staged`: trailer suggests `kempt format --staged`.
- `--discovery=walk`: trailer suggests `kempt format --discovery=walk`.
- Hook in `[hook] mode = "check"`: trailer reminds about
  `git commit --no-verify` as the bypass.
- Mixed format diffs + parse errors: trailer says "fix the syntax errors
  above, then run `kempt format` for the rest."
- Pure parse errors: trailer says formatting can't proceed until they're
  fixed.

ktfmt/gjf parse errors are surfaced with their file:line:col message; JVM
deprecation warnings are filtered out so the actual error is what you read.

## Pre-commit hook

`kempt install-hook` writes a one-line `pre-commit` that calls `kempt hook`.
The hook does, in order:

1. Collect staged files matching the config's path globs.
2. Refuse to run if any of those files have unstaged modifications. This is
   the "partial staging" case where formatting and re-staging would silently pull
   unstaged hunks into the commit. kempt doesn't pretend that this is a 
   desirable situation, instead refusing to proceed and asking you to pick one 
   of three escape hatches:
   1. Stage the rest
   2. `git stash --keep-index`
   3. Commit with `--no-verify`.
3. Run the full pipeline (license headers, whitespace, ktfmt, gjf).
4. `git add` the formatted files back to the index.

Set `[hook] mode = "check"` to make the hook fail on any required change
instead of formatting in place.

## CI

The cache lives at `~/.kempt/cache/` by default. Point it elsewhere with
`KEMPT_CACHE_DIR`. The cache contains versioned jars only, so the cache key
is just the contents of `.kempt.toml`.

### GitHub Actions

```yaml
name: format-check
on: [push, pull_request]

jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v6
      - uses: actions/setup-java@v5
        with:
          distribution: zulu
          java-version: '21'
      - uses: actions/cache@v5
        with:
          path: ~/.kempt/cache
          key: kempt-${{ hashFiles('.kempt.toml') }}
      - name: Install kempt
        run: |
          curl --proto '=https' --tlsv1.2 -LsSf \
            https://github.com/ZacSweers/kempt/releases/latest/download/kempt-fmt-installer.sh | sh
      - name: Pre-warm cache
        run: kempt update
      - name: Check formatting
        run: kempt check
```

What to cache:

- `~/.kempt/cache/` (or wherever `KEMPT_CACHE_DIR` points). Jars are 5 to 30
  MiB each.

What not to cache:

- The Rust toolchain. kempt is shipped as a static binary; no `cargo`
  required at run time.

`kempt update` is optional; the formatter will download on demand. Splitting
it out makes the failure mode (network down vs. real format error) easier to
read in CI logs.

## Versioning the formatter binaries

Pin `version = "..."` in each tool's section. The cache is version-suffixed,
so multiple repos with different pins coexist without re-downloading. To
update, change the version in `.kempt.toml`, run `kempt update`, then
`kempt cache prune` to drop the old jar.

### `kempt upgrade`

Bump the literal `version = "..."` entries in `.kempt.toml` to the latest
upstream release in one shot:

```sh
kempt upgrade            # apply
kempt upgrade --dry-run  # preview
```

Comments and formatting in `.kempt.toml` are preserved (the rewrite goes
through `toml_edit`). Sections that use `path = "..."` (vendored binaries)
or a catalog reference are skipped with a note explaining where to make
the change. After a successful upgrade, run `kempt update` to download the
new artifacts into the cache.

This is the offline-style equivalent of running Renovate locally.

### Catalog references (Gradle `libs.versions.toml`)

If you already track formatter versions in a Gradle version catalog,
`version` accepts a reference table instead of a literal:

```toml
[ktfmt]
version = { file = "gradle/libs.versions.toml", key = "ktfmt" }

[gjf]
version = { file = "gradle/libs.versions.toml" }   # key defaults to "gjf"
```

kempt resolves the reference by reading the catalog's `[versions]` table
at startup. The `key` field defaults to the tool name (`ktfmt` or `gjf`)
so the common case is even shorter:

```toml
[ktfmt]
version = { file = "gradle/libs.versions.toml" }
```

Resolution rules:
- Path is relative to the repo root (or absolute).
- Only literal version strings are accepted; structured "rich" Gradle
  versions (`{ strictly = "1.0", require = "..." }`) error with a clear
  message.
- The catalog file is parsed at most once per kempt invocation, even if
  both `[ktfmt]` and `[gjf]` reference the same file.

This works well with **Dependabot**, which doesn't natively understand
`.kempt.toml`: keep a dummy `[libraries]` entry in `libs.versions.toml`
that Dependabot can spot, and Renovate-style customization isn't needed.

### Auto-updating with Renovate

Renovate has no built-in knowledge of `.kempt.toml`, but its custom regex
manager covers it cleanly. Add this to your `renovate.json`:

```json
{
  "customManagers": [
    {
      "customType": "regex",
      "fileMatch": ["(^|/)\\.kempt\\.toml$"],
      "matchStrings": [
        "\\[ktfmt\\][^\\[]*?version\\s*=\\s*\"(?<currentValue>[^\"]+)\""
      ],
      "datasourceTemplate": "maven",
      "registryUrlTemplate": "https://repo1.maven.org/maven2",
      "depNameTemplate": "com.facebook:ktfmt"
    },
    {
      "customType": "regex",
      "fileMatch": ["(^|/)\\.kempt\\.toml$"],
      "matchStrings": [
        "\\[gjf\\][^\\[]*?version\\s*=\\s*\"(?<currentValue>[^\"]+)\""
      ],
      "datasourceTemplate": "maven",
      "registryUrlTemplate": "https://repo1.maven.org/maven2",
      "depNameTemplate": "com.google.googlejavaformat:google-java-format"
    }
  ]
}
```

Each manager scans `.kempt.toml`, finds the first `version = "..."` after a
matching tool section header (`[ktfmt]` or `[gjf]`), and tracks the
corresponding Maven Central coordinate. Renovate opens a PR per upstream
release.

`[^\[]*?` in the match string keeps the lookahead within the current
section, so a later section's `version` value isn't matched by the wrong
manager. Tool sections that use `path = "..."` instead of `version` are
skipped automatically (no `version` line to match).

### Dependabot

Dependabot does not support arbitrary regex-based managers, so there is no
direct equivalent for `.kempt.toml`. If Dependabot is a hard requirement,
the workaround is to keep the version pin in a file Dependabot already
understands (e.g. a Gradle `libs.versions.toml`) and copy it into
`.kempt.toml` manually or via a small CI step. Native catalog support in
kempt itself is on the table for a future release.

### Native gjf

Starting with gjf 1.20.0, Google publishes GraalVM-native binaries alongside
the JVM jar. They start in roughly 20ms instead of ~500ms (JVM warmup),
don't need a JDK, and skip the `--add-opens` dance entirely. Native builds
exist for `darwin-arm64`, `linux-x86-64`, `linux-arm64` (1.26.0+), and
`windows-x86-64`. There is no Intel macOS (`darwin-x86-64`) native build,
and no native ktfmt at all.

`[gjf].native` controls which artifact kempt downloads:

- `auto` (default): native when published for this platform + version, jar
  otherwise.
- `always`: native always; errors if not published for the host.
- `never`: jar always.

ktfmt is unaffected (always JVM).

### Checking in the binaries (hermetic / offline builds)

If you want full reproducibility or your CI can't reach Maven Central and
GitHub releases, commit the formatter binaries and point at them with
`path` instead of `version`. The fast path:

```sh
kempt vendor                    # downloads (if needed) and copies into config/bin/
kempt vendor --dir tools/bin    # custom directory
```

`vendor` skips any tool already using `path = ...` and prints the snippet
to paste into `.kempt.toml`:

```toml
[ktfmt]
path = "config/bin/ktfmt-0.62.jar"

[gjf]
path = "config/bin/gjf-1.35.0.jar"
```

`path` and `version` are mutually exclusive; exactly one must be set per
tool. Relative paths resolve against the repo root, absolute paths are used
as-is. When `path` is set, kempt never touches the cache for that tool:

- `kempt update` skips it.
- `kempt cache prune` ignores it.
- `kempt format` and the hook run the in-repo binary directly.

If the file is missing at the resolved path, kempt errors out with the full
path so you can spot the typo quickly.

## License

Apache 2.0. See [LICENSE](LICENSE.txt).
