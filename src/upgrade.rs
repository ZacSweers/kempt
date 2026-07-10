// Copyright (C) 2026 Zac Sweers
// SPDX-License-Identifier: Apache-2.0
//! `kempt upgrade`: bump the `version` fields in `.kempt.toml` to the
//! latest upstream releases.
//!
//! Sections using `path = "..."` (vendored binaries) and catalog
//! references (`version = { file = ..., key = ... }`) are skipped with a
//! note explaining where to make the change.
//!
//! The TOML edit goes through `toml_edit` so comments and formatting are
//! preserved.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::path::Path;
use toml_edit::DocumentMut;

/// What `run_upgrade` produced.
#[derive(Debug, Default, Clone)]
pub struct UpgradeOutcome {
    pub changes: Vec<Change>,
    pub skipped: Vec<String>,
    pub already_current: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub struct Change {
    pub tool: &'static str,
    pub from: String,
    pub to: String,
}

/// Source of latest-version answers. Real impl hits the network; tests use
/// a fake.
pub trait VersionFetcher {
    fn latest_ktfmt(&self) -> Result<String>;
    fn latest_gjf(&self) -> Result<String>;
    fn latest_detekt(&self, current: &str) -> Result<String>;
}

pub struct UreqVersionFetcher;

impl VersionFetcher for UreqVersionFetcher {
    fn latest_ktfmt(&self) -> Result<String> {
        let url = "https://repo1.maven.org/maven2/com/facebook/ktfmt/maven-metadata.xml";
        let body = ureq::get(url)
            .call()
            .with_context(|| format!("GET {url}"))?
            .into_body()
            .read_to_string()
            .context("read maven metadata body")?;
        extract_xml_tag(&body, "release")
            .ok_or_else(|| anyhow!("could not find <release> in ktfmt maven-metadata.xml at {url}"))
    }

    fn latest_gjf(&self) -> Result<String> {
        let url = "https://api.github.com/repos/google/google-java-format/releases/latest";
        let body = ureq::get(url)
            .header("User-Agent", "kempt")
            .header("Accept", "application/vnd.github+json")
            .call()
            .with_context(|| format!("GET {url}"))?
            .into_body()
            .read_to_string()
            .context("read github releases body")?;
        let tag = extract_json_string_field(&body, "tag_name")
            .ok_or_else(|| anyhow!("could not find `tag_name` in GitHub response from {url}"))?;
        Ok(tag.trim_start_matches('v').to_string())
    }

    fn latest_detekt(&self, current: &str) -> Result<String> {
        let url = "https://api.github.com/repos/detekt/detekt/releases?per_page=100";
        let body = ureq::get(url)
            .header("User-Agent", "kempt")
            .header("Accept", "application/vnd.github+json")
            .call()
            .with_context(|| format!("GET {url}"))?
            .into_body()
            .read_to_string()
            .context("read detekt releases body")?;
        let releases: Vec<GitHubRelease> =
            serde_json::from_str(&body).context("parse detekt releases response")?;
        select_detekt_release(current, &releases)
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
}

fn select_detekt_release(current: &str, releases: &[GitHubRelease]) -> Result<String> {
    let current = current.trim_start_matches('v');
    let current_major = current
        .split('.')
        .next()
        .ok_or_else(|| anyhow!("invalid detekt version `{current}`"))?;
    let release = if current.contains('-') {
        releases.iter().find(|release| {
            !release.draft
                && release.tag_name.trim_start_matches('v').split('.').next() == Some(current_major)
        })
    } else {
        releases.iter().find(|release| {
            !release.draft && !release.tag_name.trim_start_matches('v').contains('-')
        })
    };
    release
        .map(|release| release.tag_name.trim_start_matches('v').to_string())
        .ok_or_else(|| anyhow!("could not find a compatible detekt release for `{current}`"))
}

fn extract_xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)?;
    Some(body[start..start + end].trim().to_string())
}

fn extract_json_string_field(body: &str, field: &str) -> Option<String> {
    let key = format!("\"{field}\"");
    let key_pos = body.find(&key)?;
    let after = &body[key_pos + key.len()..];
    let colon = after.find(':')?;
    let after_colon = after[colon + 1..].trim_start();
    let stripped = after_colon.strip_prefix('"')?;
    let end = stripped.find('"')?;
    Some(stripped[..end].to_string())
}

/// Bump versions in the config at `config_path`. When `dry_run` is true
/// nothing is written; the returned [`UpgradeOutcome`] still describes
/// what would change.
pub fn run_upgrade(
    config_path: &Path,
    fetcher: &dyn VersionFetcher,
    dry_run: bool,
) -> Result<UpgradeOutcome> {
    let contents = std::fs::read_to_string(config_path)
        .with_context(|| format!("read {}", config_path.display()))?;
    let mut doc: DocumentMut = contents
        .parse()
        .with_context(|| format!("parse {} as TOML", config_path.display()))?;

    let mut outcome = UpgradeOutcome::default();

    upgrade_section(&mut doc, "ktfmt", &mut outcome, |_| fetcher.latest_ktfmt())?;
    upgrade_section(&mut doc, "gjf", &mut outcome, |_| fetcher.latest_gjf())?;
    upgrade_section(&mut doc, "detekt", &mut outcome, |current| {
        fetcher.latest_detekt(current)
    })?;

    if !dry_run && !outcome.changes.is_empty() {
        std::fs::write(config_path, doc.to_string())
            .with_context(|| format!("write {}", config_path.display()))?;
    }
    Ok(outcome)
}

fn upgrade_section(
    doc: &mut DocumentMut,
    tool: &'static str,
    outcome: &mut UpgradeOutcome,
    fetch: impl FnOnce(&str) -> Result<String>,
) -> Result<()> {
    let Some(item) = doc.get(tool) else {
        // Section not configured at all; nothing to upgrade.
        return Ok(());
    };
    let Some(table) = item.as_table_like() else {
        return Ok(());
    };
    let Some(version_item) = table.get("version") else {
        outcome
            .skipped
            .push(format!("{tool}: uses `path = ...`, skipping"));
        return Ok(());
    };
    let Some(current) = version_item.as_value().and_then(|v| v.as_str()) else {
        // Inline table = catalog reference. Bumping the catalog itself is
        // out of scope for this command.
        outcome.skipped.push(format!(
            "{tool}: uses a catalog reference; bump the catalog file directly"
        ));
        return Ok(());
    };
    let current = current.to_string();
    let latest = fetch(&current).with_context(|| format!("fetch latest {tool} version"))?;
    if latest == current {
        outcome.already_current.push(tool);
        return Ok(());
    }
    let table = doc
        .get_mut(tool)
        .and_then(|i| i.as_table_like_mut())
        .expect("table presence already checked");
    // Replace the value in place, restoring its decor so any trailing /
    // leading comments around the value are preserved.
    let item = table
        .get_mut("version")
        .expect("version presence already checked");
    let value_slot = item
        .as_value_mut()
        .ok_or_else(|| anyhow!("[{tool}] version is not a value (this is a bug)"))?;
    let decor = value_slot.decor().clone();
    *value_slot = toml_edit::Value::from(latest.clone());
    *value_slot.decor_mut() = decor;
    outcome.changes.push(Change {
        tool,
        from: current,
        to: latest,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeFetcher {
        ktfmt: Result<String>,
        gjf: Result<String>,
        detekt: Result<String>,
    }

    impl FakeFetcher {
        fn new(ktfmt: &str, gjf: &str) -> Self {
            Self {
                ktfmt: Ok(ktfmt.to_string()),
                gjf: Ok(gjf.to_string()),
                detekt: Ok("1.23.8".to_string()),
            }
        }
    }

    impl VersionFetcher for FakeFetcher {
        fn latest_ktfmt(&self) -> Result<String> {
            self.ktfmt
                .as_ref()
                .map(String::clone)
                .map_err(|e| anyhow!("{e}"))
        }
        fn latest_gjf(&self) -> Result<String> {
            self.gjf
                .as_ref()
                .map(String::clone)
                .map_err(|e| anyhow!("{e}"))
        }
        fn latest_detekt(&self, _current: &str) -> Result<String> {
            self.detekt
                .as_ref()
                .map(String::clone)
                .map_err(|e| anyhow!("{e}"))
        }
    }

    fn write_config(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join(".kempt.toml");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn upgrades_both_when_outdated() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_config(
            dir.path(),
            "[ktfmt]\nversion = \"0.50\"\n\n[gjf]\nversion = \"1.20.0\"\n",
        );
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        let outcome = run_upgrade(&p, &fetcher, false).unwrap();
        assert_eq!(outcome.changes.len(), 2);
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("version = \"0.62\""));
        assert!(body.contains("version = \"1.35.0\""));
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let original = "[ktfmt]\nversion = \"0.50\"\n";
        let p = write_config(dir.path(), original);
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        let outcome = run_upgrade(&p, &fetcher, true).unwrap();
        assert_eq!(outcome.changes.len(), 1);
        // File contents unchanged.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
    }

    #[test]
    fn no_changes_when_already_current() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_config(
            dir.path(),
            "[ktfmt]\nversion = \"0.62\"\n\n[gjf]\nversion = \"1.35.0\"\n",
        );
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        let outcome = run_upgrade(&p, &fetcher, false).unwrap();
        assert!(outcome.changes.is_empty());
        assert_eq!(outcome.already_current.len(), 2);
    }

    #[test]
    fn skips_path_based_section() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_config(
            dir.path(),
            "[ktfmt]\npath = \"config/bin/ktfmt.jar\"\n\n[gjf]\nversion = \"1.20.0\"\n",
        );
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        let outcome = run_upgrade(&p, &fetcher, true).unwrap();
        assert_eq!(outcome.changes.len(), 1);
        assert_eq!(outcome.changes[0].tool, "gjf");
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("ktfmt"));
        assert!(outcome.skipped[0].contains("path"));
    }

    #[test]
    fn skips_catalog_ref_section() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_config(
            dir.path(),
            "[ktfmt]\nversion = { file = \"libs.versions.toml\", key = \"ktfmt\" }\n",
        );
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        let outcome = run_upgrade(&p, &fetcher, true).unwrap();
        assert!(outcome.changes.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert!(outcome.skipped[0].contains("catalog"));
    }

    #[test]
    fn preserves_comments_and_formatting() {
        let dir = tempfile::tempdir().unwrap();
        let original = "\
# important comment
[ktfmt]
version = \"0.50\"   # inline comment
style = \"google\"
";
        let p = write_config(dir.path(), original);
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        run_upgrade(&p, &fetcher, false).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert!(body.contains("# important comment"));
        assert!(body.contains("# inline comment"));
        assert!(body.contains("style = \"google\""));
        assert!(body.contains("version = \"0.62\""));
    }

    #[test]
    fn missing_section_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        // Only [ktfmt] configured; [gjf] missing.
        let p = write_config(dir.path(), "[ktfmt]\nversion = \"0.50\"\n");
        let fetcher = FakeFetcher::new("0.62", "1.35.0");
        let outcome = run_upgrade(&p, &fetcher, false).unwrap();
        assert_eq!(outcome.changes.len(), 1);
        // No skip noise for an absent section.
        assert!(outcome.skipped.is_empty());
    }

    #[test]
    fn upgrades_detekt_without_crossing_back_from_v2_prereleases() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_config(dir.path(), "[detekt]\nversion = \"2.0.0-alpha.3\"\n");
        let mut fetcher = FakeFetcher::new("0.62", "1.35.0");
        fetcher.detekt = Ok("2.0.0-alpha.5".into());
        let outcome = run_upgrade(&p, &fetcher, false).unwrap();
        assert_eq!(outcome.changes.len(), 1);
        assert_eq!(outcome.changes[0].tool, "detekt");
        assert!(std::fs::read_to_string(p)
            .unwrap()
            .contains("2.0.0-alpha.5"));
    }

    #[test]
    fn detekt_release_selection_preserves_prerelease_major() {
        let releases = vec![
            GitHubRelease {
                tag_name: "v1.23.8".into(),
                draft: false,
            },
            GitHubRelease {
                tag_name: "v2.0.0-alpha.5".into(),
                draft: false,
            },
        ];
        assert_eq!(
            select_detekt_release("2.0.0-alpha.3", &releases).unwrap(),
            "2.0.0-alpha.5"
        );
    }

    // --- extract helpers ---

    #[test]
    fn extract_release_tag_from_maven_metadata() {
        let body = "\
<metadata>
  <groupId>com.facebook</groupId>
  <artifactId>ktfmt</artifactId>
  <versioning>
    <release>0.62</release>
    <versions><version>0.61</version><version>0.62</version></versions>
  </versioning>
</metadata>";
        assert_eq!(extract_xml_tag(body, "release").as_deref(), Some("0.62"));
    }

    #[test]
    fn extract_tag_name_from_github_response() {
        let body = r#"{"name":"1.35.0","tag_name":"v1.35.0","draft":false}"#;
        assert_eq!(
            extract_json_string_field(body, "tag_name").as_deref(),
            Some("v1.35.0")
        );
    }

    #[test]
    fn extract_tag_name_handles_whitespace_around_colon() {
        let body = r#"{ "tag_name" :  "v0.62" }"#;
        assert_eq!(
            extract_json_string_field(body, "tag_name").as_deref(),
            Some("v0.62")
        );
    }
}
