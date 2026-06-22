//! In-process formatting steps (license header + whitespace) that don't need
//! to spawn a JVM. Keeping them here makes the orchestration easy to unit-test.

use crate::config::{Config, ResolvedHeader};
use crate::license::{self, SourceKind};
use crate::whitespace;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// Per-file outcome for the in-process steps.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FileReport {
    pub header_added: bool,
    pub whitespace_fixed: bool,
}

impl FileReport {
    pub fn changed(&self) -> bool {
        self.header_added || self.whitespace_fixed
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PipelineReport {
    /// Files that were (or would be) modified by the in-process steps.
    pub changed: Vec<PathBuf>,
}

impl PipelineReport {
    pub fn record(&mut self, path: &Path, r: &FileReport) {
        if r.changed() {
            self.changed.push(path.to_path_buf());
        }
    }
}

/// Materialized license header data for one tool's languages.
#[derive(Debug)]
pub struct HeaderSpec {
    pub rendered: String,
    pub marker: String,
    pub excludes: GlobSet,
}

impl HeaderSpec {
    pub fn load(resolved: &ResolvedHeader, repo_root: &Path, year: u32) -> Result<Self> {
        let template_path = repo_root.join(&resolved.file);
        let template = std::fs::read_to_string(&template_path)
            .with_context(|| format!("read license header template {}", template_path.display()))?;
        let rendered = license::render_header(&template, year);
        let marker = license::marker_for_template(&template);
        let excludes = load_excludes(resolved.excludes.as_deref(), repo_root)?;
        Ok(Self {
            rendered,
            marker,
            excludes,
        })
    }

    pub fn is_excluded(&self, file: &Path) -> bool {
        self.excludes.is_match(file)
    }
}

/// Headers resolved per-language. `kotlin` covers `.kt` and `.kts`, `java`
/// covers `.java`, and `rust` covers `.rs`. Any may be `None` if the config
/// doesn't define a header file for that language.
#[derive(Default)]
pub struct Headers {
    pub kotlin: Option<HeaderSpec>,
    pub java: Option<HeaderSpec>,
    pub rust: Option<HeaderSpec>,
}

impl Headers {
    pub fn build(config: &Config, repo_root: &Path, year: u32) -> Result<Self> {
        let kotlin = match config.ktfmt_header() {
            Some(r) => Some(HeaderSpec::load(&r, repo_root, year)?),
            None => None,
        };
        let java = match config.gjf_header() {
            Some(r) => Some(HeaderSpec::load(&r, repo_root, year)?),
            None => None,
        };
        let rust = match config.rustfmt_header() {
            Some(r) => Some(HeaderSpec::load(&r, repo_root, year)?),
            None => None,
        };
        Ok(Self { kotlin, java, rust })
    }

    pub fn for_kind(&self, kind: SourceKind) -> Option<&HeaderSpec> {
        match kind {
            SourceKind::Kotlin | SourceKind::Kts => self.kotlin.as_ref(),
            SourceKind::Java => self.java.as_ref(),
            SourceKind::Rust => self.rust.as_ref(),
        }
    }
}

fn load_excludes(path: Option<&Path>, repo_root: &Path) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let Some(rel) = path else {
        return builder.build().context("build empty exclude globset");
    };
    let abs = repo_root.join(rel);
    if !abs.exists() {
        return builder.build().context("build empty exclude globset");
    }
    let contents =
        std::fs::read_to_string(&abs).with_context(|| format!("read {}", abs.display()))?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let glob = Glob::new(trimmed)
            .with_context(|| format!("invalid exclude glob {trimmed:?} in {}", abs.display()))?;
        builder.add(glob);
    }
    builder.build().context("build exclude globset")
}

/// Apply the in-process pipeline to a single file's content.
/// Returns the new content (which may equal the old content) and a report.
pub fn process_content(
    content: &str,
    kind: SourceKind,
    header: Option<(&str, &str)>, // (rendered, marker)
    ws: whitespace::Options,
) -> (String, FileReport) {
    let mut report = FileReport::default();
    let mut current = std::borrow::Cow::Borrowed(content);

    if let Some((rendered, marker)) = header {
        if !license::has_header(&current, marker) {
            let new = license::insert_header(&current, rendered, kind);
            current = std::borrow::Cow::Owned(new);
            report.header_added = true;
        }
    }

    if (ws.strip_trailing || ws.final_newline) && whitespace::diagnose(&current, ws).any() {
        let fixed = whitespace::fix(&current, ws);
        current = std::borrow::Cow::Owned(fixed);
        report.whitespace_fixed = true;
    }

    (current.into_owned(), report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws_off() -> whitespace::Options {
        whitespace::Options::default()
    }

    fn ws_on() -> whitespace::Options {
        whitespace::Options {
            strip_trailing: true,
            final_newline: true,
        }
    }

    #[test]
    fn process_inserts_header_when_missing() {
        let (out, r) = process_content(
            "package foo\n",
            SourceKind::Kotlin,
            Some(("// (c) 2026\n", "(c)")),
            ws_off(),
        );
        assert!(r.header_added);
        assert!(out.starts_with("// (c) 2026"));
    }

    #[test]
    fn process_skips_header_when_present() {
        let (_out, r) = process_content(
            "// (c) 2025\npackage foo\n",
            SourceKind::Kotlin,
            Some(("// (c) 2026\n", "(c)")),
            ws_off(),
        );
        assert!(!r.header_added);
    }

    #[test]
    fn process_fixes_whitespace_when_dirty() {
        let (out, r) = process_content("package foo   \n", SourceKind::Kotlin, None, ws_on());
        assert!(r.whitespace_fixed);
        assert_eq!(out, "package foo\n");
    }

    #[test]
    fn process_clean_input_no_changes() {
        let (out, r) = process_content("package foo\n", SourceKind::Kotlin, None, ws_on());
        assert!(!r.changed());
        assert_eq!(out, "package foo\n");
    }

    #[test]
    fn process_combines_steps() {
        let (out, r) = process_content(
            "package foo   \n",
            SourceKind::Kotlin,
            Some(("// h\n", "// h")),
            ws_on(),
        );
        assert!(r.header_added);
        assert!(r.whitespace_fixed);
        assert!(out.starts_with("// h\n"));
        assert!(!out.contains("foo   "));
    }

    #[test]
    fn process_with_only_strip_trailing_leaves_missing_newline_alone() {
        let opts = whitespace::Options {
            strip_trailing: true,
            final_newline: false,
        };
        let (out, r) = process_content("foo   ", SourceKind::Kotlin, None, opts);
        assert!(r.whitespace_fixed);
        // Stripped trailing spaces but did not add a final newline.
        assert_eq!(out, "foo");
    }

    #[test]
    fn process_with_only_final_newline_leaves_trailing_alone() {
        let opts = whitespace::Options {
            strip_trailing: false,
            final_newline: true,
        };
        let (out, r) = process_content("foo   ", SourceKind::Kotlin, None, opts);
        assert!(r.whitespace_fixed);
        // Added newline but kept the trailing whitespace.
        assert_eq!(out, "foo   \n");
    }

    #[test]
    fn report_records_only_changed_files() {
        let mut rep = PipelineReport::default();
        rep.record(
            Path::new("a.kt"),
            &FileReport {
                header_added: true,
                whitespace_fixed: false,
            },
        );
        rep.record(Path::new("b.kt"), &FileReport::default());
        assert_eq!(rep.changed, vec![PathBuf::from("a.kt")]);
    }

    fn header_spec_with_excludes(patterns: &[&str]) -> HeaderSpec {
        let dir = tempfile::tempdir().unwrap();
        let template = dir.path().join("header.txt");
        std::fs::write(&template, "// (c) ${YEAR}\n").unwrap();
        let excludes_file = dir.path().join("excludes.txt");
        std::fs::write(&excludes_file, patterns.join("\n")).unwrap();
        let resolved = ResolvedHeader {
            file: PathBuf::from("header.txt"),
            excludes: Some(PathBuf::from("excludes.txt")),
        };
        HeaderSpec::load(&resolved, dir.path(), 2026).unwrap()
    }

    #[test]
    fn excludes_glob_basename_pattern_matches_nested_path() {
        // Regression: `**/Foo.kt` was previously stored as a literal PathBuf
        // and only matched the literal string "**/Foo.kt", so deeply-nested
        // files were never excluded.
        let spec = header_spec_with_excludes(&["**/AbstractMapFactory.kt"]);
        assert!(spec.is_excluded(Path::new(
            "runtime/src/commonMain/kotlin/dev/zacsweers/metro/internal/AbstractMapFactory.kt"
        )));
        assert!(spec.is_excluded(Path::new("AbstractMapFactory.kt")));
        assert!(!spec.is_excluded(Path::new(
            "runtime/src/commonMain/kotlin/dev/zacsweers/metro/internal/Other.kt"
        )));
    }

    #[test]
    fn excludes_glob_directory_pattern() {
        let spec = header_spec_with_excludes(&["**/src/test/data/**"]);
        assert!(spec.is_excluded(Path::new("compiler/src/test/data/foo.kt")));
        assert!(spec.is_excluded(Path::new("compiler/src/test/data/nested/bar.kt")));
        assert!(!spec.is_excluded(Path::new("compiler/src/main/kotlin/foo.kt")));
    }

    #[test]
    fn excludes_skips_blank_and_comment_lines() {
        let spec = header_spec_with_excludes(&["# header comment", "", "**/Foo.kt", "  # another"]);
        assert!(spec.is_excluded(Path::new("a/b/Foo.kt")));
    }

    #[test]
    fn excludes_invalid_glob_errors_with_helpful_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("header.txt"), "// (c) ${YEAR}\n").unwrap();
        std::fs::write(dir.path().join("excludes.txt"), "[bad").unwrap();
        let resolved = ResolvedHeader {
            file: PathBuf::from("header.txt"),
            excludes: Some(PathBuf::from("excludes.txt")),
        };
        let err = HeaderSpec::load(&resolved, dir.path(), 2026).unwrap_err();
        assert!(format!("{err:#}").contains("[bad"));
    }
}
