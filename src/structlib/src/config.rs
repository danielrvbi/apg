//! `apg/config.json` structural-scope loading and glob matching.

use std::path::Path;

/// The `apg/config.json` structural scope section: include/exclude globs
/// deciding which files the structural scanner claims, plus the structural
/// `code_type`. It is orthogonal to the existing `types` classification rules
/// (scope decides which files; `types` decides their `code_type`) and defaults
/// to ON — an empty include/exclude claims everything the taxonomy routes.
#[derive(serde::Deserialize, Default)]
pub(crate) struct StructuralScope {
    #[serde(default)]
    pub(crate) include: Vec<String>,
    #[serde(default)]
    pub(crate) exclude: Vec<String>,
    #[serde(default)]
    pub(crate) code_type: Option<String>,
    /// The `json` stream's tree options (`structural.json`).
    #[serde(default)]
    pub(crate) json: JsonConfig,
}

/// One set of `json` tree limits; an absent field inherits.
#[derive(serde::Deserialize, Default, Clone)]
pub(crate) struct JsonLimits {
    #[serde(default)]
    pub(crate) max_depth: Option<u32>,
    #[serde(default)]
    pub(crate) max_items: Option<u32>,
    #[serde(default)]
    pub(crate) max_nodes: Option<u32>,
}

/// A glob-keyed override of the `json` limits for the files it matches
/// (matched against the repo-relative path; first match wins).
#[derive(serde::Deserialize, Default, Clone)]
pub(crate) struct JsonOverride {
    pub(crate) glob: String,
    #[serde(flatten)]
    pub(crate) limits: JsonLimits,
}

/// The `structural.json` section: the global limits plus per-glob overrides.
#[derive(serde::Deserialize, Default, Clone)]
pub(crate) struct JsonConfig {
    #[serde(flatten)]
    pub(crate) limits: JsonLimits,
    #[serde(default)]
    pub(crate) overrides: Vec<JsonOverride>,
}

/// The effective `json` tree limits for one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JsonOptions {
    /// Object nesting levels emitted as nodes (a top-level key is depth 1).
    pub(crate) max_depth: u32,
    /// List elements emitted per list (object lists) or recorded in `items`.
    pub(crate) max_items: u32,
    /// Struct records emitted per file.
    pub(crate) max_nodes: u32,
}

impl Default for JsonOptions {
    fn default() -> Self {
        Self {
            max_depth: 3,
            max_items: 50,
            max_nodes: 2000,
        }
    }
}

impl JsonOptions {
    fn apply(&mut self, l: &JsonLimits) {
        if let Some(v) = l.max_depth {
            self.max_depth = v;
        }
        if let Some(v) = l.max_items {
            self.max_items = v;
        }
        if let Some(v) = l.max_nodes {
            self.max_nodes = v;
        }
    }
}

/// True for the lockfiles forced to depth 1 by default: `package-lock.json`,
/// `*.lock.json` and `composer.lock`.
pub(crate) fn is_json_lockfile(file_name: &str) -> bool {
    let lower = file_name.to_ascii_lowercase();
    lower == "package-lock.json" || lower == "composer.lock" || lower.ends_with(".lock.json")
}

impl JsonConfig {
    /// The effective limits for the file at repo-relative `rel_path`: the
    /// built-in defaults, then the global `structural.json` limits, then the
    /// lockfile depth-1 default, then the first matching override.
    pub(crate) fn resolve(&self, rel_path: &str) -> JsonOptions {
        let mut o = JsonOptions::default();
        o.apply(&self.limits);
        let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
        if is_json_lockfile(name) {
            o.max_depth = 1;
        }
        if let Some(ov) = self
            .overrides
            .iter()
            .find(|ov| glob_match(&ov.glob, rel_path))
        {
            o.apply(&ov.limits);
        }
        o
    }
}

/// The subset of `apg/config.json` this frontend reads (unknown keys, including
/// the whole `types` list, are ignored).
#[derive(serde::Deserialize, Default)]
struct ConfigFile {
    #[serde(default)]
    structural: Option<StructuralScope>,
}

/// Loads the structural scope section from `apg/config.json` at the repository
/// base (falling back to the legacy `apg.json`), defaulting to ON when absent.
pub(crate) fn load_structural_scope(base: &Path) -> StructuralScope {
    for cand in [base.join("apg/config.json"), base.join("apg.json")] {
        let Ok(text) = std::fs::read_to_string(&cand) else {
            continue;
        };
        if let Ok(cfg) = serde_json::from_str::<ConfigFile>(&text) {
            return cfg.structural.unwrap_or_default();
        }
    }
    StructuralScope::default()
}

/// Simple glob matcher: `*` matches any run (including `/`), `?` matches a
/// single character.
pub(crate) fn glob_match(pattern: &str, path: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = path.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut star_ti = 0usize;
    while ti < txt.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi] == txt[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
            while pi < pat.len() && pat[pi] == '*' {
                pi += 1;
            }
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}
