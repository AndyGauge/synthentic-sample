//! RBS syntax highlighting. LSP gives us diagnostics, not tokens, so this is a
//! small hand-written lexer; colours come from [`Theme`](crate::settings::Theme).

use crate::settings::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Comment,
    Keyword,
    Type,
    Builtin,
    Ivar,
    Function,
    Punct,
    String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub kind: Kind,
    pub text: String,
}

const KEYWORDS: &[&str] = &[
    "class", "module", "interface", "def", "end", "type", "attr_reader", "attr_writer",
    "attr_accessor", "include", "extend", "prepend", "alias", "public", "private", "self",
];
const BUILTINS: &[&str] = &["void", "untyped", "nil", "bool", "true", "false", "top", "bot", "instance", "singleton"];

pub type Rgb = (u8, u8, u8);

/// A theme with its colours parsed. Unparseable colours fall back to `plain`.
#[derive(Clone, Debug)]
pub struct Highlighter {
    pub background: Rgb,
    pub error: Rgb,
    pub warning: Rgb,
    colors: [Rgb; 9],
}

pub fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    (h.len() == 6).then_some(())?;
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

impl Highlighter {
    pub fn new(t: &Theme) -> Self {
        let d = Theme::default();
        let plain = parse_hex(&t.plain).or(parse_hex(&d.plain)).unwrap();
        let c = |s: &str| parse_hex(s).unwrap_or(plain);
        Self {
            background: parse_hex(&t.background).unwrap_or((0x1e, 0x21, 0x27)),
            error: c(&t.error),
            warning: c(&t.warning),
            colors: [
                plain, c(&t.comment), c(&t.keyword), c(&t.r#type), c(&t.builtin),
                c(&t.ivar), c(&t.function), c(&t.punct), c(&t.string),
            ],
        }
    }

    pub fn color(&self, k: Kind) -> Rgb {
        self.colors[k as usize]
    }

    /// Splits `src` into lines of coloured spans. Concatenating a line's span
    /// texts reproduces the line exactly.
    pub fn lines(&self, src: &str) -> Vec<Vec<Span>> {
        src.lines().map(tokenize_line).collect()
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}
fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn tokenize_line(line: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut push = |kind, text: &str| {
        if let Some(last) = out.last_mut().filter(|l| l.kind == kind && kind != Kind::Keyword) {
            last.text.push_str(text);
        } else {
            out.push(Span { kind, text: text.to_string() });
        }
    };

    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut i = 0;
    let mut after_def = false; // the next identifier is a method name
    let at = |i: usize| chars.get(i).map(|&(_, c)| c);
    let slice = |a: usize, b: usize| {
        let s = chars[a].0;
        let e = chars.get(b).map_or(line.len(), |&(p, _)| p);
        &line[s..e]
    };

    while i < chars.len() {
        let c = chars[i].1;
        let start = i;
        if c.is_whitespace() {
            while at(i).is_some_and(char::is_whitespace) {
                i += 1;
            }
            push(Kind::Plain, slice(start, i));
        } else if c == '#' {
            i = chars.len();
            push(Kind::Comment, slice(start, i));
        } else if c == '"' {
            i += 1;
            while let Some(ch) = at(i) {
                i += 1;
                if ch == '\\' {
                    i += 1;
                } else if ch == '"' {
                    break;
                }
            }
            i = i.min(chars.len());
            push(Kind::String, slice(start, i));
        } else if c == '@' {
            i += 1;
            while at(i).is_some_and(|c| is_ident(c) || c == '@') {
                i += 1;
            }
            push(Kind::Ivar, slice(start, i));
        } else if is_ident_start(c) {
            while at(i).is_some_and(is_ident) {
                i += 1;
            }
            // Only method names swallow a trailing `?`/`!` (`valid?`); elsewhere `?` is the optional operator.
            if after_def && at(i).is_some_and(|c| c == '?' || c == '!') {
                i += 1;
            }
            let word = slice(start, i);
            let kind = if after_def {
                if word == "self" && at(i) == Some('.') {
                    Kind::Keyword // `def self.name`: still waiting for the name
                } else {
                    after_def = false;
                    Kind::Function
                }
            } else if KEYWORDS.contains(&word) {
                after_def = word == "def";
                Kind::Keyword
            } else if BUILTINS.contains(&word) {
                Kind::Builtin
            } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                Kind::Type
            } else {
                Kind::Plain
            };
            push(kind, word);
        } else if c == ':' && at(i + 1) == Some(':') {
            i += 2;
            push(Kind::Punct, slice(start, i));
        } else if c == '-' && at(i + 1) == Some('>') {
            i += 2;
            push(Kind::Punct, slice(start, i));
        } else {
            i += 1;
            push(Kind::Punct, slice(start, i));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str) -> Vec<(Kind, String)> {
        tokenize_line(line).into_iter().filter(|s| s.kind != Kind::Plain || !s.text.trim().is_empty()).map(|s| (s.kind, s.text)).collect()
    }

    #[test]
    fn def_line() {
        use Kind::*;
        assert_eq!(
            kinds("  def tagged: (Integer x) -> String?"),
            [
                (Keyword, "def".into()), (Function, "tagged".into()), (Punct, ":".into()), (Punct, "(".into()),
                (Type, "Integer".into()), (Plain, " x".into()), (Punct, ")".into()),
                (Punct, "->".into()), (Type, "String".into()), (Punct, "?".into()),
            ]
        );
    }

    #[test]
    fn singleton_def_attr_ivar_comment() {
        use Kind::*;
        assert_eq!(kinds("  def self.cm: () -> void")[..3], [(Keyword, "def".into()), (Keyword, "self".into()), (Punct, ".".into())]);
        assert_eq!(kinds("  def self.cm: () -> void")[3], (Function, "cm".into()));
        assert_eq!(kinds("  attr_reader a: Hash[Symbol, untyped]")[1], (Plain, " a".into()));
        assert_eq!(kinds("  @c: String")[0], (Ivar, "@c".into()));
        assert_eq!(kinds("# Generated by Sentinel"), [(Comment, "# Generated by Sentinel".into())]);
        assert_eq!(kinds("  def valid?: () -> bool")[1], (Function, "valid?".into()));
    }

    #[test]
    fn lossless() {
        for line in ["", "  def f: (?Integer, k: ::Foo::Bar) -> \"s\\\"x\" | nil", "    # c", "def é: () -> void"] {
            let joined: String = tokenize_line(line).iter().map(|s| s.text.as_str()).collect();
            assert_eq!(joined, line);
        }
    }

    #[test]
    fn colors_parse() {
        assert_eq!(parse_hex("#ff8000"), Some((255, 128, 0)));
        assert_eq!(parse_hex("red"), None);
    }
}
