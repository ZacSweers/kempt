// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! License header insertion.
//!
//! Replaces ${YEAR} in the template, detects the language-specific insertion
//! point (first `package`/`@file:`/`use`/etc. line), and prepends the
//! rendered header. Anything in the file before the insertion point is
//! discarded, matching the behavior of the original format.sh.

use std::path::Path;

const HEADER_SCAN_LINES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Kotlin,
    Kts,
    Java,
    Rust,
}

impl SourceKind {
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        match ext {
            "kt" => Some(Self::Kotlin),
            "kts" => Some(Self::Kts),
            "java" => Some(Self::Java),
            "rs" => Some(Self::Rust),
            _ => None,
        }
    }

    /// Prefixes that mark the start of "real" file content (where the header
    /// should be inserted just above).
    fn delimiter_prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Kotlin => &["package ", "@file:"],
            Self::Kts => &[
                "@file:",
                "import ",
                "plugins ",
                "buildscript ",
                "dependencies ",
                "pluginManagement",
                "dependencyResolutionManagement",
            ],
            Self::Java => &["package "],
            Self::Rust => &[
                "#![",
                "//!",
                "use ",
                "mod ",
                "pub ",
                "fn ",
                "impl ",
                "struct ",
                "enum ",
                "trait ",
                "const ",
                "static ",
                "type ",
                "macro_rules!",
            ],
        }
    }
}

/// Substitute `${YEAR}` in the template.
pub fn render_header(template: &str, year: u32) -> String {
    let rendered = template.replace("${YEAR}", &year.to_string());
    if rendered.ends_with('\n') {
        rendered
    } else {
        format!("{rendered}\n")
    }
}

/// A stable substring derived from the template that lets us detect whether a
/// file already has *some* version of this header (any year). Picks the longest
/// non-empty line that doesn't contain `${YEAR}`. If every line contains
/// `${YEAR}`, falls back to the longest line with the placeholder stripped.
pub fn marker_for_template(template: &str) -> String {
    let best_stable = template
        .lines()
        .filter(|l| !l.contains("${YEAR}"))
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .max_by_key(|l| l.len());
    if let Some(line) = best_stable {
        return line.to_string();
    }
    // Fallback: split each line on `${YEAR}` and use the longest non-empty
    // chunk. This is more robust than stripping the placeholder, since it
    // produces a substring that actually appears in year-substituted files.
    template
        .lines()
        .flat_map(|l| l.split("${YEAR}"))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .max_by_key(|s| s.len())
        .unwrap_or("")
        .to_string()
}

/// True if the first few lines of `content` contain `marker`.
pub fn has_header(content: &str, marker: &str) -> bool {
    if marker.is_empty() {
        return false;
    }
    content
        .lines()
        .take(HEADER_SCAN_LINES)
        .any(|line| line.contains(marker))
}

/// Insert (or replace pre-package preamble with) `header` in `content`.
/// Returns the new content. `header` is expected to end with a newline.
pub fn insert_header(content: &str, header: &str, kind: SourceKind) -> String {
    let prefixes = kind.delimiter_prefixes();
    let mut byte_idx = None;
    let mut cursor = 0usize;
    for line in content.split_inclusive('\n') {
        let trimmed_start = line.trim_start();
        if prefixes.iter().any(|p| trimmed_start.starts_with(p)) {
            byte_idx = Some(cursor);
            break;
        }
        cursor += line.len();
    }
    let mut out = String::with_capacity(header.len() + content.len());
    out.push_str(header);
    match byte_idx {
        Some(idx) => out.push_str(&content[idx..]),
        None => out.push_str(content),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn template() -> String {
        "// Copyright (C) ${YEAR} Zac Sweers\n// SPDX-License-Identifier: Apache-2.0\n".into()
    }

    #[test]
    fn source_kind_from_extension() {
        assert_eq!(
            SourceKind::from_path(&PathBuf::from("a/b/Foo.kt")),
            Some(SourceKind::Kotlin)
        );
        assert_eq!(
            SourceKind::from_path(&PathBuf::from("build.gradle.kts")),
            Some(SourceKind::Kts)
        );
        assert_eq!(
            SourceKind::from_path(&PathBuf::from("Foo.java")),
            Some(SourceKind::Java)
        );
        assert_eq!(
            SourceKind::from_path(&PathBuf::from("src/lib.rs")),
            Some(SourceKind::Rust)
        );
        assert_eq!(SourceKind::from_path(&PathBuf::from("README.md")), None);
    }

    #[test]
    fn render_substitutes_year_and_ensures_trailing_newline() {
        let h = render_header(&template(), 2026);
        assert!(h.contains("2026"));
        assert!(!h.contains("${YEAR}"));
        assert!(h.ends_with('\n'));
    }

    #[test]
    fn render_preserves_existing_trailing_newline() {
        let h = render_header(&template(), 2026);
        assert_eq!(h.matches('\n').count(), 2);
    }

    #[test]
    fn render_adds_newline_if_template_lacks_one() {
        let h = render_header("// © ${YEAR}", 2026);
        assert_eq!(h, "// © 2026\n");
    }

    #[test]
    fn marker_uses_longest_non_year_line() {
        let m = marker_for_template(&template());
        assert_eq!(m, "// SPDX-License-Identifier: Apache-2.0");
    }

    #[test]
    fn marker_falls_back_when_every_line_has_year() {
        let m = marker_for_template("// (c) ${YEAR}\n// year ${YEAR} all rights\n");
        assert!(m.contains("all rights"), "got: {m}");
    }

    #[test]
    fn marker_empty_template_returns_empty() {
        assert_eq!(marker_for_template(""), "");
    }

    #[test]
    fn has_header_true_when_marker_in_first_lines() {
        let content = "// Copyright (C) 2024 Zac Sweers\n\
                       // SPDX-License-Identifier: Apache-2.0\n\
                       package foo\n";
        assert!(has_header(content, "SPDX-License-Identifier: Apache-2.0"));
    }

    #[test]
    fn has_header_false_when_marker_absent() {
        assert!(!has_header(
            "package foo\n",
            "SPDX-License-Identifier: Apache-2.0"
        ));
    }

    #[test]
    fn has_header_only_scans_first_n_lines() {
        let mut content = String::new();
        for _ in 0..(HEADER_SCAN_LINES + 5) {
            content.push_str("// noise\n");
        }
        content.push_str("// SPDX-License-Identifier: Apache-2.0\n");
        assert!(!has_header(&content, "SPDX-License-Identifier: Apache-2.0"));
    }

    #[test]
    fn has_header_empty_marker_is_false() {
        assert!(!has_header("anything", ""));
    }

    #[test]
    fn insert_header_kotlin_with_package() {
        let content = "package com.example\n\nclass Foo\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Kotlin);
        assert!(out.starts_with("// Copyright (C) 2026"));
        assert!(out.contains("package com.example"));
    }

    #[test]
    fn insert_header_kotlin_with_file_annotation() {
        let content = "@file:JvmName(\"X\")\npackage com.example\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Kotlin);
        let header_then_rest: Vec<&str> = out.lines().collect();
        assert_eq!(header_then_rest[0], "// Copyright (C) 2026 Zac Sweers");
        assert_eq!(header_then_rest[2], "@file:JvmName(\"X\")");
    }

    #[test]
    fn insert_header_kts_with_plugins() {
        let content = "plugins {\n  kotlin(\"jvm\")\n}\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Kts);
        assert!(out.starts_with("// Copyright (C) 2026"));
        assert!(out.contains("plugins {"));
    }

    #[test]
    fn insert_header_java_with_package() {
        let content = "package com.example;\n\nclass Foo {}\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Java);
        assert!(out.starts_with("// Copyright (C) 2026"));
        assert!(out.contains("package com.example;"));
    }

    #[test]
    fn insert_header_rust_with_inner_attribute() {
        let content = "#![allow(dead_code)]\n\npub fn answer() -> i32 {\n    1\n}\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Rust);
        assert!(out.starts_with("// Copyright (C) 2026"));
        assert!(out.contains("#![allow(dead_code)]"));
    }

    #[test]
    fn insert_header_rust_with_use_statement() {
        let content = "use std::path::Path;\n\nfn main() {}\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Rust);
        assert!(out.starts_with("// Copyright (C) 2026"));
        assert!(out.contains("use std::path::Path;"));
    }

    #[test]
    fn insert_header_replaces_pre_package_preamble() {
        // Mirrors bash behavior: comments before the delimiter line are
        // dropped when we insert.
        let content = "// stale leftover\n// more stale\npackage com.example\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Kotlin);
        assert!(!out.contains("stale"));
        assert!(out.starts_with("// Copyright (C) 2026"));
    }

    #[test]
    fn insert_header_no_delimiter_falls_back_to_prepend() {
        let content = "// just a snippet\nval x = 1\n";
        let header = render_header(&template(), 2026);
        let out = insert_header(content, &header, SourceKind::Kotlin);
        assert!(out.starts_with("// Copyright (C) 2026"));
        assert!(out.contains("val x = 1"));
    }

    #[test]
    fn insert_header_empty_content() {
        let header = render_header(&template(), 2026);
        let out = insert_header("", &header, SourceKind::Kotlin);
        assert_eq!(out, header);
    }
}
