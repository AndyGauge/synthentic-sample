//! Ruby → Ruby-with-inline-RBS pairs.
//!
//! A random class model is rendered twice: bare (input) and annotated (output).
//! Because both come from one model the diff is *only* the annotations, which is
//! the signal we want the model to learn. Each sample uses one of two styles:
//!
//! * `#:` — method-type comments: `#: (String, ?Integer) -> void`
//! * `@rbs` — doc-style: `# @rbs name: String` / `# @rbs return: void`
//!
//! Attributes use a trailing `#: Type`, which also declares the instance variable.

use crate::{
    Pair, PairFactory,
    lsp::LspClient,
    factories::{rbs_reverse, ruby_hunks::split_hunks},
    rbs::RbsIndex,
    pair::{Compiled, Diagnostic, ImportOptions, SourceFile},
    rng::{self, Rng},
    settings::{LspSettings, SentinelSettings},
};
use std::sync::Mutex;

/// Compiles through one `sentinel lsp` process (`sentinel/transpile`, in memory).
/// Against a sentinel that predates that request it falls back to running
/// `sentinel init` in a temp dir, plus the LSP's disk-based diagnostics.
#[derive(Default)]
pub struct RubyRbsFactory {
    sentinel: SentinelSettings,
    lsp: LspSettings,
    state: Mutex<Lsp>,
}

#[derive(Default)]
struct Lsp {
    client: Option<LspClient>,
    /// Set once the server has told us it lacks `sentinel/transpile`.
    legacy: bool,
    /// How the last compile was done, for `compile_mode`.
    mode: &'static str,
}

const MODE_MEMORY: &str = "in memory via sentinel lsp (sentinel/transpile)";
const MODE_OLD_SENTINEL: &str = "fallback: sentinel init in a temp dir (this sentinel lacks sentinel/transpile)";
const MODE_NO_LSP: &str = "fallback: sentinel init in a temp dir (sentinel lsp unavailable)";
const MODE_LSP_OFF: &str = "sentinel init in a temp dir (lsp disabled in settings)";

impl RubyRbsFactory {
    pub fn new(sentinel: SentinelSettings, lsp: LspSettings) -> Self {
        Self { sentinel, lsp, state: Mutex::new(Lsp::default()) }
    }

    /// Runs `f` on the (lazily started) LSP client; a failed call drops the
    /// client so the next one restarts it.
    fn with_client<T>(&self, f: impl FnOnce(&mut LspClient) -> Result<T, String>) -> Result<T, String> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.client.is_none() {
            st.client = Some(LspClient::start(&self.lsp.tool())?);
        }
        let result = f(st.client.as_mut().unwrap());
        if result.is_err() {
            st.client = None;
        }
        result
    }

    fn compile_legacy(&self, source: &str) -> Result<Compiled, String> {
        let output = super::sentinel::transpile(&self.sentinel.tool(), source)?;
        let diagnostics = if self.lsp.enabled {
            self.with_client(|c| c.diagnose(source)).unwrap_or_else(|e| {
                vec![Diagnostic { line: 1, severity: "error".into(), message: e }]
            })
        } else {
            Vec::new()
        };
        Ok(Compiled { output, diagnostics })
    }
}

impl PairFactory for RubyRbsFactory {
    fn id(&self) -> &'static str {
        "ruby-rbs"
    }

    fn description(&self) -> &'static str {
        "Add inline RBS comments (#: and @rbs) to a plain Ruby class"
    }

    fn generate(&self, seed: u64) -> Pair {
        let mut rng = Rng::new(seed);
        let style = if rng.chance(50) { Style::TypeComment } else { Style::RbsTag };
        let class = Class::random(&mut rng);
        let instruction = instruction(&mut rng, style);
        Pair {
            id: Pair::make_id(self.id(), seed),
            factory: self.id().into(),
            seed,
            instruction,
            input: class.render(None),
            output: class.render(Some(style)),
            expected: String::new(),
            compiled: String::new(),
            compile_error: None,
            diagnostics: Vec::new(),
        }
    }

    fn compile_mode(&self) -> String {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).mode.to_string()
    }

    fn import_extensions(&self) -> &'static [&'static str] {
        &[".rb", ".rbs"]
    }

    /// "Reverse compiles" the repo's `.rbs` files into inline annotations: each
    /// Ruby file (or hunk) that has matching signatures becomes a pair whose input
    /// is the plain Ruby and whose output is the same Ruby with inline RBS.
    /// Ruby files with no matching signatures have no ground truth and are skipped.
    fn import_files(&self, label: &str, files: &[SourceFile], opts: &ImportOptions) -> Vec<Pair> {
        let mut index = RbsIndex::default();
        for f in files.iter().filter(|f| f.path.ends_with(".rbs")) {
            index.add_file(&f.text);
        }
        let mut pairs = Vec::new();
        for f in files.iter().filter(|f| f.path.ends_with(".rb")) {
            let hunks = split_hunks(&f.text, opts.max_lines);
            for (n, hunk) in hunks.iter().enumerate() {
                let annotated = rbs_reverse::annotate(hunk, &index);
                if annotated.count == 0 {
                    continue;
                }
                let suffix = if hunks.len() > 1 { format!("#{}", n + 1) } else { String::new() };
                let id = format!("{}:{label}:{}{suffix}", self.id(), f.path);
                let instruction = instruction(&mut Rng::new(rng::hash64(&id)), Style::TypeComment);
                pairs.push(Pair {
                    id,
                    factory: self.id().into(),
                    seed: 0,
                    instruction,
                    input: hunk.clone(),
                    output: annotated.text,
                    expected: annotated.expected,
                    compiled: String::new(),
                    compile_error: None,
                    diagnostics: Vec::new(),
                });
            }
        }
        pairs
    }

    fn compile(&self, source: &str) -> Result<Compiled, String> {
        let set_mode = |mode: &'static str| self.state.lock().unwrap_or_else(|e| e.into_inner()).mode = mode;
        let legacy = self.state.lock().unwrap_or_else(|e| e.into_inner()).legacy;
        if !self.lsp.enabled {
            set_mode(MODE_LSP_OFF);
        } else if legacy {
            set_mode(MODE_OLD_SENTINEL);
        } else {
            match self.with_client(|c| c.transpile(source)) {
                Ok(c) => {
                    set_mode(MODE_MEMORY);
                    return Ok(c);
                }
                Err(e) if e.contains("-32601") => {
                    self.state.lock().unwrap_or_else(|e| e.into_inner()).legacy = true;
                    set_mode(MODE_OLD_SENTINEL);
                }
                // Couldn't start or talk to the server: `sentinel init` still works.
                Err(_) => set_mode(MODE_NO_LSP),
            }
        }
        self.compile_legacy(source)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Style {
    TypeComment,
    RbsTag,
}

fn instruction(rng: &mut Rng, style: Style) -> String {
    let how = match style {
        Style::TypeComment => "`#:` method type comments",
        Style::RbsTag => "`# @rbs` tag comments",
    };
    let templates = [
        format!("Add inline RBS annotations to this Ruby class using {how}."),
        format!("Annotate the following Ruby code with inline RBS ({how}). Do not change any behaviour."),
        format!("Add inline RBS type comments using {how}, keeping the code itself unchanged."),
        format!("This Ruby file has no types. Write inline RBS comments for it using {how}."),
    ];
    rng.pick(&templates).clone()
}

// ---- vocabulary -----------------------------------------------------------

/// (attribute name, RBS type, ruby literal of that type)
const ATTRS: &[(&str, &str, &str)] = &[
    ("name", "String", "\"anon\""),
    ("title", "String", "\"untitled\""),
    ("email", "String", "\"a@example.com\""),
    ("slug", "String", "\"item\""),
    ("count", "Integer", "0"),
    ("age", "Integer", "18"),
    ("quantity", "Integer", "1"),
    ("priority", "Integer", "3"),
    ("price", "Float", "9.5"),
    ("ratio", "Float", "0.5"),
    ("active", "bool", "true"),
    ("archived", "bool", "false"),
    ("status", "Symbol", ":pending"),
    ("kind", "Symbol", ":basic"),
    ("tags", "Array[String]", "[]"),
    ("scores", "Array[Integer]", "[]"),
    ("labels", "Hash[Symbol, String]", "{}"),
    ("totals", "Hash[String, Integer]", "{}"),
    ("nickname", "String?", "nil"),
    ("parent_id", "Integer?", "nil"),
    ("amount", "Integer | Float", "0"),
    ("ref", "String | Symbol", ":none"),
];

const CLASSES: &[&str] = &[
    "Account", "Invoice", "Ticket", "Product", "Member", "Order", "Report", "Widget", "Booking",
    "Profile", "Project", "Shipment", "Survey", "Device", "Coupon", "Session",
];

// ---- model ----------------------------------------------------------------

#[derive(Clone)]
struct Attr {
    name: &'static str,
    ty: &'static str,
    lit: &'static str,
}

#[derive(Clone)]
enum ParamKind {
    Required,
    Optional(String),
    Keyword,
    OptionalKeyword(String),
}

#[derive(Clone)]
struct Param {
    name: String,
    ty: String,
    kind: ParamKind,
}

impl Param {
    fn req(name: &str, ty: &str) -> Self {
        Self { name: name.into(), ty: ty.into(), kind: ParamKind::Required }
    }
    fn opt(name: &str, ty: &str, default: &str) -> Self {
        Self { name: name.into(), ty: ty.into(), kind: ParamKind::Optional(default.into()) }
    }
    fn kw_opt(name: &str, ty: &str, default: &str) -> Self {
        Self { name: name.into(), ty: ty.into(), kind: ParamKind::OptionalKeyword(default.into()) }
    }

    /// Ruby source form, e.g. `limit = 10` / `verbose: false`.
    fn ruby(&self) -> String {
        match &self.kind {
            ParamKind::Required => self.name.clone(),
            ParamKind::Optional(d) => format!("{} = {d}", self.name),
            ParamKind::Keyword => format!("{}:", self.name),
            ParamKind::OptionalKeyword(d) => format!("{}: {d}", self.name),
        }
    }

    /// RBS form inside a `#:` method type, e.g. `?Integer` / `verbose: bool`.
    fn rbs(&self) -> String {
        match &self.kind {
            ParamKind::Required => self.ty.clone(),
            ParamKind::Optional(_) => format!("?{}", self.ty),
            ParamKind::Keyword => format!("{}: {}", self.name, self.ty),
            ParamKind::OptionalKeyword(_) => format!("?{}: {}", self.name, self.ty),
        }
    }
}

struct Method {
    name: String,
    params: Vec<Param>,
    ret: String,
    body: Vec<String>,
}

impl Method {
    fn render(&self, out: &mut String, style: Option<Style>) {
        match style {
            Some(Style::TypeComment) => {
                let ps: Vec<_> = self.params.iter().map(Param::rbs).collect();
                out.push_str(&format!("  #: ({}) -> {}\n", ps.join(", "), self.ret));
            }
            Some(Style::RbsTag) => {
                for p in &self.params {
                    out.push_str(&format!("  # @rbs {}: {}\n", p.name, p.ty));
                }
                out.push_str(&format!("  # @rbs return: {}\n", self.ret));
            }
            None => {}
        }
        let sig = if self.params.is_empty() {
            String::new()
        } else {
            let ps: Vec<_> = self.params.iter().map(Param::ruby).collect();
            format!("({})", ps.join(", "))
        };
        out.push_str(&format!("  def {}{sig}\n", self.name));
        for line in &self.body {
            out.push_str(&format!("    {line}\n"));
        }
        out.push_str("  end\n");
    }
}

struct Class {
    name: &'static str,
    attrs: Vec<Attr>,
    methods: Vec<Method>,
}

impl Class {
    fn random(rng: &mut Rng) -> Self {
        let name = *rng.pick(CLASSES);
        let count = rng.range(2, 4);
        let attrs: Vec<Attr> = rng
            .sample(ATTRS, count)
            .into_iter()
            .map(|(name, ty, lit)| Attr { name, ty, lit })
            .collect();

        let mut methods = vec![Self::initialize(rng, &attrs)];
        let extra = rng.range(1, 3);
        let mut makers: Vec<fn(&mut Rng, &Class, &[Attr]) -> Option<Method>> =
            vec![Self::to_s, Self::setter, Self::predicate, Self::describe, Self::lookup];
        for _ in 0..extra {
            if makers.is_empty() {
                break;
            }
            let maker = makers.swap_remove(rng.below(makers.len()));
            let shell = Class { name, attrs: attrs.clone(), methods: vec![] };
            if let Some(m) = maker(rng, &shell, &attrs) {
                methods.push(m);
            }
        }
        Self { name, attrs, methods }
    }

    fn initialize(rng: &mut Rng, attrs: &[Attr]) -> Method {
        let params = attrs
            .iter()
            .map(|a| {
                // Nilable attributes always get a default; others are mixed.
                match (a.ty.ends_with('?'), rng.below(3)) {
                    (true, _) => Param::kw_opt(a.name, a.ty, a.lit),
                    (false, 0) => Param::opt(a.name, a.ty, a.lit),
                    (false, 1) => Param { name: a.name.into(), ty: a.ty.into(), kind: ParamKind::Keyword },
                    _ => Param::req(a.name, a.ty),
                }
            })
            .collect::<Vec<_>>();
        // Ruby requires optional positionals after required ones; keywords go last.
        let mut ordered: Vec<Param> = Vec::new();
        ordered.extend(params.iter().filter(|p| matches!(p.kind, ParamKind::Required)).cloned());
        ordered.extend(params.iter().filter(|p| matches!(p.kind, ParamKind::Optional(_))).cloned());
        ordered.extend(
            params
                .iter()
                .filter(|p| matches!(p.kind, ParamKind::Keyword | ParamKind::OptionalKeyword(_)))
                .cloned(),
        );
        Method {
            name: "initialize".into(),
            body: attrs.iter().map(|a| format!("@{0} = {0}", a.name)).collect(),
            params: ordered,
            ret: "void".into(),
        }
    }

    fn to_s(_: &mut Rng, c: &Class, a: &[Attr]) -> Option<Method> {
        let parts: Vec<_> = a.iter().take(2).map(|x| format!("{}=#{{@{}}}", x.name, x.name)).collect();
        Some(Method {
            name: "to_s".into(),
            params: vec![],
            ret: "String".into(),
            body: vec![format!("\"{} {}\"", c.name.to_lowercase(), parts.join(" "))],
        })
    }

    fn setter(rng: &mut Rng, _: &Class, a: &[Attr]) -> Option<Method> {
        let at = rng.pick(a);
        Some(Method {
            name: format!("update_{}", at.name),
            params: vec![Param::req("value", at.ty)],
            ret: "void".into(),
            body: vec![format!("@{} = value", at.name)],
        })
    }

    fn predicate(rng: &mut Rng, _: &Class, a: &[Attr]) -> Option<Method> {
        let at = a.iter().find(|x| x.ty == "bool").unwrap_or_else(|| rng.pick(a));
        let body = match at.ty {
            "bool" => format!("@{}", at.name),
            "String" => format!("!@{}.empty?", at.name),
            t if t.ends_with('?') => format!("!@{}.nil?", at.name),
            t if t.starts_with("Array") || t.starts_with("Hash") => format!("!@{}.empty?", at.name),
            _ => format!("!@{}.nil?", at.name),
        };
        Some(Method { name: format!("{}?", at.name.trim_end_matches('s')), params: vec![], ret: "bool".into(), body: vec![body] })
    }

    fn describe(rng: &mut Rng, _: &Class, a: &[Attr]) -> Option<Method> {
        let at = rng.pick(a);
        Some(Method {
            name: "describe".into(),
            params: vec![Param::opt("prefix", "String", "\"\""), Param::kw_opt("verbose", "bool", "false")],
            ret: "String".into(),
            body: vec![
                format!("text = \"#{{prefix}}#{{@{}}}\"", at.name),
                "verbose ? \"#{text} (verbose)\" : text".into(),
            ],
        })
    }

    fn lookup(rng: &mut Rng, _: &Class, a: &[Attr]) -> Option<Method> {
        let at = a.iter().find(|x| x.ty.starts_with("Hash"))?;
        let _ = rng;
        let (k, v) = at.ty.strip_prefix("Hash[")?.strip_suffix(']')?.split_once(", ")?;
        Some(Method {
            name: format!("find_{}", at.name.trim_end_matches('s')),
            params: vec![Param::req("key", k)],
            ret: format!("{v}?"),
            body: vec![format!("@{}[key]", at.name)],
        })
    }

    fn render(&self, style: Option<Style>) -> String {
        let mut out = format!("class {}\n", self.name);
        for a in &self.attrs {
            let ann = if style.is_some() { format!(" #: {}", a.ty) } else { String::new() };
            out.push_str(&format!("  attr_reader :{}{ann}\n", a.name));
        }
        out.push('\n');
        for (i, m) in self.methods.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            m.render(&mut out, style);
        }
        out.push_str("end\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let f = RubyRbsFactory::default();
        assert_eq!(f.generate(42), f.generate(42));
        assert_ne!(f.generate(42).output, f.generate(43).output);
    }

    #[test]
    fn output_is_input_plus_comments_only() {
        let f = RubyRbsFactory::default();
        for seed in 0..500 {
            let p = f.generate(seed);
            let stripped: Vec<String> = p
                .output
                .lines()
                .filter(|l| !l.trim_start().starts_with("# @rbs") && !l.trim_start().starts_with("#:"))
                .map(|l| l.split(" #: ").next().unwrap().to_string())
                .collect();
            let input: Vec<String> = p.input.lines().map(String::from).collect();
            // Blank-line separators around the @rbs ivar block may differ; compare non-blank lines.
            let nb = |v: &[String]| v.iter().filter(|l| !l.trim().is_empty()).cloned().collect::<Vec<_>>();
            assert_eq!(nb(&stripped), nb(&input), "seed {seed}\n{}", p.output);
        }
    }

    #[test]
    fn sample() {
        let p = RubyRbsFactory::default().generate(7);
        println!("{}\n---\n{}", p.input, p.output);
    }
}

#[cfg(test)]
mod measure {
    use super::*;
    #[test]
    #[ignore]
    fn sentinel_coverage() {
        // Honours SENTINEL_BIN, like the app does.
        let s = crate::Settings::load(std::path::Path::new("/nonexistent")).unwrap();
        let f = RubyRbsFactory::new(s.sentinel, s.lsp);
        let started = std::time::Instant::now();
        let (mut sigs_in, mut sigs_out, mut attrs_in, mut attrs_out) = (0, 0, 0, 0);
        for seed in 0..100 {
            let p = f.generate(seed);
            let rbs = f.compile(&p.output).map(|c| c.output).unwrap_or_default();
            sigs_in += p.output.matches("  def ").count();
            sigs_out += rbs.matches("  def ").count();
            attrs_in += p.output.matches("attr_reader").count();
            attrs_out += rbs.matches("attr_").count();
        }
        println!("defs {sigs_out}/{sigs_in}  attrs {attrs_out}/{attrs_in}  100 compiles in {:?}", started.elapsed());
    }
}
