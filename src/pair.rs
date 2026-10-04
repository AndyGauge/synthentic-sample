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
    /// The RBS `output` is expected to compile to (empty when there is no ground
    /// truth, as for synthetic pairs). Imported pairs get it from the source RBS.
    #[serde(default)]
    pub expected: String,
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

/// Whether a pair's compiled RBS matches what it was expected to compile to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// No ground truth to compare against.
    NotApplicable,
    /// Not compiled yet.
    Pending,
    Match,
    /// What differed, one line per member.
    Mismatch(Vec<String>),
}

impl Pair {
    /// Compares `compiled` against `expected`.
    pub fn check(&self) -> Check {
        if self.expected.is_empty() {
            return Check::NotApplicable;
        }
        if let Some(e) = &self.compile_error {
            return Check::Mismatch(vec![format!("compile failed: {}", e.lines().next().unwrap_or(""))]);
        }
        if self.compiled.is_empty() {
            return Check::Pending;
        }
        match crate::rbs::mismatches(&self.expected, &self.compiled) {
            m if m.is_empty() => Check::Match,
            m => Check::Mismatch(m),
        }
    }

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

    /// Builds pairs from a whole checkout (`label` names it, e.g. `gh:owner/repo@sha`
    /// or `gem:name@1.2.3`): one pair
    /// per file, or per hunk when a file is longer than `opts.max_lines`. Gets
    /// every file so it can relate them (e.g. Ruby sources to their `.rbs`).
    /// Pair ids must be unique and stable for a given revision.
    fn import_files(&self, _label: &str, _files: &[SourceFile], _opts: &ImportOptions) -> Vec<Pair> {
        Vec::new()
    }

    /// A short note on how `compile` is currently being done, for the UI (e.g. which
    /// of several strategies is in use). Empty when there is nothing to say.
    fn compile_mode(&self) -> String {
        String::new()
    }

    /// Runs the downstream compiler/transpiler over a pair's `output`, returning
    /// what it produces and any diagnostics about the source. Side-effecting
    /// (spawns processes), so it is separate from the pure `generate`.
    fn compile(&self, _source: &str) -> Result<Compiled, String> {
        Err("this factory has no compiler".into())
    }
}
