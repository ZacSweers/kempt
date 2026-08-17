// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Platform-aware command-line argument batching.

use std::ffi::OsString;
use std::path::PathBuf;

const UNIX_ARGUMENT_BUDGET: usize = 100 * 1024;
const WINDOWS_ARGUMENT_BUDGET: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Platform {
    Unix,
    Windows,
}

const fn current_platform() -> Platform {
    if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Unix
    }
}

const fn argument_budget_for(platform: Platform) -> usize {
    match platform {
        Platform::Unix => UNIX_ARGUMENT_BUDGET,
        Platform::Windows => WINDOWS_ARGUMENT_BUDGET,
    }
}

/// Split `paths` into contiguous chunks that fit within the current platform's
/// conservative command-line budget after accounting for `fixed_args`.
///
/// Windows limits the complete command line to 32,767 UTF-16 code units. Kempt
/// uses half that limit measured in encoded bytes so executable paths, quoting,
/// and other fixed invocation details have ample headroom. Unix keeps the
/// existing 100 KiB budget, well below the usual `ARG_MAX` values.
pub(crate) fn path_chunks<'a>(fixed_args: &[OsString], paths: &'a [PathBuf]) -> Vec<&'a [PathBuf]> {
    path_chunks_with_budget(fixed_args, paths, argument_budget_for(current_platform()))
}

fn path_chunks_with_budget<'a>(
    fixed_args: &[OsString],
    paths: &'a [PathBuf],
    total_budget: usize,
) -> Vec<&'a [PathBuf]> {
    let fixed_size = fixed_args
        .iter()
        .map(|arg| arg.as_os_str().len() + 1)
        .sum::<usize>();
    let path_budget = total_budget.saturating_sub(fixed_size).max(1);
    let mut chunks = Vec::new();
    let mut start = 0;
    let mut size = 0usize;

    for (index, path) in paths.iter().enumerate() {
        let path_size = path.as_os_str().len() + 1;
        if size.saturating_add(path_size) > path_budget && index > start {
            chunks.push(&paths[start..index]);
            start = index;
            size = 0;
        }
        size = size.saturating_add(path_size);
    }

    if start < paths.len() {
        chunks.push(&paths[start..]);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn platform_budgets_keep_unix_capacity_and_leave_windows_headroom() {
        assert_eq!(argument_budget_for(Platform::Unix), 100 * 1024);
        assert_eq!(argument_budget_for(Platform::Windows), 16 * 1024);
    }

    #[test]
    fn current_platform_selects_the_build_target_budget() {
        let expected = if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Unix
        };
        assert_eq!(current_platform(), expected);
    }

    #[test]
    fn empty_paths_produce_no_chunks() {
        assert!(path_chunks_with_budget(&[], &[], 100).is_empty());
    }

    #[test]
    fn paths_fit_in_one_chunk_under_budget() {
        let paths = paths(&["a.kt", "b.kt", "c.kt"]);
        let chunks = path_chunks_with_budget(&[], &paths, 100);
        assert_eq!(chunks, vec![paths.as_slice()]);
    }

    #[test]
    fn fixed_arguments_reduce_the_path_budget() {
        let paths = paths(&["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc"]);
        let fixed = vec![OsString::from("123456789")];

        let chunks = path_chunks_with_budget(&fixed, &paths, 32);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], &paths[0..2]);
        assert_eq!(chunks[1], &paths[2..3]);
    }

    #[test]
    fn single_path_larger_than_budget_gets_its_own_chunk() {
        let paths = paths(&["this/is/a/very/long/path/that/exceeds/budget.kt"]);
        let chunks = path_chunks_with_budget(&[], &paths, 5);
        assert_eq!(chunks, vec![paths.as_slice()]);
    }

    #[test]
    fn path_chunks_preserve_order() {
        let paths = paths(&["a", "b", "c", "d", "e"]);
        let chunks = path_chunks_with_budget(&[], &paths, 4);
        let flattened = chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .collect::<Vec<_>>();
        assert_eq!(flattened, paths.iter().collect::<Vec<_>>());
    }
}
