//! User settings, loaded from a JSON file. Every field is optional; missing
//! fields fall back to the defaults below (see `settings.example.json`).

use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

/// An external command and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LspSettings {
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
}

/// Colours are `#rrggbb` strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    pub background: String,
    pub plain: String,
    pub comment: String,
    pub keyword: String,
    pub r#type: String,
    pub builtin: String,
    pub ivar: String,
    pub function: String,
    pub punct: String,
    pub string: String,
    pub error: String,
    pub warning: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HighlightSettings {
    /// When false the compiled panel is a plain, selectable text box.
    pub enabled: bool,
    pub theme: Theme,
}

/// Importing pairs from a GitHub repository.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GithubSettings {
    /// The git executable used to clone.
    pub git: String,
    /// Files longer than this are split into hunks of at most this many lines
    /// (a single member longer than this stays whole).
    pub max_lines: usize,
    /// Directory names never descended into.
    pub exclude_dirs: Vec<String>,
    /// Larger files are skipped.
    pub max_file_bytes: u64,
    /// Check that every hunk of a split file is valid Ruby, falling back to bigger hunks (and
    /// finally skipping the file) when it isn't. Uses one warm `ruby` process per thread.
    pub check_syntax: bool,
    /// The `ruby` executable used for that check.
    pub ruby: String,
}

impl Default for GithubSettings {
    fn default() -> Self {
        Self {
            git: "git".into(),
            max_lines: 120,
            exclude_dirs: [".git", "vendor", "node_modules", "tmp"].map(String::from).to_vec(),
            max_file_bytes: 1_000_000,
            check_syntax: true,
            ruby: "ruby".into(),
        }
    }
}

/// The `ruby/gem_rbs_collection` signature repository, used as an import source.
/// The collection holds only `.rbs`; the Ruby comes from the gems themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CollectionSettings {
    /// Repository to sync.
    pub url: String,
    /// Where the clone and unpacked gems are cached. Empty means `~/.cache/synthentic-sample`.
    pub cache_dir: String,
    /// The `gem` executable used to download and unpack gem sources.
    pub gem: String,
    /// Fetch the latest signatures before every collection import.
    pub auto_sync: bool,
    /// How many gems are downloaded and turned into pairs at once.
    pub jobs: usize,
    /// Directory names skipped inside an unpacked gem.
    pub exclude_dirs: Vec<String>,
}

impl Default for CollectionSettings {
    fn default() -> Self {
        Self {
            url: "https://github.com/ruby/gem_rbs_collection.git".into(),
            cache_dir: String::new(),
            gem: "gem".into(),
            auto_sync: true,
            jobs: 4,
            exclude_dirs: ["test", "spec", "vendor", "node_modules", ".git", "tmp", "bin", "examples", "features"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// What the scenarios do. See `crate::scenario`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScenarioSettings {
    /// The scenario selected when the app starts.
    pub default: String,
    /// `gem-rbs-collection`: only these gems (empty means every gem in the collection).
    pub gems: Vec<String>,
    /// `synthetic`: how many pairs to generate, starting at `synthetic_seed`.
    pub synthetic_seed: u64,
    pub synthetic_count: u64,
}

impl Default for ScenarioSettings {
    fn default() -> Self {
        Self { default: "gem-rbs-collection".into(), gems: Vec::new(), synthetic_seed: 0, synthetic_count: 100 }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    pub scenario: ScenarioSettings,
    pub collection: CollectionSettings,
    pub github: GithubSettings,
    /// The transpiler run on each pair's output (`sentinel init`).
    pub sentinel: SentinelSettings,
    /// The language server queried for diagnostics (`sentinel lsp`).
    pub lsp: LspSettings,
    pub highlight: HighlightSettings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SentinelSettings {
    /// The command run for `sentinel init` (the fallback compile path), and for
    /// `sentinel lsp` unless `lsp.command` says otherwise. A scenario replaces it with
    /// the binary it resolved from `source`.
    pub command: String,
    pub args: Vec<String>,
    /// Where a scenario gets the sentinel it tests.
    pub source: SentinelSource,
    /// For `source = "path"`: the binary to use.
    pub path: String,
    /// The gem fetched for `source = "rubygems"`.
    pub gem_name: String,
    /// For `source = "git"`: the repository and ref (branch, tag) built with `cargo`.
    pub git_url: String,
    pub git_ref: String,
    pub cargo: String,
}

/// Where a scenario gets its sentinel binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SentinelSource {
    /// Whatever `sentinel.command` resolves to on this machine.
    Installed,
    /// The latest published gem: its platform binary is extracted from the `.gem`.
    Rubygems,
    /// Clone `git_url` at `git_ref` and `cargo build --release` it: tests unreleased code.
    Git,
    /// An explicit binary, `sentinel.path`.
    Path,
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

impl SentinelSettings {
    pub fn tool(&self) -> Tool {
        Tool { command: self.command.clone(), args: self.args.clone() }
    }
}

impl LspSettings {
    pub fn tool(&self) -> Tool {
        Tool { command: self.command.clone(), args: self.args.clone() }
    }
}

impl Default for SentinelSettings {
    fn default() -> Self {
        Self {
            command: "sentinel".into(),
            args: args(&["init"]),
            source: SentinelSource::Rubygems,
            path: String::new(),
            gem_name: "rbs-sentinel".into(),
            git_url: "https://github.com/AndyGauge/rbs-sentinel.git".into(),
            git_ref: "master".into(),
            cargo: "cargo".into(),
        }
    }
}

impl Default for LspSettings {
    fn default() -> Self {
        Self { enabled: true, command: "sentinel".into(), args: args(&["lsp"]) }
    }
}

impl Default for HighlightSettings {
    fn default() -> Self {
        Self { enabled: true, theme: Theme::default() }
    }
}

impl Default for Theme {
    fn default() -> Self {
        let s = |c: &str| c.to_string();
        Self {
            background: s("#1e2127"),
            plain: s("#abb2bf"),
            comment: s("#5c6370"),
            keyword: s("#c678dd"),
            r#type: s("#e5c07b"),
            builtin: s("#56b6c2"),
            ivar: s("#e06c75"),
            function: s("#61afef"),
            punct: s("#7f848e"),
            string: s("#98c379"),
            error: s("#ff6b6b"),
            warning: s("#e5c07b"),
        }
    }
}

impl Settings {
    /// Loads `path`; a missing file means defaults. `SENTINEL_BIN`, if set,
    /// overrides the command for both the transpiler and the language server.
    pub fn load(path: &Path) -> io::Result<Self> {
        let mut s: Settings = match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e),
        };
        if let Ok(bin) = std::env::var("SENTINEL_BIN") {
            s.sentinel.command = bin.clone();
            s.lsp.command = bin;
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_keeps_defaults() {
        let s: Settings = serde_json::from_str(
            r##"{"sentinel": {"command": "/opt/sentinel"}, "highlight": {"theme": {"keyword": "#ff0000"}}}"##,
        )
        .unwrap();
        assert_eq!(s.sentinel.command, "/opt/sentinel");
        assert_eq!(s.sentinel.args, ["init"]);
        assert_eq!(s.lsp, LspSettings::default());
        assert_eq!(s.highlight.theme.keyword, "#ff0000");
        assert_eq!(s.highlight.theme.comment, Theme::default().comment);
    }

    #[test]
    fn example_file_matches_defaults() {
        let text = include_str!("../settings.example.json");
        assert_eq!(serde_json::from_str::<Settings>(text).unwrap(), Settings::default());
    }
}
