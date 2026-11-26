// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! Trailing whitespace + final newline normalization. Pure string transforms.
//!
//! Two passes are independent: stripping trailing whitespace from each line,
//! and ensuring the file ends with a single newline. The caller passes
//! [`Options`] selecting which pass to run.

use crate::config::Whitespace;

/// Per-pass selection. Each field defaults to disabled; cheap to construct
/// from [`Whitespace`] config.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    pub strip_trailing: bool,
    pub final_newline: bool,
}

impl From<&Whitespace> for Options {
    fn from(c: &Whitespace) -> Self {
        Self {
            strip_trailing: c.strip_trailing,
            final_newline: c.final_newline,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Issues {
    pub trailing_whitespace: bool,
    pub missing_final_newline: bool,
}

impl Issues {
    pub fn any(self) -> bool {
        self.trailing_whitespace || self.missing_final_newline
    }
}

/// Detect issues that the configured passes would fix. Disabled passes are
/// reported as `false` regardless of file content.
pub fn diagnose(content: &str, opts: Options) -> Issues {
    Issues {
        trailing_whitespace: opts.strip_trailing && has_trailing_whitespace(content),
        missing_final_newline: opts.final_newline
            && !content.is_empty()
            && !content.ends_with('\n'),
    }
}

fn has_trailing_whitespace(content: &str) -> bool {
    content.split('\n').any(|line| {
        let trimmed = line.trim_end_matches([' ', '\t', '\r']);
        trimmed.len() != line.len()
    })
}

/// Apply the enabled passes. Empty input is left empty regardless of
/// options.
pub fn fix(content: &str, opts: Options) -> String {
    if content.is_empty() {
        return String::new();
    }
    if !opts.strip_trailing && !opts.final_newline {
        return content.to_string();
    }
    if !opts.strip_trailing {
        // Only ensure a trailing newline; leave line content alone.
        return ensure_final_newline(content);
    }
    // Strip trailing whitespace per line. If `final_newline` is enabled, end
    // the result with one `\n`; otherwise preserve whether the original had
    // a trailing newline.
    let had_final_newline = content.ends_with('\n');
    let mut out = String::with_capacity(content.len());
    for (i, line) in content.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches([' ', '\t', '\r']));
    }
    if opts.final_newline || had_final_newline {
        out.push('\n');
    }
    out
}

fn ensure_final_newline(content: &str) -> String {
    if content.ends_with('\n') {
        content.to_string()
    } else {
        let mut out = String::with_capacity(content.len() + 1);
        out.push_str(content);
        out.push('\n');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> Options {
        Options {
            strip_trailing: true,
            final_newline: true,
        }
    }

    fn only_strip() -> Options {
        Options {
            strip_trailing: true,
            final_newline: false,
        }
    }

    fn only_newline() -> Options {
        Options {
            strip_trailing: false,
            final_newline: true,
        }
    }

    // --- diagnose ---

    #[test]
    fn diagnose_clean_file() {
        assert!(!diagnose("hello\nworld\n", both()).any());
    }

    #[test]
    fn diagnose_trailing_spaces() {
        let i = diagnose("hello   \nworld\n", both());
        assert!(i.trailing_whitespace);
        assert!(!i.missing_final_newline);
    }

    #[test]
    fn diagnose_trailing_tabs() {
        assert!(diagnose("x\t\n", both()).trailing_whitespace);
    }

    #[test]
    fn diagnose_trailing_cr() {
        assert!(diagnose("x\r\ny\n", both()).trailing_whitespace);
    }

    #[test]
    fn diagnose_missing_final_newline() {
        let i = diagnose("hello", both());
        assert!(i.missing_final_newline);
        assert!(!i.trailing_whitespace);
    }

    #[test]
    fn diagnose_empty_is_clean() {
        assert!(!diagnose("", both()).any());
    }

    #[test]
    fn diagnose_disabled_pass_does_not_flag_issues() {
        // Trailing whitespace exists, but the strip-trailing pass is off.
        assert!(!diagnose("foo   \n", only_newline()).trailing_whitespace);
        // Missing newline, but the final-newline pass is off.
        assert!(!diagnose("foo", only_strip()).missing_final_newline);
    }

    // --- fix ---

    #[test]
    fn fix_strips_trailing_spaces() {
        assert_eq!(fix("hello   \nworld\n", both()), "hello\nworld\n");
    }

    #[test]
    fn fix_strips_mixed_whitespace() {
        assert_eq!(fix("a \t\nb \r\n", both()), "a\nb\n");
    }

    #[test]
    fn fix_adds_missing_final_newline() {
        assert_eq!(fix("hello", both()), "hello\n");
    }

    #[test]
    fn fix_idempotent_on_clean_input() {
        let clean = "package foo\n\nclass Bar\n";
        assert_eq!(fix(clean, both()), clean);
    }

    #[test]
    fn fix_empty_stays_empty() {
        assert_eq!(fix("", both()), "");
    }

    #[test]
    fn fix_does_not_collapse_blank_lines() {
        assert_eq!(fix("a\n\nb\n", both()), "a\n\nb\n");
    }

    #[test]
    fn fix_preserves_indentation() {
        assert_eq!(fix("  hello   \n", both()), "  hello\n");
    }

    #[test]
    fn fix_strips_whitespace_only_line_to_empty() {
        assert_eq!(fix("a\n   \nb\n", both()), "a\n\nb\n");
    }

    #[test]
    fn fix_only_strip_preserves_no_final_newline() {
        // strip-trailing on, final-newline off → leave the no-newline state.
        assert_eq!(fix("hello   ", only_strip()), "hello");
    }

    #[test]
    fn fix_only_strip_preserves_existing_final_newline() {
        assert_eq!(fix("hello   \n", only_strip()), "hello\n");
    }

    #[test]
    fn fix_only_newline_does_not_strip_trailing() {
        assert_eq!(fix("hello   ", only_newline()), "hello   \n");
    }

    #[test]
    fn fix_both_disabled_is_identity() {
        let opts = Options::default();
        let s = "noisy   \nstuff";
        assert_eq!(fix(s, opts), s);
    }
}
