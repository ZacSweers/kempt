# Changelog

## [Unreleased]

- Add a reusable GitHub Action for installing Kempt and checking formatting.
- Preserve Kotlin script shebangs when inserting license headers.
- Add experimental partial-staging support for ktfmt-managed Kotlin files behind
  `KEMPT_EXPERIMENTAL_PARTIAL_KTFMT`. Note this requires ktfmt `0.65`+.

## [0.1.2]

_2026-06-22_

- Make `kempt init` license-header scaffolding opt-in with `--license-header`.

## [0.1.1]

_2026-06-21_

- Re-stage hook-formatted files with `git add --force` so tracked files inside
  ignored directories do not fail the commit hook.

## [0.1.0]

_2026-06-12_

- Fix "Nothing to do" always being printed.
- Add experimental partial-staging support for GJF-managed Java files behind `KEMPT_EXPERIMENTAL_PARTIAL_GJF`.
- Add Rust formatting and license-header support through `cargo fmt`.

## [0.0.4]

_2026-05-04_

- Always run `git add` on previously staged files after running formatting.

## [0.0.3]

_2026-05-03_

## [0.0.2]

_2026-05-03_

## [0.0.1]

_2026-05-03_

Initial release
