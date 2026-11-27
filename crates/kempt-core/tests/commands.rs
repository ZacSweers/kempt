// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
#![cfg(feature = "commands")]

use anyhow::{bail, Result};
use kempt_core::cache::{ArtifactFormat, ArtifactRequest, Cache, Downloader};
use kempt_core::commands::*;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Mutex;

struct FakeDownloader(Mutex<Vec<String>>);
impl Downloader for FakeDownloader {
    fn download(&self, url: &str, dest: &Path) -> Result<()> {
        self.0.lock().unwrap().push(url.into());
        std::fs::write(dest, b"downloaded jar")?;
        Ok(())
    }
}
struct FakeVersions;
impl VersionFetcher for FakeVersions {
    fn latest_ktfmt(&self) -> Result<String> {
        Ok("0.66".into())
    }
    fn latest_gjf(&self) -> Result<String> {
        Ok("1.29.0".into())
    }
    fn latest_gradle_dependencies_sorter(&self) -> Result<String> {
        Ok("0.15".into())
    }
}

fn context<'a>(root: &Path, downloader: &'a dyn Downloader) -> ExecutionContext<'a> {
    ExecutionContext {
        working_directory: root.into(),
        cache: Cache::new(root.join("tool-cache")),
        downloader,
        version_fetcher: &FakeVersions,
        year: 2026,
        hook_options: HookOptions::default(),
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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

fn report(outcome: &CommandOutcome) -> &FormatOutcome {
    match &outcome.result {
        CommandResult::Format { report, .. } | CommandResult::Hook { report } => report,
        other => panic!("unexpected result: {other:?}"),
    }
}

#[test]
fn typed_checks_and_dry_runs_report_changes_without_writing() {
    let temp = whitespace_repo();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    for command in [
        Command::Check(CheckOptions::default()),
        Command::Format(FormatOptions {
            dry_run: true,
            ..Default::default()
        }),
    ] {
        let outcome = execute(&CommandRequest::new(command), &context).unwrap();
        assert_eq!(outcome.exit_code(), 1);
        assert_eq!(
            report(&outcome).changed.iter().next().unwrap(),
            Path::new("nested/Foo.kt")
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("nested/Foo.kt")).unwrap(),
            "class Foo   \n"
        );
    }
    let outcome = execute(
        &CommandRequest::new(Command::Format(FormatOptions::default())),
        &context,
    )
    .unwrap();
    assert_eq!(outcome.exit_code(), 0);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("nested/Foo.kt")).unwrap(),
        "class Foo\n"
    );
    let clean = execute(
        &CommandRequest::new(Command::Check(CheckOptions::default())),
        &context,
    )
    .unwrap();
    assert!(report(&clean).changed.is_empty());
    assert_eq!(clean.exit_code(), 0);
    assert!(downloader.0.lock().unwrap().is_empty());
}

#[test]
fn nested_calls_and_separate_repositories_do_not_change_process_state() {
    let first = whitespace_repo();
    let second = whitespace_repo();
    let cwd_before = std::env::current_dir().unwrap();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let mut first_context = context(first.path(), &downloader);
    first_context.working_directory = first.path().join("nested");
    std::fs::write(first.path().join("nested/custom.toml"), "").unwrap();
    let nested = CommandRequest {
        config: Some("custom.toml".into()),
        command: Command::Check(CheckOptions {
            scope: FileScope::Explicit {
                targets: vec!["Foo.kt".into()],
                force: false,
            },
        }),
    };
    let outcome = execute(&nested, &first_context).unwrap();
    assert_eq!(report(&outcome).changed.len(), 1);
    let second_context = context(second.path(), &downloader);
    let outcome = execute(
        &CommandRequest::new(Command::Format(FormatOptions::default())),
        &second_context,
    )
    .unwrap();
    assert_eq!(report(&outcome).changed.len(), 1);
    assert_eq!(std::env::current_dir().unwrap(), cwd_before);
    assert_eq!(
        std::fs::read_to_string(first.path().join("nested/Foo.kt")).unwrap(),
        "class Foo   \n"
    );
    assert_eq!(
        std::fs::read_to_string(second.path().join("nested/Foo.kt")).unwrap(),
        "class Foo\n"
    );
}

#[test]
fn invalid_explicit_scope_and_configuration_return_errors() {
    let temp = whitespace_repo();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    let request = CommandRequest::new(Command::Check(CheckOptions {
        scope: FileScope::Explicit {
            targets: vec![],
            force: true,
        },
    }));
    assert!(execute(&request, &context)
        .unwrap_err()
        .to_string()
        .contains("at least one target"));
    std::fs::write(
        temp.path().join(".kempt.toml"),
        "[ktfmt]\nversion = \"0.65\"\npath = \"tool\"\n",
    )
    .unwrap();
    assert!(execute(
        &CommandRequest::new(Command::Check(CheckOptions::default())),
        &context
    )
    .is_err());
    assert!(downloader.0.lock().unwrap().is_empty());
}

#[test]
fn catalog_versions_are_resolved_before_downloading() {
    let temp = repo();
    std::fs::write(
        temp.path().join("versions.toml"),
        "[versions]\nformatter = \"0.65\"\n",
    )
    .unwrap();
    std::fs::write(temp.path().join(".kempt.toml"), "[ktfmt]\nversion = { file = \"versions.toml\", key = \"formatter\" }\nnative = \"never\"\n").unwrap();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    execute(&CommandRequest::new(Command::Update), &context).unwrap();
    assert!(context.cache.root().join("ktfmt-0.65.jar").exists());
    assert!(downloader.0.lock().unwrap()[0].contains("v0.65/"));
}

#[test]
fn cache_commands_preserve_custom_namespaces() {
    let temp = tempfile::tempdir().unwrap();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    let namespace = context.cache.namespace("other-tool").unwrap();
    let custom = namespace
        .ensure(
            &ArtifactRequest {
                key: "tool-1".into(),
                url: "custom".into(),
                format: ArtifactFormat::File,
            },
            &downloader,
        )
        .unwrap();
    std::fs::write(context.cache.root().join("ktfmt-0.65.jar"), b"jar").unwrap();
    let list = execute(&CommandRequest::new(Command::CacheList), &context).unwrap();
    match list.result {
        CommandResult::CacheList { entries, .. } => assert_eq!(entries.len(), 1),
        other => panic!("{other:?}"),
    }
    let prune = execute(
        &CommandRequest::new(Command::CachePrune(PruneOptions { all: true })),
        &context,
    )
    .unwrap();
    match prune.result {
        CommandResult::CachePrune { removed } => assert_eq!(removed.len(), 1),
        other => panic!("{other:?}"),
    }
    assert!(custom.exists());
}

#[test]
fn init_install_update_vendor_and_upgrade_return_typed_results() {
    let temp = repo();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    let created = execute(
        &CommandRequest::new(Command::Init(InitOptions::default())),
        &context,
    )
    .unwrap();
    assert!(matches!(created.result, CommandResult::Init { created } if !created.is_empty()));
    let installed = execute(
        &CommandRequest::new(Command::InstallHook(InstallHookOptions::default())),
        &context,
    )
    .unwrap();
    assert!(matches!(installed.result, CommandResult::InstallHook { path } if path.exists()));
    let config = "[ktfmt]\nversion = \"0.65\"\nnative = \"never\"\n";
    std::fs::write(temp.path().join(".kempt.toml"), config).unwrap();
    let updated = execute(&CommandRequest::new(Command::Update), &context).unwrap();
    assert!(
        matches!(updated.result, CommandResult::Update { cache_root } if cache_root.join("ktfmt-0.65.jar").exists())
    );
    let vendored = execute(
        &CommandRequest::new(Command::Vendor(VendorOptions::default())),
        &context,
    )
    .unwrap();
    assert!(
        matches!(vendored.result, CommandResult::Vendor { report } if report.entries[0].dest.exists())
    );
    let upgraded = execute(
        &CommandRequest::new(Command::Upgrade(UpgradeOptions { dry_run: true })),
        &context,
    )
    .unwrap();
    assert!(
        matches!(upgraded.result, CommandResult::Upgrade { report, dry_run: true } if report.changes[0].to == "0.66")
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join(".kempt.toml")).unwrap(),
        config
    );
    execute(
        &CommandRequest::new(Command::Upgrade(UpgradeOptions::default())),
        &context,
    )
    .unwrap();
    assert!(std::fs::read_to_string(temp.path().join(".kempt.toml"))
        .unwrap()
        .contains("0.66"));
    let pruned = execute(
        &CommandRequest::new(Command::CachePrune(PruneOptions::default())),
        &context,
    )
    .unwrap();
    assert!(matches!(pruned.result, CommandResult::CachePrune { removed } if removed.len() == 1));
}

#[test]
fn hooks_return_check_results_and_restaging_results() {
    let temp = whitespace_repo();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    std::fs::write(
        temp.path().join(".kempt.toml"),
        "[hook]\nmode = \"check\"\n",
    )
    .unwrap();
    let check = execute(&CommandRequest::new(Command::Hook), &context).unwrap();
    assert_eq!(check.exit_code(), 1);
    std::fs::write(temp.path().join(".kempt.toml"), "").unwrap();
    let formatted = execute(&CommandRequest::new(Command::Hook), &context).unwrap();
    assert_eq!(formatted.exit_code(), 0);
    let staged = std::process::Command::new("git")
        .args(["show", ":nested/Foo.kt"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert_eq!(staged.stdout, b"class Foo\n");
}

#[test]
fn hook_partial_staging_diagnostics_are_returned() {
    let temp = whitespace_repo();
    std::fs::write(
        temp.path().join("nested/Foo.kt"),
        "class Foo   \n// unstaged\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join(".kempt.toml"),
        "[ktfmt]\npath = \"formatter\"\n",
    )
    .unwrap();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let error = execute(
        &CommandRequest::new(Command::Hook),
        &context(temp.path(), &downloader),
    )
    .unwrap_err();
    assert_eq!(error.diagnostics.len(), 1);
    assert!(error.diagnostics[0].contains("Foo.kt"));
    assert!(error.to_string().contains("partial staging detected"));
}

#[cfg(unix)]
fn fake_formatter(root: &Path, fail: bool) {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("formatter");
    let body = if fail {
        "printf 'TOOL_STDOUT\\n'\nprintf 'TOOL_STDERR\\n' >&2\nexit 7\n"
    } else {
        "printf 'TOOL_STDOUT\\n'\nprintf 'WARNING: JVM noise\\n' >&2\npwd >&2\n"
    };
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join(".kempt.toml"), "[ktfmt]\npath = \"formatter\"\n").unwrap();
}

#[cfg(unix)]
#[test]
fn formatter_failures_retain_output_and_underlying_error() {
    let temp = whitespace_repo();
    fake_formatter(temp.path(), true);
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let error = execute(
        &CommandRequest::new(Command::Format(FormatOptions::default())),
        &context(temp.path(), &downloader),
    )
    .unwrap_err();
    assert!(error.to_string().contains("exit 7"));
    assert_eq!(error.output[0].stdout, b"TOOL_STDOUT\n");
    assert_eq!(error.output[0].stderr, b"TOOL_STDERR\n");
    assert!(std::error::Error::source(&error).is_some());
}

#[cfg(unix)]
#[test]
fn quiet_library_child() {
    let Some(root) = std::env::var_os("KEMPT_TEST_QUIET_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let outcome = execute(
        &CommandRequest::new(Command::Format(FormatOptions::default())),
        &context(&root, &downloader),
    )
    .unwrap();
    assert_eq!(outcome.output[0].stdout, b"TOOL_STDOUT\n");
    assert!(String::from_utf8_lossy(&outcome.output[0].stderr).contains(root.to_str().unwrap()));
}

#[cfg(unix)]
#[test]
fn library_execution_does_not_print_subprocess_output() {
    let temp = whitespace_repo();
    fake_formatter(temp.path(), false);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "quiet_library_child", "--nocapture"])
        .env("KEMPT_TEST_QUIET_ROOT", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("TOOL_STDOUT"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("JVM noise"));
}

#[cfg(feature = "cli")]
#[test]
fn cli_arguments_and_typed_requests_execute_the_same_commands() {
    let temp = whitespace_repo();
    let downloader = FakeDownloader(Mutex::new(vec![]));
    let context = context(temp.path(), &downloader);
    for (args, command) in [
        (
            vec!["kempt", "check", "--staged"],
            Command::Check(CheckOptions {
                scope: FileScope::Staged,
            }),
        ),
        (
            vec!["kempt", "format", "--dry-run", "nested/Foo.kt"],
            Command::Format(FormatOptions {
                dry_run: true,
                scope: FileScope::Explicit {
                    targets: vec!["nested/Foo.kt".into()],
                    force: false,
                },
            }),
        ),
    ] {
        let kempt_core::cli::ParseOutcome::Command(parsed) =
            kempt_core::cli::parse_from(args).unwrap()
        else {
            panic!("expected command");
        };
        let from_cli = execute(&parsed, &context).unwrap();
        let typed = execute(&CommandRequest::new(command), &context).unwrap();
        assert_eq!(report(&from_cli).changed, report(&typed).changed);
        assert_eq!(from_cli.exit_code(), typed.exit_code());
        let (mut cli_stdout, mut cli_stderr) = (Vec::new(), Vec::new());
        let (mut typed_stdout, mut typed_stderr) = (Vec::new(), Vec::new());
        kempt_core::cli::render(&from_cli, &mut cli_stdout, &mut cli_stderr).unwrap();
        kempt_core::cli::render(&typed, &mut typed_stdout, &mut typed_stderr).unwrap();
        assert_eq!(cli_stdout, typed_stdout);
        assert_eq!(cli_stderr, typed_stderr);
    }
}

#[cfg(feature = "cli")]
#[test]
fn parsing_help_version_and_invalid_arguments_does_not_exit() {
    use kempt_core::cli::{parse_from, ParseOutcome};
    assert!(matches!(
        parse_from(["kempt", "--help"]).unwrap(),
        ParseOutcome::Display { version: false, .. }
    ));
    assert!(matches!(
        parse_from(["kempt", "--version"]).unwrap(),
        ParseOutcome::Display { version: true, .. }
    ));
    assert_eq!(
        parse_from(["kempt", "--unknown"]).unwrap_err().exit_code(),
        2
    );
    assert_eq!(
        parse_from(["kempt", "check", "--all", "--staged"])
            .unwrap_err()
            .exit_code(),
        1
    );
    assert!(parse_from(["kempt", "check", "--force"]).is_err());
}

#[cfg(feature = "cli")]
#[test]
fn renderer_preserves_check_text_and_forwards_formatter_output() {
    let outcome = CommandOutcome {
        result: CommandResult::Format {
            report: FormatOutcome::default(),
            context: CheckContext::All,
            check: false,
        },
        output: vec![ProcessOutput {
            tool: "fake".into(),
            stdout: b"formatted\n".to_vec(),
            stderr: b"WARNING: noise".to_vec(),
            exit_code: Some(0),
            display_stdout: true,
            display_stderr: false,
        }],
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    kempt_core::cli::render(&outcome, &mut stdout, &mut stderr).unwrap();
    assert_eq!(stdout, b"formatted\nkempt: nothing to do\n");
    assert!(stderr.is_empty());
}

#[test]
fn failed_downloads_return_errors_without_printing() {
    struct Failed;
    impl Downloader for Failed {
        fn download(&self, _url: &str, dest: &Path) -> Result<()> {
            std::fs::write(dest, b"partial")?;
            bail!("network failed")
        }
    }
    let temp = repo();
    std::fs::write(
        temp.path().join(".kempt.toml"),
        "[ktfmt]\nversion = \"0.65\"\nnative = \"never\"\n",
    )
    .unwrap();
    let error = execute(
        &CommandRequest::new(Command::Update),
        &context(temp.path(), &Failed),
    )
    .unwrap_err();
    assert!(error.to_string().contains("network failed"));
    assert!(!temp.path().join("tool-cache/ktfmt-0.65.jar").exists());
}

#[cfg(all(unix, feature = "cli"))]
#[test]
fn cli_parsing_preserves_non_utf8_paths() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let target = OsString::from_vec(b"file-\xff.kt".to_vec());
    let args = [
        OsString::from("kempt"),
        OsString::from("check"),
        target.clone(),
    ];
    let kempt_core::cli::ParseOutcome::Command(request) =
        kempt_core::cli::parse_from(args).unwrap()
    else {
        panic!("expected command");
    };
    match request.command {
        Command::Check(CheckOptions {
            scope: FileScope::Explicit { targets, .. },
        }) => assert_eq!(targets[0].as_os_str(), target),
        other => panic!("unexpected command: {other:?}"),
    }
}
