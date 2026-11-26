// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use kempt_core::commands::{execute, CheckOptions, Command, CommandRequest, CommandResult, ExecutionContext};

fn main() -> anyhow::Result<()> {
    let context = ExecutionContext::from_env(std::env::current_dir()?)?;
    let outcome = execute(&CommandRequest::new(Command::Check(CheckOptions::default())), &context)?;
    if let CommandResult::Format { report, .. } = &outcome.result {
        for path in &report.changed { println!("{}", path.display()); }
    }
    std::process::exit(i32::from(outcome.exit_code()));
}
