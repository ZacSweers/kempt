// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use kempt_core::{cli, commands};

fn main() -> anyhow::Result<()> {
    let parsed = cli::parse_from(["kempt", "check", "--staged"])?;
    if let cli::ParseOutcome::Command(request) = parsed {
        let context = commands::ExecutionContext::from_env(std::env::current_dir()?)?;
        let outcome = commands::execute(&request, &context)?;
        cli::render(&outcome, &mut std::io::stdout(), &mut std::io::stderr())?;
        std::process::exit(i32::from(outcome.exit_code()));
    }
    Ok(())
}
