// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
use std::process::Output;
use std::sync::{Arc, Mutex};

/// Captured output from an external tool or Git operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub tool: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Absent if the process was terminated by a signal.
    pub exit_code: Option<i32>,
    /// Whether the terminal renderer should forward stdout to the user.
    pub display_stdout: bool,
    /// Whether the terminal renderer should forward stderr without filtering.
    pub display_stderr: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<ProcessOutput>>>);

impl Capture {
    pub fn record(&self, tool: &str, output: &Output, display_stdout: bool, display_stderr: bool) {
        self.0
            .lock()
            .expect("output capture poisoned")
            .push(ProcessOutput {
                tool: tool.into(),
                stdout: output.stdout.clone(),
                stderr: output.stderr.clone(),
                exit_code: output.status.code(),
                display_stdout,
                display_stderr,
            });
    }

    pub fn take(&self) -> Vec<ProcessOutput> {
        std::mem::take(&mut *self.0.lock().expect("output capture poisoned"))
    }
}
