use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = ".kempt.toml";

/// Where a tool's binary comes from after applying the rules in `[ktfmt]` /
/// `[gjf]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    /// Download `version` into the user cache.
    Cached(String),
    /// Use a binary already on disk (in-repo or absolute).
    Local(PathBuf),
}

/// Either a literal version string (`"0.62"`) or a reference to a
/// Gradle-style version catalog (`{ file = "...", key = "..." }`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum VersionSpec {
    Literal(String),
    Ref(VersionRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct VersionRef {
    /// Path to a catalog TOML, relative to repo root or absolute.
    pub file: PathBuf,
    /// Lookup key under the catalog's `[versions]` table. Defaults to the
    /// tool name (`ktfmt` or `gjf`).
    pub key: Option<String>,
}

/// A list of glob patterns. Sourced either inline (`["**/*.kt"]`) or from a
/// text file (one glob per line, `#` comments allowed).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum GlobList {
    Inline(Vec<String>),
    FromFile(PathBuf),
}

impl GlobList {
    /// Resolve to a flat `Vec<String>` of glob patterns. File-sourced lists
    /// are read relative to `repo_root`.
    pub fn resolve(&self, repo_root: &Path) -> Result<Vec<String>> {
        match self {
            Self::Inline(v) => Ok(v.clone()),
            Self::FromFile(p) => load_glob_file(repo_root, p),
        }
    }
}

fn load_glob_file(repo_root: &Path, p: &Path) -> Result<Vec<String>> {
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        repo_root.join(p)
    };
    let contents = std::fs::read_to_string(&abs)
        .with_context(|| format!("read glob file {}", abs.display()))?;
    Ok(contents
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}

/// Per-tool path scope. Both fields polymorphic via [`GlobList`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ToolPaths {
    pub include: Option<GlobList>,
    pub exclude: Option<GlobList>,
}

/// Path scope after defaults have been filled in and any file references
/// resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPaths {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl ToolPaths {
    /// Resolve against per-tool defaults. Either field returns the user's
    /// value when set, otherwise the default.
    pub fn resolve_with_defaults(
        &self,
        repo_root: &Path,
        default_include: &[&str],
        default_exclude: &[&str],
    ) -> Result<ResolvedPaths> {
        let include = match &self.include {
            Some(g) => g.resolve(repo_root)?,
            None => default_include.iter().map(|s| (*s).to_string()).collect(),
        };
        let exclude = match &self.exclude {
            Some(g) => g.resolve(repo_root)?,
            None => default_exclude.iter().map(|s| (*s).to_string()).collect(),
        };
        Ok(ResolvedPaths { include, exclude })
    }
}

impl VersionSpec {
    /// Convenience constructor for tests.
    #[cfg(test)]
    pub(crate) fn literal(s: impl Into<String>) -> Self {
        Self::Literal(s.into())
    }

    /// Returns the literal string. Panics on `Ref`; callers must run
    /// [`Config::resolve_catalogs`] before reading versions.
    pub fn as_literal(&self) -> &str {
        match self {
            Self::Literal(s) => s,
            Self::Ref(_) => {
                panic!("unresolved catalog ref; call Config::resolve_catalogs first")
            }
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    pub ktfmt: Option<Ktfmt>,
    pub gjf: Option<Gjf>,
    pub license_header: Option<LicenseHeader>,
    #[serde(default)]
    pub paths: Paths,
    #[serde(default)]
    pub whitespace: Whitespace,
    #[serde(default)]
    pub hook: Hook,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Ktfmt {
    /// Maven Central version. Mutually exclusive with `path`. Accepts either
    /// a literal string (`"0.62"`) or a catalog reference table
    /// (`{ file = "gradle/libs.versions.toml", key = "ktfmt" }`).
    pub version: Option<VersionSpec>,
    /// Path to a checked-in formatter binary (relative to the repo root, or
    /// absolute). Mutually exclusive with `version`. Use this when you commit
    /// the formatter into the repo for hermetic / offline builds.
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub style: KtfmtStyle,
    pub license_header: Option<ToolLicenseHeader>,
    /// Tool-specific path scope. Defaults to `**/*.kt` and `**/*.kts`.
    pub paths: Option<ToolPaths>,
}

impl Ktfmt {
    pub fn resolve_paths(&self, repo_root: &Path) -> Result<ResolvedPaths> {
        let p = self.paths.clone().unwrap_or_default();
        p.resolve_with_defaults(repo_root, &["**/*.kt", "**/*.kts"], &[])
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum KtfmtStyle {
    #[default]
    Google,
    Kotlinlang,
    Meta,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Gjf {
    /// Release version on GitHub. Mutually exclusive with `path`. Accepts
    /// either a literal string or a catalog reference (see [`VersionSpec`]).
    pub version: Option<VersionSpec>,
    /// Path to a checked-in formatter binary (relative to the repo root, or
    /// absolute). Mutually exclusive with `version`. The file extension
    /// determines how kempt invokes it: `.jar` runs via `java -jar`, anything
    /// else is run directly (e.g. a GraalVM native build).
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub style: GjfStyle,
    pub license_header: Option<ToolLicenseHeader>,
    /// Whether to use gjf's GraalVM-native executable (no JVM, instant
    /// startup) instead of the JVM jar. `auto` (default) uses native when
    /// available for this platform + version, falls back to jar. `always`
    /// errors if native isn't available. `never` always uses the jar.
    /// Ignored when `path` is set.
    #[serde(default)]
    pub native: NativeMode,
    /// Tool-specific path scope. Defaults to `**/*.java`.
    pub paths: Option<ToolPaths>,
}

impl Gjf {
    pub fn resolve_paths(&self, repo_root: &Path) -> Result<ResolvedPaths> {
        let p = self.paths.clone().unwrap_or_default();
        p.resolve_with_defaults(repo_root, &["**/*.java"], &[])
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum GjfStyle {
    #[default]
    Google,
    Aosp,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NativeMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LicenseHeader {
    pub file: PathBuf,
}

/// Per-tool override of the license-header settings. Both fields are optional;
/// `file` overrides the global `[license-header].file`, and `excludes` is a
/// pointer to an exclude-list file that applies only to that tool's languages.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ToolLicenseHeader {
    pub file: Option<PathBuf>,
    pub excludes: Option<PathBuf>,
}

/// Resolved header settings for a single tool. `file` is required (the caller
/// only constructs this if either the tool override or the global config
/// supplied one); `excludes` is optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedHeader {
    pub file: PathBuf,
    pub excludes: Option<PathBuf>,
}

/// Universal path filter applied before any tool-specific scope. Currently
/// only excludes are configurable here; per-language inclusion lives in
/// `[ktfmt.paths]`, `[gjf.paths]`, and `[whitespace.paths]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Paths {
    #[serde(default = "default_global_exclude")]
    pub exclude: GlobList,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            exclude: default_global_exclude(),
        }
    }
}

fn default_global_exclude() -> GlobList {
    GlobList::Inline(vec!["**/build/**".into(), "**/target/**".into()])
}

/// Whitespace normalization knobs. Both pass-toggles default to enabled.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Whitespace {
    /// Strip trailing space/tab/CR from the end of every line.
    #[serde(default = "default_true")]
    pub strip_trailing: bool,
    /// Ensure files end with exactly one trailing newline.
    #[serde(default = "default_true")]
    pub final_newline: bool,
    /// Tool-specific path scope. Defaults to all source files we know about.
    pub paths: Option<ToolPaths>,
}

impl Default for Whitespace {
    fn default() -> Self {
        Self {
            strip_trailing: true,
            final_newline: true,
            paths: None,
        }
    }
}

impl Whitespace {
    pub fn resolve_paths(&self, repo_root: &Path) -> Result<ResolvedPaths> {
        let p = self.paths.clone().unwrap_or_default();
        p.resolve_with_defaults(repo_root, &["**/*.kt", "**/*.kts", "**/*.java"], &[])
    }
}

impl Whitespace {
    /// True when at least one whitespace pass is enabled.
    #[cfg(test)]
    pub(crate) fn any_enabled(&self) -> bool {
        self.strip_trailing || self.final_newline
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Hook {
    #[serde(default)]
    pub mode: HookMode,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum HookMode {
    #[default]
    Format,
    Check,
}

impl Config {
    pub fn parse(s: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(s).context("failed to parse config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config at {}", path.display()))?;
        Self::parse(&contents)
            .with_context(|| format!("failed to parse config at {}", path.display()))
    }

    /// Resolve the header settings for ktfmt-managed languages (`.kt`, `.kts`).
    /// Returns `None` when neither the global nor the per-tool section
    /// supplies a header file.
    pub fn ktfmt_header(&self) -> Option<ResolvedHeader> {
        let tool = self.ktfmt.as_ref().and_then(|k| k.license_header.as_ref());
        resolve_header(self.license_header.as_ref(), tool)
    }

    /// Resolve the header settings for gjf-managed languages (`.java`).
    pub fn gjf_header(&self) -> Option<ResolvedHeader> {
        let tool = self.gjf.as_ref().and_then(|g| g.license_header.as_ref());
        resolve_header(self.license_header.as_ref(), tool)
    }

    fn validate(&self) -> Result<()> {
        if let Some(kt) = &self.ktfmt {
            validate_source_xor(kt.version.as_ref(), kt.path.as_deref(), "ktfmt")?;
        }
        if let Some(g) = &self.gjf {
            validate_source_xor(g.version.as_ref(), g.path.as_deref(), "gjf")?;
        }
        Ok(())
    }

    /// Resolve any [`VersionSpec::Ref`] entries against the filesystem.
    /// After this returns Ok, every `Ktfmt::version` / `Gjf::version` is
    /// either `None` or `Some(VersionSpec::Literal(_))`.
    ///
    /// The same catalog file is parsed at most once per call.
    pub fn resolve_catalogs(mut self, repo_root: &Path) -> Result<Self> {
        let mut cache: HashMap<PathBuf, ParsedCatalog> = HashMap::new();
        if let Some(kt) = &mut self.ktfmt {
            if let Some(VersionSpec::Ref(r)) = kt.version.clone() {
                let v = resolve_catalog_ref(&r, repo_root, "ktfmt", &mut cache)?;
                kt.version = Some(VersionSpec::Literal(v));
            }
        }
        if let Some(g) = &mut self.gjf {
            if let Some(VersionSpec::Ref(r)) = g.version.clone() {
                let v = resolve_catalog_ref(&r, repo_root, "gjf", &mut cache)?;
                g.version = Some(VersionSpec::Literal(v));
            }
        }
        Ok(self)
    }
}

impl Ktfmt {
    pub fn source(&self, repo_root: &Path) -> ToolSource {
        resolve_source(
            self.version.as_ref().map(|v| v.as_literal()),
            self.path.as_deref(),
            repo_root,
        )
    }
}

impl Gjf {
    pub fn source(&self, repo_root: &Path) -> ToolSource {
        resolve_source(
            self.version.as_ref().map(|v| v.as_literal()),
            self.path.as_deref(),
            repo_root,
        )
    }
}

fn validate_source_xor(
    version: Option<&VersionSpec>,
    path: Option<&Path>,
    tool: &str,
) -> Result<()> {
    match (version.is_some(), path.is_some()) {
        (true, true) => Err(anyhow!("[{tool}] sets both `version` and `path`; pick one")),
        (false, false) => Err(anyhow!("[{tool}] must set either `version` or `path`")),
        _ => Ok(()),
    }
}

fn resolve_source(version: Option<&str>, path: Option<&Path>, repo_root: &Path) -> ToolSource {
    if let Some(p) = path {
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            repo_root.join(p)
        };
        ToolSource::Local(abs)
    } else {
        ToolSource::Cached(version.expect("validated").to_string())
    }
}

struct ParsedCatalog {
    /// Plain string entries under `[versions]`.
    versions: HashMap<String, String>,
    /// Keys whose values were structured (rich) Gradle versions, kept so
    /// we can emit a more helpful error if the user requests one.
    rich_keys: Vec<String>,
}

fn resolve_catalog_ref(
    r: &VersionRef,
    repo_root: &Path,
    default_key: &str,
    cache: &mut HashMap<PathBuf, ParsedCatalog>,
) -> Result<String> {
    let abs = if r.file.is_absolute() {
        r.file.clone()
    } else {
        repo_root.join(&r.file)
    };
    if !cache.contains_key(&abs) {
        cache.insert(abs.clone(), load_catalog(&abs)?);
    }
    let parsed = cache
        .get(&abs)
        .expect("inserted just above or already present");
    let key = r.key.as_deref().unwrap_or(default_key);
    if let Some(v) = parsed.versions.get(key) {
        return Ok(v.clone());
    }
    if parsed.rich_keys.iter().any(|k| k == key) {
        return Err(anyhow!(
            "catalog key `{key}` in {} is a structured (rich) Gradle version; kempt only supports literal version strings",
            abs.display()
        ));
    }
    Err(anyhow!(
        "catalog key `{key}` not found under [versions] in {}",
        abs.display()
    ))
}

fn load_catalog(abs: &Path) -> Result<ParsedCatalog> {
    let contents =
        std::fs::read_to_string(abs).with_context(|| format!("read catalog {}", abs.display()))?;
    let raw: toml::Value =
        toml::from_str(&contents).with_context(|| format!("parse catalog {}", abs.display()))?;
    let table = raw
        .as_table()
        .ok_or_else(|| anyhow!("catalog is not a TOML table: {}", abs.display()))?;
    let versions_table = match table.get("versions") {
        Some(toml::Value::Table(t)) => t,
        Some(_) => return Err(anyhow!("[versions] is not a table in {}", abs.display())),
        None => return Err(anyhow!("[versions] table not found in {}", abs.display())),
    };
    let mut versions = HashMap::new();
    let mut rich_keys = Vec::new();
    for (k, v) in versions_table {
        match v {
            toml::Value::String(s) => {
                versions.insert(k.clone(), s.clone());
            }
            toml::Value::Table(_) => rich_keys.push(k.clone()),
            _ => {}
        }
    }
    Ok(ParsedCatalog {
        versions,
        rich_keys,
    })
}

fn resolve_header(
    global: Option<&LicenseHeader>,
    tool: Option<&ToolLicenseHeader>,
) -> Option<ResolvedHeader> {
    let file = tool
        .and_then(|t| t.file.clone())
        .or_else(|| global.map(|g| g.file.clone()))?;
    let excludes = tool.and_then(|t| t.excludes.clone());
    Some(ResolvedHeader { file, excludes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_yields_defaults() {
        let c = Config::parse("").unwrap();
        assert!(c.ktfmt.is_none());
        assert!(c.gjf.is_none());
        assert!(c.license_header.is_none());
        assert!(c.whitespace.strip_trailing);
        assert!(c.whitespace.final_newline);
        assert_eq!(c.hook.mode, HookMode::Format);
        // Default global exclude is built-in (build/, target/).
        match &c.paths.exclude {
            GlobList::Inline(v) => {
                assert!(v.contains(&"**/build/**".to_string()));
                assert!(v.contains(&"**/target/**".to_string()));
            }
            GlobList::FromFile(_) => panic!("expected inline list by default"),
        }
    }

    #[test]
    fn ktfmt_section_default_style_is_google() {
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"
        "#,
        )
        .unwrap();
        let kt = c.ktfmt.unwrap();
        assert_eq!(kt.version.as_ref().unwrap().as_literal(), "0.56");
        assert!(kt.path.is_none());
        assert_eq!(kt.style, KtfmtStyle::Google);
    }

    #[test]
    fn ktfmt_explicit_style() {
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"
            style = "kotlinlang"
        "#,
        )
        .unwrap();
        assert_eq!(c.ktfmt.unwrap().style, KtfmtStyle::Kotlinlang);
    }

    #[test]
    fn gjf_section_default_style_is_google() {
        let c = Config::parse(
            r#"
            [gjf]
            version = "1.28.0"
        "#,
        )
        .unwrap();
        let g = c.gjf.unwrap();
        assert_eq!(g.version.as_ref().unwrap().as_literal(), "1.28.0");
        assert!(g.path.is_none());
        assert_eq!(g.style, GjfStyle::Google);
    }

    #[test]
    fn gjf_aosp_style() {
        let c = Config::parse(
            r#"
            [gjf]
            version = "1.28.0"
            style = "aosp"
        "#,
        )
        .unwrap();
        assert_eq!(c.gjf.unwrap().style, GjfStyle::Aosp);
    }

    #[test]
    fn license_header_global_only() {
        let c = Config::parse(
            r#"
            [license-header]
            file = "config/header.txt"
        "#,
        )
        .unwrap();
        let lh = c.license_header.unwrap();
        assert_eq!(lh.file, PathBuf::from("config/header.txt"));
    }

    #[test]
    fn ktfmt_license_header_excludes_only() {
        let c = Config::parse(
            r#"
            [license-header]
            file = "config/header.txt"

            [ktfmt]
            version = "0.56"

            [ktfmt.license-header]
            excludes = "config/excludes-kt.txt"
        "#,
        )
        .unwrap();
        let resolved = c.ktfmt_header().unwrap();
        assert_eq!(resolved.file, PathBuf::from("config/header.txt"));
        assert_eq!(
            resolved.excludes,
            Some(PathBuf::from("config/excludes-kt.txt"))
        );
    }

    #[test]
    fn ktfmt_license_header_file_overrides_global() {
        let c = Config::parse(
            r#"
            [license-header]
            file = "config/global.txt"

            [ktfmt]
            version = "0.56"

            [ktfmt.license-header]
            file = "config/kt.txt"
        "#,
        )
        .unwrap();
        let resolved = c.ktfmt_header().unwrap();
        assert_eq!(resolved.file, PathBuf::from("config/kt.txt"));
        assert!(resolved.excludes.is_none());
    }

    #[test]
    fn gjf_license_header_uses_global_when_only_excludes_set() {
        let c = Config::parse(
            r#"
            [license-header]
            file = "config/global.txt"

            [gjf]
            version = "1.28.0"

            [gjf.license-header]
            excludes = "config/excludes-java.txt"
        "#,
        )
        .unwrap();
        let resolved = c.gjf_header().unwrap();
        assert_eq!(resolved.file, PathBuf::from("config/global.txt"));
        assert_eq!(
            resolved.excludes,
            Some(PathBuf::from("config/excludes-java.txt"))
        );
    }

    #[test]
    fn license_header_only_per_tool_no_global_works() {
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"

            [ktfmt.license-header]
            file = "config/kt.txt"
        "#,
        )
        .unwrap();
        assert_eq!(
            c.ktfmt_header().unwrap().file,
            PathBuf::from("config/kt.txt")
        );
        // gjf has no per-tool override and no global file → no header
        assert!(c.gjf_header().is_none());
    }

    #[test]
    fn license_header_disabled_when_neither_global_nor_tool_provides_file() {
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"

            [ktfmt.license-header]
            excludes = "config/excludes-kt.txt"
        "#,
        )
        .unwrap();
        // No file anywhere → resolution returns None even though excludes is set.
        assert!(c.ktfmt_header().is_none());
    }

    #[test]
    fn no_license_header_config_yields_no_resolution() {
        let c = Config::parse("").unwrap();
        assert!(c.ktfmt_header().is_none());
        assert!(c.gjf_header().is_none());
    }

    #[test]
    fn paths_exclude_replaces_default() {
        let c = Config::parse(
            r#"
            [paths]
            exclude = ["**/generated/**"]
        "#,
        )
        .unwrap();
        match &c.paths.exclude {
            GlobList::Inline(v) => assert_eq!(v, &vec!["**/generated/**".to_string()]),
            GlobList::FromFile(_) => panic!(),
        }
    }

    #[test]
    fn paths_exclude_can_be_a_file_path() {
        let c = Config::parse(
            r#"
            [paths]
            exclude = "config/global-excludes.txt"
        "#,
        )
        .unwrap();
        match &c.paths.exclude {
            GlobList::FromFile(p) => assert_eq!(p, &PathBuf::from("config/global-excludes.txt")),
            GlobList::Inline(_) => panic!("expected file path form"),
        }
    }

    #[test]
    fn paths_include_field_no_longer_supported() {
        // Removed; user should use per-tool [<tool>.paths].include.
        let err = Config::parse(
            r#"
            [paths]
            include = ["**/*.kt"]
        "#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("unknown") || format!("{err:#}").contains("include"));
    }

    #[test]
    fn ktfmt_paths_default_to_kt_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.62"
        "#,
        )
        .unwrap();
        let kt = c.ktfmt.unwrap();
        let resolved = kt.resolve_paths(dir.path()).unwrap();
        assert_eq!(resolved.include, vec!["**/*.kt", "**/*.kts"]);
        assert!(resolved.exclude.is_empty());
    }

    #[test]
    fn gjf_paths_default_to_java_extension() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::parse(
            r#"
            [gjf]
            version = "1.35.0"
        "#,
        )
        .unwrap();
        let g = c.gjf.unwrap();
        let resolved = g.resolve_paths(dir.path()).unwrap();
        assert_eq!(resolved.include, vec!["**/*.java"]);
    }

    #[test]
    fn whitespace_paths_default_to_all_known_languages() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = Whitespace::default().resolve_paths(dir.path()).unwrap();
        assert_eq!(resolved.include, vec!["**/*.kt", "**/*.kts", "**/*.java"]);
    }

    #[test]
    fn tool_paths_override_take_precedence_over_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.62"

            [ktfmt.paths]
            include = ["src/**/*.kt"]
            exclude = ["**/Generated.kt"]
        "#,
        )
        .unwrap();
        let resolved = c.ktfmt.unwrap().resolve_paths(dir.path()).unwrap();
        assert_eq!(resolved.include, vec!["src/**/*.kt"]);
        assert_eq!(resolved.exclude, vec!["**/Generated.kt"]);
    }

    #[test]
    fn glob_list_accepts_inline_array_or_file_path() {
        // Inline.
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.62"

            [ktfmt.paths]
            include = ["**/*.kt"]
        "#,
        )
        .unwrap();
        match &c.ktfmt.as_ref().unwrap().paths.as_ref().unwrap().include {
            Some(GlobList::Inline(v)) => assert_eq!(v, &vec!["**/*.kt".to_string()]),
            other => panic!("expected inline, got {other:?}"),
        }

        // File path.
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.62"

            [ktfmt.paths]
            exclude = "config/ktfmt-skip.txt"
        "#,
        )
        .unwrap();
        match &c.ktfmt.as_ref().unwrap().paths.as_ref().unwrap().exclude {
            Some(GlobList::FromFile(p)) => {
                assert_eq!(p, &PathBuf::from("config/ktfmt-skip.txt"))
            }
            other => panic!("expected file-path form, got {other:?}"),
        }
    }

    #[test]
    fn glob_list_resolves_file_path_to_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("excludes.txt");
        std::fs::write(
            &p,
            "# header comment\n\n**/Skip.kt\n\n# trailing comment\n**/Other.kt\n",
        )
        .unwrap();
        let list = GlobList::FromFile(PathBuf::from("excludes.txt"));
        let resolved = list.resolve(dir.path()).unwrap();
        assert_eq!(resolved, vec!["**/Skip.kt", "**/Other.kt"]);
    }

    #[test]
    fn glob_list_resolves_inline_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let list = GlobList::Inline(vec!["**/*.kt".into(), "**/*.kts".into()]);
        assert_eq!(
            list.resolve(dir.path()).unwrap(),
            vec!["**/*.kt", "**/*.kts"]
        );
    }

    #[test]
    fn whitespace_strip_trailing_can_be_disabled() {
        let c = Config::parse(
            r#"
            [whitespace]
            strip-trailing = false
        "#,
        )
        .unwrap();
        assert!(!c.whitespace.strip_trailing);
        // The other knob keeps its default.
        assert!(c.whitespace.final_newline);
    }

    #[test]
    fn whitespace_final_newline_can_be_disabled() {
        let c = Config::parse(
            r#"
            [whitespace]
            final-newline = false
        "#,
        )
        .unwrap();
        assert!(c.whitespace.strip_trailing);
        assert!(!c.whitespace.final_newline);
    }

    #[test]
    fn whitespace_both_disabled() {
        let c = Config::parse(
            r#"
            [whitespace]
            strip-trailing = false
            final-newline = false
        "#,
        )
        .unwrap();
        assert!(!c.whitespace.any_enabled());
    }

    #[test]
    fn hook_check_mode() {
        let c = Config::parse(
            r#"
            [hook]
            mode = "check"
        "#,
        )
        .unwrap();
        assert_eq!(c.hook.mode, HookMode::Check);
    }

    #[test]
    fn unknown_top_level_field_rejected() {
        let err = Config::parse(
            r#"
            unknown_field = true
        "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn unknown_section_field_rejected() {
        let err = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"
            bogus = 1
        "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknown") || msg.contains("bogus"),
            "got: {msg}"
        );
    }

    #[test]
    fn invalid_ktfmt_style_rejected() {
        assert!(Config::parse(
            r#"
            [ktfmt]
            version = "0.56"
            style = "wat"
        "#
        )
        .is_err());
    }

    #[test]
    fn ktfmt_missing_version_and_path_rejected() {
        let err = Config::parse(
            r#"
            [ktfmt]
            style = "google"
        "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ktfmt") && msg.contains("version"),
            "got: {msg}"
        );
    }

    #[test]
    fn ktfmt_with_both_version_and_path_rejected() {
        let err = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"
            path = "config/bin/ktfmt.jar"
        "#,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ktfmt") && msg.contains("pick one"),
            "got: {msg}"
        );
    }

    #[test]
    fn gjf_with_both_version_and_path_rejected() {
        let err = Config::parse(
            r#"
            [gjf]
            version = "1.28.0"
            path = "config/bin/gjf.jar"
        "#,
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("gjf"));
    }

    #[test]
    fn ktfmt_path_only_is_valid() {
        let c = Config::parse(
            r#"
            [ktfmt]
            path = "config/bin/ktfmt.jar"
        "#,
        )
        .unwrap();
        let kt = c.ktfmt.unwrap();
        assert!(kt.version.is_none());
        assert_eq!(kt.path, Some(PathBuf::from("config/bin/ktfmt.jar")));
    }

    #[test]
    fn ktfmt_source_relative_path_resolves_against_repo_root() {
        let c = Config::parse(
            r#"
            [ktfmt]
            path = "config/bin/ktfmt.jar"
        "#,
        )
        .unwrap();
        let src = c.ktfmt.as_ref().unwrap().source(Path::new("/repo"));
        assert_eq!(
            src,
            ToolSource::Local(PathBuf::from("/repo/config/bin/ktfmt.jar"))
        );
    }

    #[test]
    fn ktfmt_source_absolute_path_used_as_is() {
        let c = Config::parse(
            r#"
            [ktfmt]
            path = "/usr/local/share/ktfmt.jar"
        "#,
        )
        .unwrap();
        let src = c.ktfmt.as_ref().unwrap().source(Path::new("/repo"));
        assert_eq!(
            src,
            ToolSource::Local(PathBuf::from("/usr/local/share/ktfmt.jar"))
        );
    }

    #[test]
    fn ktfmt_source_version_returns_cached() {
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.56"
        "#,
        )
        .unwrap();
        let src = c.ktfmt.as_ref().unwrap().source(Path::new("/repo"));
        assert_eq!(src, ToolSource::Cached("0.56".into()));
    }

    #[test]
    fn gjf_source_relative_path_resolves_against_repo_root() {
        let c = Config::parse(
            r#"
            [gjf]
            path = "tools/gjf.jar"
        "#,
        )
        .unwrap();
        let src = c.gjf.as_ref().unwrap().source(Path::new("/r"));
        assert_eq!(src, ToolSource::Local(PathBuf::from("/r/tools/gjf.jar")));
    }

    #[test]
    fn load_from_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".kempt.toml");
        std::fs::write(
            &path,
            r#"
            [ktfmt]
            version = "0.56"
        "#,
        )
        .unwrap();
        let c = Config::load(&path).unwrap();
        assert_eq!(
            c.ktfmt.unwrap().version.as_ref().unwrap().as_literal(),
            "0.56"
        );
    }

    // --- catalog refs ---

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn version_accepts_catalog_ref_table_with_explicit_key() {
        let c = Config::parse(
            r#"
            [ktfmt]
            version = { file = "gradle/libs.versions.toml", key = "ktfmt-cli" }
        "#,
        )
        .unwrap();
        let kt = c.ktfmt.unwrap();
        match kt.version.as_ref().unwrap() {
            VersionSpec::Ref(r) => {
                assert_eq!(r.file, PathBuf::from("gradle/libs.versions.toml"));
                assert_eq!(r.key.as_deref(), Some("ktfmt-cli"));
            }
            other => panic!("expected Ref, got {other:?}"),
        }
    }

    #[test]
    fn version_catalog_ref_key_is_optional() {
        // Default key is the tool name when omitted.
        let c = Config::parse(
            r#"
            [gjf]
            version = { file = "gradle/libs.versions.toml" }
        "#,
        )
        .unwrap();
        let g = c.gjf.unwrap();
        match g.version.as_ref().unwrap() {
            VersionSpec::Ref(r) => assert!(r.key.is_none()),
            _ => panic!(),
        }
    }

    #[test]
    fn resolve_catalogs_replaces_ref_with_literal() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "gradle/libs.versions.toml",
            "[versions]\nktfmt = \"0.62\"\ngjf = \"1.35.0\"\n",
        );
        let c = Config::parse(
            r#"
            [ktfmt]
            version = { file = "gradle/libs.versions.toml" }

            [gjf]
            version = { file = "gradle/libs.versions.toml" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap();
        assert_eq!(c.ktfmt.unwrap().version.unwrap().as_literal(), "0.62");
        assert_eq!(c.gjf.unwrap().version.unwrap().as_literal(), "1.35.0");
    }

    #[test]
    fn resolve_catalogs_uses_explicit_key() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "catalog.toml",
            "[versions]\nformatter-kotlin = \"0.62\"\n",
        );
        let c = Config::parse(
            r#"
            [ktfmt]
            version = { file = "catalog.toml", key = "formatter-kotlin" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap();
        assert_eq!(c.ktfmt.unwrap().version.unwrap().as_literal(), "0.62");
    }

    #[test]
    fn resolve_catalogs_errors_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let err = Config::parse(
            r#"
            [ktfmt]
            version = { file = "missing.toml" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("missing.toml"), "got: {msg}");
    }

    #[test]
    fn resolve_catalogs_errors_when_versions_table_absent() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "catalog.toml",
            "[libraries]\nktfmt = { module = \"x\" }\n",
        );
        let err = Config::parse(
            r#"
            [ktfmt]
            version = { file = "catalog.toml" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap_err();
        assert!(format!("{err:#}").contains("[versions]"));
    }

    #[test]
    fn resolve_catalogs_errors_when_key_missing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "catalog.toml", "[versions]\nfoo = \"1.0\"\n");
        let err = Config::parse(
            r#"
            [ktfmt]
            version = { file = "catalog.toml" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("ktfmt"), "expected key name in error: {msg}");
    }

    #[test]
    fn resolve_catalogs_errors_clearly_for_rich_version() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "catalog.toml",
            "[versions]\nktfmt = { strictly = \"0.62\" }\n",
        );
        let err = Config::parse(
            r#"
            [ktfmt]
            version = { file = "catalog.toml" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap_err();
        assert!(format!("{err:#}").contains("rich"));
    }

    #[test]
    fn resolve_catalogs_caches_per_file() {
        // Same file referenced twice shouldn't fail when one access succeeds
        // and the second reaches the cached parse.
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "catalog.toml",
            "[versions]\nktfmt = \"0.62\"\ngjf = \"1.35.0\"\n",
        );
        Config::parse(
            r#"
            [ktfmt]
            version = { file = "catalog.toml" }

            [gjf]
            version = { file = "catalog.toml" }
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap();
    }

    #[test]
    fn resolve_catalogs_passes_through_literal_versions() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::parse(
            r#"
            [ktfmt]
            version = "0.62"
        "#,
        )
        .unwrap()
        .resolve_catalogs(dir.path())
        .unwrap();
        assert_eq!(c.ktfmt.unwrap().version.unwrap().as_literal(), "0.62");
    }
}
