//! A small RBS reader: just enough of the language (classes, modules, `def`,
//! `attr_*`, ivars) to index signatures by class and to compare two RBS texts
//! member by member. Anything else (type aliases, `include`, ...) is ignored.

use std::collections::HashMap;

#[derive(Default, Debug)]
pub struct ClassSigs {
    /// (is_singleton, name) → overloads (inline `#:` can only express one).
    pub methods: HashMap<(bool, String), Vec<String>>,
    /// attribute name → (`attr_reader`/`attr_writer`/`attr_accessor`, type)
    pub attrs: HashMap<String, (String, String)>,
    /// (`@name`, type) in declaration order
    pub ivars: Vec<(String, String)>,
}

/// Signatures from any number of `.rbs` files, keyed by fully-qualified class name.
#[derive(Default, Debug)]
pub struct RbsIndex {
    pub classes: HashMap<String, ClassSigs>,
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
                                class.attrs.insert(name.to_string(), (first.to_string(), ty.trim().to_string()));
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

/// Form of a type for comparing signatures: whitespace is insignificant, and so is
/// a trailing comma before a closing bracket (sentinel wraps long parameter lists
/// one per line with a trailing comma, which is the same type).
fn norm(s: &str) -> String {
    let squashed: String = s.split_whitespace().collect();
    let chars: Vec<char> = squashed.chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|&(i, &c)| !(c == ',' && matches!(chars.get(i + 1), Some(')' | ']' | '}'))))
        .map(|(_, &c)| c)
        .collect()
}

/// Members `expected` declares that `compiled` doesn't declare identically.
/// Extra members in `compiled` are fine. Output is sorted, so it is stable.
pub fn mismatches(expected: &str, compiled: &str) -> Vec<String> {
    let (mut want, mut got) = (RbsIndex::default(), RbsIndex::default());
    want.add_file(expected);
    got.add_file(compiled);

    let mut out = Vec::new();
    let mut classes: Vec<_> = want.classes.iter().collect();
    classes.sort_by(|a, b| a.0.cmp(b.0));
    for (class, w) in classes {
        let g = got.classes.get(class);
        let mut report = |what: String, want: &str, got: Option<&str>| match got {
            None => out.push(format!("{what}: missing from compiled RBS (expected `{want}`)")),
            Some(g) if norm(g) != norm(want) => {
                out.push(format!("{what}: expected `{want}`, compiled `{g}`"))
            }
            Some(_) => {}
        };

        let mut methods: Vec<_> = w.methods.iter().collect();
        methods.sort_by(|a, b| a.0.cmp(b.0));
        for ((singleton, name), sigs) in methods {
            let got = g
                .and_then(|g| g.methods.get(&(*singleton, name.clone())))
                .and_then(|s| s.first())
                .map(String::as_str);
            let sep = if *singleton { "." } else { "#" };
            report(format!("{class}{sep}{name}"), &sigs[0], got);
        }
        let mut attrs: Vec<_> = w.attrs.iter().collect();
        attrs.sort_by(|a, b| a.0.cmp(b.0));
        for (name, (_, ty)) in attrs {
            let got = g.and_then(|g| g.attrs.get(name)).map(|(_, t)| t.as_str());
            report(format!("{class}#{name} (attr)"), ty, got);
        }
        for (name, ty) in &w.ivars {
            let got = g.and_then(|g| g.ivars.iter().find(|(n, _)| n == name)).map(|(_, t)| t.as_str());
            report(format!("{class} {name}"), ty, got);
        }
    }
    out
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

    def add: (String item, ?qty: Integer) -> void
    def self.build: () -> Cart
    def label: () -> String | Symbol
    def parse: (String,
                Integer) -> Hash[Symbol, String]
    def pick: (Integer) -> String
            | (String) -> Symbol
  end
end
";

    #[test]
    fn parses_members() {
        let mut i = RbsIndex::default();
        i.add_file(RBS);
        let c = &i.classes["Shop::Cart"];
        assert_eq!(c.attrs["items"], ("attr_reader".to_string(), "Array[String]".to_string()));
        assert_eq!(c.attrs["owner"].1, "String?");
        assert_eq!(c.methods[&(false, "add".into())], ["(String item, ?qty: Integer) -> void"]);
        assert_eq!(c.methods[&(true, "build".into())], ["() -> Cart"]);
        // Union return types are not overloads; real overloads are.
        assert_eq!(c.methods[&(false, "label".into())].len(), 1);
        assert_eq!(c.methods[&(false, "pick".into())].len(), 2);
        // Multi-line signatures are joined.
        assert_eq!(c.methods[&(false, "parse".into())], ["(String, Integer) -> Hash[Symbol, String]"]);
        assert_eq!(c.ivars, [("@count".into(), "Integer".into())]);
    }

    #[test]
    fn identical_rbs_matches_regardless_of_layout() {
        let compiled = "# Generated\n\nmodule Shop\n  class Cart\n    attr_reader items: Array[String]\n    def add: (String item,\n      ?qty: Integer) -> void\n    def extra: () -> void\n  end\nend\n";
        let expected = "class Shop::Cart\n  attr_reader items: Array[String]\n  def add: (String item, ?qty: Integer) -> void\nend\n";
        assert_eq!(mismatches(expected, compiled), Vec::<String>::new());
    }

    #[test]
    fn wrapped_parameter_lists_with_trailing_commas_match() {
        let expected = "class A\n  def f: (String, ?Integer, k: Hash[Symbol, untyped]) -> void\nend\n";
        let compiled = "class A\n  def f: (\n    String,\n    ?Integer,\n    k: Hash[Symbol, untyped],\n  ) -> void\nend\n";
        assert_eq!(mismatches(expected, compiled), Vec::<String>::new());
        // A genuinely different type still fails.
        let other = compiled.replace("String,", "Symbol,");
        assert_eq!(mismatches(expected, &other).len(), 1);
    }

    #[test]
    fn reports_missing_and_different_members() {
        let expected = "class A\n  attr_reader x: Integer\n  @y: String\n  def f: (Integer) -> void\n  def self.g: () -> A\n  def h: () -> bool\nend\n";
        let compiled = "class A\n  attr_reader x: String\n  def f: (Integer) -> void\n  def h: () -> Integer\nend\n";
        assert_eq!(
            mismatches(expected, compiled),
            [
                "A#h: expected `() -> bool`, compiled `() -> Integer`",
                "A.g: missing from compiled RBS (expected `() -> A`)",
                "A#x (attr): expected `Integer`, compiled `String`",
                "A @y: missing from compiled RBS (expected `String`)",
            ]
        );
        assert_eq!(mismatches(expected, "").len(), 5);
    }
}
