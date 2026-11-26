// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Smoke tests for the executable adapter and its kempt-core integration.
use kempt_core::cache::Cache;
use kempt_core::commands::*;
use std::path::Path;

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    git(temp.path(), &["init", "-q"]);
    temp
}

fn whitespace_repo() -> tempfile::TempDir {
    let temp = repo();
    std::fs::write(temp.path().join(".kempt.toml"), "").unwrap();
    std::fs::create_dir(temp.path().join("nested")).unwrap();
    std::fs::write(temp.path().join("nested/Foo.kt"), "class Foo   \n").unwrap();
    git(temp.path(), &["add", "nested/Foo.kt"]);
    temp
}

#[test]
fn executable_and_library_render_the_same_check_results() {
    let temp = whitespace_repo();
    let context = ExecutionContext::new(temp.path().into(), Cache::new(temp.path().join("tool-cache"))).unwrap();
    let outcome = execute(&CommandRequest::new(Command::Check(CheckOptions::default())), &context).unwrap();
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    kempt_core::cli::render(&outcome, &mut stdout, &mut stderr).unwrap();
    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_kempt"))
        .arg("check").current_dir(temp.path()).env("KEMPT_CACHE_DIR", context.cache.root()).output().unwrap();
    assert_eq!(cli.status.code(), Some(i32::from(outcome.exit_code())));
    assert_eq!(cli.stdout, stdout);
    assert_eq!(cli.stderr, stderr);
}

#[test]
fn commands_without_cache_work_without_home() {
    let temp = repo();
    for args in [vec!["init"], vec!["install-hook"], vec!["upgrade", "--dry-run"]] {
        if args[0] == "upgrade" {
            std::fs::write(temp.path().join(".kempt.toml"), "[ktfmt]\npath = \"formatter\"\n").unwrap();
        }
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_kempt"))
            .args(args).current_dir(temp.path()).env_remove("HOME").env_remove("KEMPT_CACHE_DIR").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
}
