//! "Reverse compiling": given a Ruby source and the RBS that describes it, put
//! the RBS back into the Ruby as inline annotations — the inverse of what
//! `sentinel` does.
//!
//! * `def name: SIG`          → `#: SIG` on the line above the `def`
//! * `attr_reader name: T`    → trailing `#: T` on `attr_reader :name`
//! * `@name: T` (no matching attr) → `# @rbs @name: T` at the top of the class
//!
//! Alongside the annotated source, [`annotate`] returns the RBS those
//! annotations are *expected* to compile to, so the result can be verified
//! against what sentinel actually produces (see [`crate::rbs::mismatches`]).
//! Anything not understood (type aliases, `include`, overloaded methods,
//! multi-symbol `attr_*` lines) is left alone.

use crate::rbs::RbsIndex;

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

/// The result of [`annotate`].
#[derive(Debug, PartialEq, Eq)]
pub struct Annotated {
    /// The source with inline annotations added.
    pub text: String,
    /// How many annotations were added (0: nothing in the source matched the RBS).
    pub count: usize,
    /// RBS declaring exactly the annotated members, grouped by fully-qualified class.
    pub expected: String,
}

/// Members to record in `Annotated::expected`, per class, in annotation order.
#[derive(Default)]
struct Expected(Vec<(String, Vec<String>)>);

impl Expected {
    fn add(&mut self, class: &str, decl: String) {
        match self.0.iter_mut().find(|(c, _)| c == class) {
            Some((_, members)) => members.push(decl),
            None => self.0.push((class.to_string(), vec![decl])),
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (class, members) in &self.0 {
            out.push_str(&format!("class {class}\n"));
            for m in members {
                out.push_str(&format!("  {m}\n"));
            }
            out.push_str("end\n");
        }
        out
    }
}

/// Annotates `src` from `index`.
pub fn annotate(src: &str, index: &RbsIndex) -> Annotated {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 8);
    let mut stack: Vec<Scope> = Vec::new();
    let mut expected = Expected::default();
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
                let before = count;
                for (ivar, ty) in &sigs.ivars {
                    // Class variables (`@@x`) are skipped: rbs-inline doesn't read that form.
                    if !ivar.starts_with("@@") && !sigs.attrs.contains_key(ivar.trim_start_matches('@')) {
                        out.push(format!("{}# @rbs {ivar}: {ty}", " ".repeat(member_indent)));
                        expected.add(&qualified, format!("{ivar}: {ty}"));
                        count += 1;
                    }
                }
                // rbs-inline reads a comment directly above a member as that member's
                // documentation and ignores an `@rbs @ivar` in it, so set the block apart.
                if count > before && lines.get(i + 1).is_some_and(|l| !l.trim().is_empty()) {
                    out.push(String::new());
                }
            }
            stack.push(Scope { indent: ind, name: Some(qualified) });
            continue;
        }

        let (class, in_singleton) = context(&stack);
        let sigs = class.as_ref().and_then(|c| index.classes.get(c));

        // `def name` / `def self.name`, possibly behind a visibility modifier.
        let decl = ["private ", "protected ", "public "].iter().find_map(|m| t.strip_prefix(m)).unwrap_or(t);
        if let (Some(def), Some(sigs), Some(class)) = (decl.strip_prefix("def "), sigs, class.as_deref()) {
            let (singleton, def) = match def.strip_prefix("self.") {
                Some(r) => (true, r),
                None => (in_singleton, def),
            };
            let name: String = def.chars().take_while(|c| !matches!(c, '(' | ' ' | '\t' | ';')).collect();
            let already = out.last().is_some_and(|l| l.trim_start().starts_with("#:"));
            if let Some([sig]) = sigs.methods.get(&(singleton, name.clone())).map(Vec::as_slice) {
                if !already {
                    out.push(format!("{}#: {sig}", " ".repeat(ind)));
                    let recv = if singleton { "self." } else { "" };
                    expected.add(class, format!("def {recv}{name}: {sig}"));
                    count += 1;
                }
            }
            out.push(line.to_string());
            continue;
        }

        if let (Some(attr), Some(sigs), Some(class)) = (single_attr(t), sigs, class.as_deref()) {
            if let Some((kind, ty)) = sigs.attrs.get(attr) {
                out.push(format!("{line} #: {ty}"));
                expected.add(class, format!("{kind} {attr}: {ty}"));
                count += 1;
                continue;
            }
        }

        out.push(line.to_string());
    }

    let mut text = out.join("\n");
    text.push('\n');
    Annotated { text, count, expected: expected.render() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbs::mismatches;

    const RBS: &str = "\
module Shop
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
    fn annotates_ruby() {
        let a = annotate(RUBY, &index());
        let want = [
            "    attr_reader :items #: Array[String]",
            "    attr_accessor :owner #: String?",
            "    attr_reader :a, :b\n", // multi-symbol: left alone
            "    # @rbs @count: Integer\n\n",
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
            assert!(a.text.contains(w), "missing {w:?} in:\n{}", a.text);
        }
        // Overloads, unknown methods, and the `@items` ivar (covered by attr) get nothing.
        assert!(!a.text.contains("#: (Integer) -> String\n    def pick"));
        assert!(!a.text.contains("# @rbs @items"));
        assert!(!a.text.contains("#: () -> Integer\n    def untyped_thing"));
        assert_eq!(a.count, 11, "{}", a.text);
    }

    #[test]
    fn expected_rbs_describes_exactly_the_annotated_members() {
        let a = annotate(RUBY, &index());
        assert!(a.expected.starts_with("class Shop::Cart\n"), "{}", a.expected);
        for line in [
            "  attr_reader items: Array[String]",
            "  attr_accessor owner: String?",
            "  @count: Integer",
            "  def initialize: (?Integer) -> void",
            "  def self.build: () -> Cart",
            "  def ==: (untyped) -> bool",
        ] {
            assert!(a.expected.contains(line), "missing {line:?} in:\n{}", a.expected);
        }
        // Not annotated, so not expected.
        assert!(!a.expected.contains("pick") && !a.expected.contains("@items"));
        assert_eq!(a.expected.lines().count(), 1 + a.count + 1);
        // And it is a faithful subset of the original RBS.
        assert_eq!(mismatches(&a.expected, RBS), Vec::<String>::new());
    }

    #[test]
    fn class_variables_are_not_annotated() {
        let mut i = RbsIndex::default();
        i.add_file("class A\n  @@shared: Integer\n  @own: String\n  def f: () -> void\nend\n");
        let a = annotate("class A\n  def f; end\nend\n", &i);
        assert!(a.text.contains("# @rbs @own: String\n\n  #: () -> void") && !a.text.contains("@@shared"), "{}", a.text);
        assert!(!a.expected.contains("@@"), "{}", a.expected);
    }

    #[test]
    fn nothing_matches_means_zero() {
        let a = annotate("class Other\n  def x\n  end\nend\n", &index());
        assert_eq!(a.count, 0);
        assert_eq!(a.text, "class Other\n  def x\n  end\nend\n");
        assert_eq!(a.expected, "");
    }

    #[test]
    fn singleton_class_body_uses_singleton_sigs() {
        let mut i = RbsIndex::default();
        i.add_file("class A\n  def self.make: () -> A\nend\n");
        let a = annotate("class A\n  class << self\n    def make\n    end\n  end\nend\n", &i);
        assert_eq!(a.count, 1, "{}", a.text);
        assert!(a.text.contains("    #: () -> A\n    def make"));
        assert_eq!(a.expected, "class A\n  def self.make: () -> A\nend\n");
    }

    #[test]
    fn annotating_twice_adds_nothing_new_to_methods() {
        let once = annotate(RUBY, &index()).text;
        let twice = annotate(&once, &index()).text;
        assert_eq!(twice.matches("def initialize").count(), 1);
        assert_eq!(twice.matches("#: (?Integer) -> void").count(), 1);
    }
}
