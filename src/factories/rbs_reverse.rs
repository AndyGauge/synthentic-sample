//! "Reverse compiling": given a Ruby source and the RBS that describes it, put
//! the RBS back into the Ruby as inline annotations — the inverse of what
//! `sentinel` does.
//!
//! * `def name: SIG`          → `#: SIG` on the line above the `def`
//! * `attr_reader name: T`    → trailing `#: T` on `attr_reader :name`
//! * `@name: T` (no matching attr) → `# @rbs @name: T` at the top of the class
//!
//! RBS is line-oriented enough that a small statement parser covers real-world
//! `sig/` files; anything it doesn't understand (type aliases, `include`,
//! overloaded methods, multi-symbol `attr_*` lines) is left alone.

use std::collections::HashMap;

#[derive(Default, Debug)]
struct ClassSigs {
    /// (is_singleton, name) → overloads (inline `#:` can only express one).
    methods: HashMap<(bool, String), Vec<String>>,
    /// attribute name → type
    attrs: HashMap<String, String>,
    /// (`@name`, type) in declaration order
    ivars: Vec<(String, String)>,
}

/// Signatures from any number of `.rbs` files, keyed by fully-qualified class name.
#[derive(Default, Debug)]
pub struct RbsIndex {
    classes: HashMap<String, ClassSigs>,
}

fn depth(s: &str) -> i32 {
    let (mut d, mut quoted) = (0, false);
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            '(' | '[' | '{' if !quoted => d += 1,
            ')' | ']' | '}' if !quoted => d -= 1,
            _ => {}
        }
    }
    d
}

/// A statement that must continue on the next line.
fn incomplete(s: &str) -> bool {
    depth(s) > 0 || ["->", "|", ":", ","].iter().any(|e| s.ends_with(e))
}

/// Splits a method type into overloads: a top-level `|` followed by `(` and a
/// later `->` is a separator; any other `|` is a union inside a type.
fn overloads(sig: &str) -> Vec<String> {
    let chars: Vec<char> = sig.chars().collect();
    let (mut parts, mut start, mut d) = (Vec::new(), 0, 0);
    for i in 0..chars.len() {
        match chars[i] {
            '(' | '[' | '{' => d += 1,
            ')' | ']' | '}' => d -= 1,
            '|' if d == 0 => {
                let rest: String = chars[i + 1..].iter().collect();
                let rest = rest.trim_start();
                if rest.starts_with('(') && rest.contains("->") {
                    parts.push(chars[start..i].iter().collect::<String>().trim().to_string());
                    start = i + 1;
                }
            }
            _ => {}
        }
    }
    parts.push(chars[start..].iter().collect::<String>().trim().to_string());
    parts
}

impl RbsIndex {
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Parses one `.rbs` file into the index. Unrecognised statements are skipped.
    pub fn add_file(&mut self, text: &str) {
        let mut scope: Vec<String> = Vec::new();
        let mut open: Option<String> = None; // statement still being continued
        let mut pending: Option<String> = None; // complete, waiting to see if `|` follows

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(stmt) = &mut open {
                stmt.push(' ');
                stmt.push_str(line);
                if !incomplete(stmt) {
                    pending = open.take();
                }
                continue;
            }
            if line.starts_with('|') {
                if let Some(p) = &mut pending {
                    p.push(' ');
                    p.push_str(line);
                    continue;
                }
            }
            if let Some(p) = pending.take() {
                self.dispatch(&mut scope, &p);
            }
            if incomplete(line) {
                open = Some(line.to_string());
            } else {
                pending = Some(line.to_string());
            }
        }
        if let Some(p) = pending.take().or(open) {
            self.dispatch(&mut scope, &p);
        }
    }

    fn dispatch(&mut self, scope: &mut Vec<String>, stmt: &str) {
        let first = stmt.split_whitespace().next().unwrap_or("");
        let rest = stmt[first.len()..].trim();
        match first {
            "class" | "module" | "interface" => {
                let name = rest
                    .split(|c: char| c.is_whitespace() || c == '<' || c == '[')
                    .next()
                    .unwrap_or("");
                let qualified = match (name.strip_prefix("::"), scope.last()) {
                    (Some(abs), _) => abs.to_string(),
                    (None, Some(parent)) => format!("{parent}::{name}"),
                    (None, None) => name.to_string(),
                };
                self.classes.entry(qualified.clone()).or_default();
                scope.push(qualified);
            }
            "end" => {
                scope.pop();
            }
            _ => {
                let Some(class) = scope.last().and_then(|c| self.classes.get_mut(c)) else { return };
                match first {
                    "def" => {
                        let (singleton, rest) = match rest.strip_prefix("self.") {
                            Some(r) => (true, r),
                            None => (false, rest),
                        };
                        if let Some((name, sig)) = rest.split_once(':') {
                            let sig = sig.trim();
                            if !sig.is_empty() && sig != "..." {
                                class.methods.insert((singleton, name.trim().to_string()), overloads(sig));
                            }
                        }
                    }
                    "attr_reader" | "attr_writer" | "attr_accessor" => {
                        if let Some((name, ty)) = rest.split_once(':') {
                            // `name (@ivar): T` — keep just the name; skip `self.name` (class-level).
                            let name = name.split_whitespace().next().unwrap_or("");
                            if !name.starts_with("self.") && !name.is_empty() {
                                class.attrs.insert(name.to_string(), ty.trim().to_string());
                            }
                        }
                    }
                    s if s.starts_with('@') => {
                        if let Some((name, ty)) = stmt.split_once(':') {
                            class.ivars.push((name.trim().to_string(), ty.trim().to_string()));
                        }
                    }
                    _ => {} // type, alias, include, extend, private, ...
                }
            }
        }
    }
}

struct Scope {
    indent: usize,
    /// Fully-qualified name; `None` for a `class << self` body.
    name: Option<String>,
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn closes_scope(t: &str) -> bool {
    t == "end" || t.starts_with("end ") || t.starts_with("end#")
}

/// `attr_reader :name` (exactly one symbol, nothing after it) → `name`.
fn single_attr(t: &str) -> Option<&str> {
    let rest = ["attr_reader ", "attr_writer ", "attr_accessor "].iter().find_map(|k| t.strip_prefix(k))?;
    let name = rest.trim().strip_prefix(':')?;
    (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '?')).then_some(name)
}

/// Annotates `src` from `index`. Returns the new source and the number of
/// annotations added (0 means nothing in `src` matched the RBS).
pub fn annotate(src: &str, index: &RbsIndex) -> (String, usize) {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 8);
    let mut stack: Vec<Scope> = Vec::new();
    let mut count = 0;

    // The class a member currently belongs to, and whether we're inside `class << self`.
    let context = |stack: &[Scope]| -> (Option<String>, bool) {
        let singleton = stack.last().is_some_and(|s| s.name.is_none());
        (stack.iter().rev().find_map(|s| s.name.clone()), singleton)
    };

    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        let ind = indent_of(line);

        if closes_scope(t) && stack.last().is_some_and(|s| s.indent == ind) {
            stack.pop();
            out.push(line.to_string());
            continue;
        }

        if let Some(rest) = t.strip_prefix("class ").or_else(|| t.strip_prefix("module ")) {
            out.push(line.to_string());
            if t.ends_with(" end") || t.contains("; end") {
                continue;
            }
            if rest.trim_start().starts_with("<<") {
                stack.push(Scope { indent: ind, name: None });
                continue;
            }
            let name = rest.split(|c: char| c.is_whitespace() || c == '<' || c == ';').next().unwrap_or("");
            let qualified = match (name.strip_prefix("::"), context(&stack).0) {
                (Some(abs), _) => abs.to_string(),
                (None, Some(parent)) => format!("{parent}::{name}"),
                (None, None) => name.to_string(),
            };
            // Ivars the RBS declares that no attr already covers.
            if let Some(sigs) = index.classes.get(&qualified) {
                let member_indent = lines[i + 1..]
                    .iter()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| indent_of(l))
                    .filter(|&n| n > ind)
                    .unwrap_or(ind + 2);
                for (ivar, ty) in &sigs.ivars {
                    if !sigs.attrs.contains_key(ivar.trim_start_matches('@')) {
                        out.push(format!("{}# @rbs {ivar}: {ty}", " ".repeat(member_indent)));
                        count += 1;
                    }
                }
            }
            stack.push(Scope { indent: ind, name: Some(qualified) });
            continue;
        }

        let (class, in_singleton) = context(&stack);
        let sigs = class.as_ref().and_then(|c| index.classes.get(c));

        // `def name` / `def self.name`, possibly behind a visibility modifier.
        let decl = ["private ", "protected ", "public "].iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t);
        if let (Some(def), Some(sigs)) = (decl.strip_prefix("def "), sigs) {
            let (singleton, def) = match def.strip_prefix("self.") {
                Some(r) => (true, r),
                None => (in_singleton, def),
            };
            let name: String = def.chars().take_while(|c| !matches!(c, '(' | ' ' | '\t' | ';')).collect();
            let already = out.last().is_some_and(|l| l.trim_start().starts_with("#:"));
            if let Some([sig]) = sigs.methods.get(&(singleton, name)).map(Vec::as_slice) {
                if !already {
                    out.push(format!("{}#: {sig}", " ".repeat(ind)));
                    count += 1;
                }
            }
            out.push(line.to_string());
            continue;
        }

        if let (Some(attr), Some(sigs)) = (single_attr(t), sigs) {
            if let Some(ty) = sigs.attrs.get(attr) {
                out.push(format!("{line} #: {ty}"));
                count += 1;
                continue;
            }
        }

        out.push(line.to_string());
    }

    let mut text = out.join("\n");
    text.push('\n');
    (text, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RBS: &str = "\
# a comment
module Shop
  type id = Integer | String

  class Cart < Base
    attr_reader items: Array[String]
    attr_accessor owner (@owner_ref): String?
    @count: Integer
    @items: Array[String]

    def initialize: (?Integer) -> void
    def add: (String item, ?qty: Integer) -> void
    def self.build: () -> Cart
    def total: () -> (Integer | Float)
    def label: () -> String | Symbol
    def each: () { (String) -> void } -> void
    def parse: (String,
                Integer) -> Hash[Symbol, String]
    def pick: (Integer) -> String
            | (String) -> Symbol
    def ==: (untyped) -> bool
  end
end
";

    const RUBY: &str = "\
module Shop
  class Cart < Base
    attr_reader :items
    attr_accessor :owner
    attr_reader :a, :b

    def initialize(count = 0)
      @count = count
    end

    # Adds an item.
    def add(item, qty: 1)
    end

    def self.build
      new
    end

    def total; 0; end

    def label
    end

    def each(&block)
    end

    def parse(a, b)
    end

    def pick(x)
    end

    private def ==(other)
      true
    end

    def untyped_thing
    end

    class << self
      def helper
      end
    end
  end
end
";

    fn index() -> RbsIndex {
        let mut i = RbsIndex::default();
        i.add_file(RBS);
        i
    }

    #[test]
    fn parses_members() {
        let i = index();
        let c = &i.classes["Shop::Cart"];
        assert_eq!(c.attrs["items"], "Array[String]");
        assert_eq!(c.attrs["owner"], "String?");
        assert_eq!(c.methods[&(false, "add".into())], ["(String item, ?qty: Integer) -> void"]);
        assert_eq!(c.methods[&(true, "build".into())], ["() -> Cart"]);
        // Union return types are not overloads; real overloads are.
        assert_eq!(c.methods[&(false, "label".into())].len(), 1);
        assert_eq!(c.methods[&(false, "pick".into())].len(), 2);
        // Multi-line signatures are joined.
        assert_eq!(c.methods[&(false, "parse".into())], ["(String, Integer) -> Hash[Symbol, String]"]);
        assert_eq!(c.ivars, [("@count".into(), "Integer".into()), ("@items".into(), "Array[String]".into())]);
    }

    #[test]
    fn annotates_ruby() {
        let (out, n) = annotate(RUBY, &index());
        let want = [
            "    attr_reader :items #: Array[String]",
            "    attr_accessor :owner #: String?",
            "    attr_reader :a, :b\n", // multi-symbol: left alone
            "    # @rbs @count: Integer\n",
            "    #: (?Integer) -> void\n    def initialize(count = 0)",
            "    # Adds an item.\n    #: (String item, ?qty: Integer) -> void\n    def add(item, qty: 1)",
            "    #: () -> Cart\n    def self.build",
            "    #: () { (String) -> void } -> void\n    def each(&block)",
            "    #: (String, Integer) -> Hash[Symbol, String]\n    def parse(a, b)",
            "    #: (untyped) -> bool\n    private def ==(other)",
            "    #: () -> String | Symbol\n    def label",
            "    #: () -> (Integer | Float)\n    def total; 0; end",
        ];
        for w in want {
            assert!(out.contains(w), "missing {w:?} in:\n{out}");
        }
        // Overloads, unknown methods, and the `@items` ivar (covered by attr) get nothing.
        assert!(!out.contains("def pick") || !out.contains("#: (Integer) -> String\n    def pick"));
        assert!(!out.contains("# @rbs @items"));
        assert!(!out.contains("#: () -> Integer\n    def untyped_thing"));
        assert_eq!(n, 11, "{out}");
    }

    #[test]
    fn nothing_matches_means_zero() {
        let (out, n) = annotate("class Other\n  def x\n  end\nend\n", &index());
        assert_eq!(n, 0);
        assert_eq!(out, "class Other\n  def x\n  end\nend\n");
    }

    #[test]
    fn singleton_class_body_uses_singleton_sigs() {
        let mut i = RbsIndex::default();
        i.add_file("class A\n  def self.make: () -> A\nend\n");
        let (out, n) = annotate("class A\n  class << self\n    def make\n    end\n  end\nend\n", &i);
        assert_eq!(n, 1, "{out}");
        assert!(out.contains("    #: () -> A\n    def make"));
    }

    #[test]
    fn annotating_twice_adds_nothing_new_to_methods() {
        let (once, _) = annotate(RUBY, &index());
        let (twice, _) = annotate(&once, &index());
        assert_eq!(twice.matches("def initialize").count(), 1);
        assert_eq!(twice.matches("#: (?Integer) -> void").count(), 1);
    }
}
