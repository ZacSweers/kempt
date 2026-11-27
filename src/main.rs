// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use kempt_core::{cli, commands};
use std::process::ExitCode;

fn main() -> ExitCode {
    let parsed = match cli::parse_from(std::env::args_os()) {
        Ok(parsed) => parsed,
        Err(error) => {
            let code = error.exit_code();
            let _ = error.print();
            return ExitCode::from(code as u8);
        }
    };
    let request = match parsed {
        cli::ParseOutcome::Command(request) => request,
        cli::ParseOutcome::Display { text, .. } => {
            print!("{text}");
            return ExitCode::SUCCESS;
        }
    };
    let context = match std::env::current_dir()
        .map_err(anyhow::Error::from)
        .and_then(|cwd| {
            // These commands do not use the cache and must work without HOME.
            if matches!(
                &request.command,
                commands::Command::Init(_)
                    | commands::Command::InstallHook(_)
                    | commands::Command::Upgrade(_)
            ) {
                commands::ExecutionContext::new(cwd.clone(), kempt_core::cache::Cache::new(cwd))
            } else {
                commands::ExecutionContext::from_env(cwd)
            }
        }) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("kempt: {error:#}");
            return ExitCode::FAILURE;
        }
    };
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    match commands::execute(&request, &context) {
        Ok(outcome) => match cli::render(&outcome, &mut stdout, &mut stderr) {
            Ok(()) => ExitCode::from(outcome.exit_code()),
            Err(error) => {
                eprintln!("kempt: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            let _ = cli::render_error(&error, &mut stdout, &mut stderr);
            ExitCode::FAILURE
        }
    }
}
