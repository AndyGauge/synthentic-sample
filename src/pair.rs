use serde::{Deserialize, Serialize};

/// A problem the language server reported against a pair's `output`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// 1-based line in `output`.
    pub line: u32,
    /// `error`, `warning`, `info` or `hint`.
    pub severity: String,
    pub message: String,
}

/// One training row: an instruction, the "before" source and the "after" source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    /// `"{factory}:{seed}"`, unique within a store.
    pub id: String,
    pub factory: String,
    pub seed: u64,
    pub instruction: String,
    pub input: String,
    pub output: String,
    /// Output of the factory's compiler run on `output` (empty until compiled).
    #[serde(default)]
    pub compiled: String,
    /// Set when the compiler failed; `compiled` is then empty.
    #[serde(default)]
    pub compile_error: Option<String>,
    /// Language-server diagnostics for `output`; a non-empty list marks a suspect row.
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

/// A file read from a checked-out repository.
#[derive(Clone, Debug)]
pub struct SourceFile {
    /// Path relative to the repo root, `/`-separated.
    pub path: String,
    pub text: String,
}

/// Knobs for turning real source files into pairs.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Files longer than this are split into hunks of at most this many lines.
    pub max_lines: usize,
}

/// What a factory's compiler produced for a source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compiled {
    pub output: String,
    pub diagnostics: Vec<Diagnostic>,
}

impl Pair {
    pub fn make_id(factory: &str, seed: u64) -> String {
        format!("{factory}:{seed}")
    }

    /// Runs `factory`'s compiler on `output` and records the results.
    pub fn recompile(&mut self, factory: &dyn PairFactory) {
        let result = factory.compile(&self.output);
        self.apply_compiled(result);
    }

    /// Records a compile result. Returns whether anything changed.
    pub fn apply_compiled(&mut self, result: Result<Compiled, String>) -> bool {
        let (compiled, diagnostics, error) = match result {
            Ok(c) => (c.output, c.diagnostics, None),
            Err(e) => (String::new(), Vec::new(), Some(e)),
        };
        let changed = self.compiled != compiled || self.diagnostics != diagnostics || self.compile_error != error;
        self.compiled = compiled;
        self.diagnostics = diagnostics;
        self.compile_error = error;
        changed
    }
}

/// Abstract factory for training pairs. Implementations must be pure functions
/// of `seed`: same seed, same pair, on every platform.
pub trait PairFactory: Send + Sync {
    /// Stable identifier, stored in each row.
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn generate(&self, seed: u64) -> Pair;

    /// File extensions (with the dot) this factory reads when importing from a
    /// repository, e.g. `[".rb", ".rbs"]`. Empty means it only generates.
    fn import_extensions(&self) -> &'static [&'static str] {
        &[]
    }

    /// Builds pairs from a whole checkout (`repo` is `owner/repo@sha`): one pair
    /// per file, or per hunk when a file is longer than `opts.max_lines`. Gets
    /// every file so it can relate them (e.g. Ruby sources to their `.rbs`).
    /// Pair ids must be unique and stable for a given repo revision.
    fn import_files(&self, _repo: &str, _files: &[SourceFile], _opts: &ImportOptions) -> Vec<Pair> {
        Vec::new()
    }

    /// Runs the downstream compiler/transpiler over a pair's `output`, returning
    /// what it produces and any diagnostics about the source. Side-effecting
    /// (spawns processes), so it is separate from the pure `generate`.
    fn compile(&self, _source: &str) -> Result<Compiled, String> {
        Err("this factory has no compiler".into())
    }
}
