//! Splitting a long Ruby file into standalone hunks.
//!
//! Heuristic and indentation based, not a parser: cut points are blank lines that
//! precede a sibling member of the innermost `class`/`module`. Each hunk after the
//! first is prefixed with its enclosing `class`/`module` header lines and followed
//! by the matching `end`s, so it stands alone (and can be annotated and compiled).
//!
//! Heredoc bodies, `=begin`/`=end` blocks and anything after `__END__` are text, not
//! structure, so they are never mistaken for scopes or cut points. Even so a cut can land
//! somewhere the heuristics don't understand, so [`split_checked`] asks Ruby whether each
//! hunk parses and falls back to bigger hunks, and finally to skipping the file.

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn opens_scope(t: &str) -> bool {
    (t.starts_with("class ") || t.starts_with("module ")) && !t.ends_with(" end") && !t.contains("; end")
}

fn closes_scope(t: &str) -> bool {
    t == "end" || t.starts_with("end ") || t.starts_with("end#")
}

/// `<<~ID`, `<<-ID` and `<<ID` (and quoted forms) opened on `line`, as `(terminator, indented)`.
/// `indented` means the terminator line may be indented (`~` and `-` forms).
fn heredoc_terminators(line: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    if line.trim_start().starts_with('#') {
        return out;
    }
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if &bytes[i..i + 2] != b"<<" {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        let indented = matches!(bytes.get(j), Some(b'~' | b'-'));
        if indented {
            j += 1;
        }
        let id = match bytes.get(j) {
            Some(&q @ (b'\'' | b'"' | b'`')) => {
                line[j + 1..].find(q as char).map(|end| (line[j + 1..j + 1 + end].to_string(), end + 2))
            }
            Some(&b) if b.is_ascii_alphabetic() || b == b'_' => {
                let end = line[j..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(line.len() - j);
                let id = &line[j..j + end];
                // `a << b` and `x <<foo` are not heredocs: a bare id must be an upper-case word.
                (indented || id.starts_with(|c: char| c.is_ascii_uppercase())).then(|| (id.to_string(), end))
            }
            _ => None,
        };
        match id {
            Some((id, len)) if !id.is_empty() => {
                out.push((id, indented));
                i = j + len;
            }
            _ => i += 2,
        }
    }
    out
}

/// Which lines are code. Heredoc bodies (and their terminators), `=begin`/`=end` blocks and
/// everything after `__END__` are not: a `class` or `end` in there is text.
fn code_mask(lines: &[&str]) -> Vec<bool> {
    let mut mask = vec![true; lines.len()];
    let mut pending: Vec<(String, bool)> = Vec::new();
    let mut block_comment = false;
    for (i, l) in lines.iter().enumerate() {
        if block_comment {
            mask[i] = false;
            block_comment = !l.starts_with("=end");
        } else if let Some((term, indented)) = pending.first() {
            mask[i] = false;
            let hit = if *indented { l.trim() == term } else { l.trim_end() == term };
            if hit {
                pending.remove(0);
            }
        } else if l.starts_with("=begin") {
            mask[i] = false;
            block_comment = true;
        } else if *l == "__END__" {
            mask[i..].iter_mut().for_each(|m| *m = false);
            break;
        } else {
            pending.extend(heredoc_terminators(l));
        }
    }
    mask
}

/// Hunks of `src`, each covering at most `max_lines` of the original lines where
/// the structure allows (the re-wrapping headers and `end`s come on top of that).
/// A single member longer than `max_lines` becomes one oversized hunk. Short
/// files (or `max_lines == 0`) come back whole.
pub fn split_hunks(src: &str, max_lines: usize) -> Vec<String> {
    let lines: Vec<&str> = src.lines().collect();
    if max_lines == 0 || lines.len() <= max_lines {
        return vec![src.to_string()];
    }

    let mask = code_mask(&lines);

    // Open scope headers (as line indices) at the start of every line, plus at EOF.
    let mut stacks: Vec<Vec<usize>> = Vec::with_capacity(lines.len() + 1);
    let mut stack: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        stacks.push(stack.clone());
        if !mask[i] {
            continue; // text, not structure
        }
        let t = l.trim();
        if opens_scope(t) {
            stack.push(i);
        } else if closes_scope(t) && stack.last().is_some_and(|&h| indent(lines[h]) == indent(l)) {
            stack.pop();
        }
    }
    stacks.push(stack);

    // Indentation of a scope's members: that of the first non-blank line after its header.
    let member_indent = |h: usize| {
        lines[h + 1..].iter().find(|l| !l.trim().is_empty()).map_or(0, |l| indent(l))
    };
    let candidates: Vec<usize> = (1..lines.len())
        .filter(|&i| {
            let t = lines[i].trim();
            mask[i]
                && lines[i - 1].trim().is_empty()
                && !t.is_empty()
                && !closes_scope(t)
                && indent(lines[i]) == stacks[i].last().map_or(0, |&h| member_indent(h))
        })
        .collect();

    // Greedy: cut at the furthest candidate that keeps the hunk within the limit.
    let mut starts = vec![0];
    let (mut cur, mut prev) = (0, None);
    for &c in &candidates {
        while c - cur > max_lines {
            match prev.take() {
                Some(p) => {
                    starts.push(p);
                    cur = p;
                }
                None => {
                    // One member is longer than the limit: keep it whole.
                    starts.push(c);
                    cur = c;
                }
            }
        }
        if c > cur {
            prev = Some(c);
        }
    }
    // The tail (members plus the closing `end`s) counts too.
    if lines.len() - cur > max_lines {
        if let Some(p) = prev {
            starts.push(p);
        }
    }
    starts.push(lines.len());

    starts
        .windows(2)
        .map(|w| {
            let (a, b) = (w[0], w[1]);
            let mut body = &lines[a..b];
            while body.last().is_some_and(|l| l.trim().is_empty()) {
                body = &body[..body.len() - 1];
            }
            let mut out = String::new();
            for &h in &stacks[a] {
                out.push_str(lines[h]);
                out.push('\n');
            }
            for l in body {
                out.push_str(l);
                out.push('\n');
            }
            for &h in stacks[b].iter().rev() {
                out.push_str(&" ".repeat(indent(lines[h])));
                out.push_str("end\n");
            }
            out
        })
        .collect()
}

/// [`split_hunks`], but every hunk that matters (`relevant`) must also be valid Ruby
/// (`valid`). If a split produces an invalid one the file is split into bigger hunks,
/// and finally not at all; if even the whole file is invalid the error is returned and
/// the file should be skipped. Hunks nobody will use are never checked.
pub fn split_checked(
    src: &str,
    max_lines: usize,
    relevant: &mut dyn FnMut(&str) -> bool,
    valid: &mut dyn FnMut(&str) -> Result<(), String>,
) -> Result<Vec<String>, String> {
    let lines = src.lines().count();
    let mut limits = vec![max_lines];
    if max_lines > 0 {
        limits.extend([2, 4].map(|m| max_lines * m).into_iter().filter(|&l| l < lines));
        limits.push(0); // the whole file
    }
    limits.dedup();

    let mut why = String::new();
    for limit in limits {
        let hunks = split_hunks(src, limit);
        let mut bad = None;
        for h in &hunks {
            if relevant(h) {
                if let Err(e) = valid(h) {
                    bad = Some(e);
                    break;
                }
            }
        }
        match bad {
            None => return Ok(hunks),
            Some(e) => {
                why = e;
                if hunks.len() == 1 {
                    break; // already the whole file: nothing coarser to try
                }
            }
        }
    }
    Err(format!("not valid Ruby ({why})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_class() -> String {
        let mut s = String::from("module Shop\n  class Cart\n    attr_reader :items\n");
        for i in 0..6 {
            s.push_str(&format!("\n    def m{i}(x)\n      x + {i}\n    end\n"));
        }
        s.push_str("  end\nend\n");
        s
    }

    #[test]
    fn short_file_is_whole() {
        let src = big_class();
        assert_eq!(split_hunks(&src, 1000), vec![src.clone()]);
        assert_eq!(split_hunks(&src, 0), vec![src]);
    }

    #[test]
    fn splits_at_members_and_rewraps() {
        let hunks = split_hunks(&big_class(), 12);
        assert!(hunks.len() > 1, "{hunks:?}");
        for h in &hunks {
            // 12 original lines plus up to 2 header and 2 `end` lines of wrapping.
            assert!(h.lines().count() <= 12 + 4, "hunk too long:\n{h}");
            // Standalone: every hunk is wrapped in the module and class.
            assert!(h.starts_with("module Shop\n  class Cart\n"), "{h}");
            assert!(h.ends_with("  end\nend\n"), "{h}");
            let opens = h.lines().filter(|l| l.trim_start().starts_with("def ") || opens_scope(l.trim())).count();
            let ends = h.lines().filter(|l| closes_scope(l.trim())).count();
            assert_eq!(opens, ends, "unbalanced:\n{h}");
        }
        // Every method lands in exactly one hunk.
        for i in 0..6 {
            assert_eq!(hunks.iter().filter(|h| h.contains(&format!("def m{i}("))).count(), 1);
        }
    }

    #[test]
    fn oversized_member_stays_whole() {
        let mut src = String::from("class A\n  def big\n");
        for i in 0..30 {
            src.push_str(&format!("    x{i} = {i}\n"));
        }
        src.push_str("  end\n\n  def small\n  end\nend\n");
        let hunks = split_hunks(&src, 10);
        assert!(hunks.iter().any(|h| h.contains("x29 = 29") && h.contains("x0 = 0")));
        assert!(hunks.iter().all(|h| h.starts_with("class A\n")));
    }

    // ---- heredocs and other non-code text ---------------------------------------------

    #[test]
    fn heredoc_openers_are_recognised_but_shifts_are_not() {
        assert_eq!(heredoc_terminators("x = <<~EOS"), [("EOS".to_string(), true)]);
        assert_eq!(heredoc_terminators("run(<<-SQL, 1)"), [("SQL".to_string(), true)]);
        assert_eq!(heredoc_terminators("a = <<TEXT"), [("TEXT".to_string(), false)]);
        assert_eq!(heredoc_terminators("m(<<'.,.,', 'f.y', 1)"), [(".,.,".to_string(), false)]);
        assert_eq!(heredoc_terminators("f(<<~A, <<~B)"), [("A".to_string(), true), ("B".to_string(), true)]);
        for not_heredoc in ["list << item", "class << self", "x = y <<foo", "arr <<~", "# <<~EOS"] {
            assert!(heredoc_terminators(not_heredoc).is_empty(), "{not_heredoc}");
        }
    }

    #[test]
    fn the_mask_hides_heredocs_block_comments_and_data() {
        let src = "class A\n  X = <<~EOS\n    class Fake\n    end\n  EOS\n=begin\nclass Nope\n=end\n  def a; end\nend\n__END__\nclass After\n";
        let lines: Vec<&str> = src.lines().collect();
        let mask = code_mask(&lines);
        let code: Vec<&str> = lines.iter().zip(&mask).filter(|(_, m)| **m).map(|(l, _)| *l).collect();
        assert_eq!(code, ["class A", "  X = <<~EOS", "  def a; end", "end"]);
    }

    #[test]
    fn a_heredoc_full_of_class_and_end_does_not_corrupt_the_split() {
        let mut src = String::from("module Outer\n  class Host\n    TEXT = <<~EOS\n      class Fake\n      def not_code\n      end\n    EOS\n");
        for i in 0..10 {
            src.push_str(&format!("\n    def m{i}(x)\n      x\n    end\n"));
        }
        src.push_str("  end\nend\n");
        let hunks = split_hunks(&src, 12);
        assert!(hunks.len() > 1, "{hunks:?}");
        for h in &hunks {
            // The heredoc is whole or absent, never cut or doubled, and the scopes balance.
            assert_eq!(h.matches("<<~EOS").count(), h.lines().filter(|l| l.trim() == "EOS").count(), "{h}");
            // Every hunk is wrapped in the real scopes (the `class Fake` in the heredoc text is not one).
            assert!(h.starts_with("module Outer\n  class Host\n"), "{h}");
            assert!(h.ends_with("  end\nend\n"), "{h}");
        }
    }

    // ---- validated splitting -----------------------------------------------------------

    #[test]
    fn split_checked_returns_the_normal_split_when_every_hunk_is_valid() {
        let src = big_class();
        let mut checked = 0;
        let got = split_checked(&src, 12, &mut |_| true, &mut |_| {
            checked += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(got, split_hunks(&src, 12));
        assert_eq!(checked, got.len(), "every relevant hunk was checked once");
    }

    #[test]
    fn split_checked_falls_back_to_bigger_hunks_then_the_whole_file() {
        let src = big_class();
        let n = src.lines().count();
        // Anything shorter than the whole file is "invalid": only the single hunk passes.
        let got = split_checked(&src, 12, &mut |_| true, &mut |h| {
            if h.lines().count() >= n { Ok(()) } else { Err("bad cut".into()) }
        })
        .unwrap();
        assert_eq!(got, vec![src.clone()]);

        // An invalid whole file is an error that says why, and only relevant hunks are judged.
        let err = split_checked(&src, 12, &mut |_| true, &mut |_| Err("line 9: unexpected end".into())).unwrap_err();
        assert!(err.contains("not valid Ruby") && err.contains("unexpected end"), "{err}");
        let mut judged = 0;
        let ok = split_checked(&src, 12, &mut |_| false, &mut |_| {
            judged += 1;
            Err("never asked".into())
        });
        assert!(ok.is_ok() && judged == 0, "hunks nobody uses are not checked");
    }

    #[test]
    fn with_a_real_ruby_every_hunk_of_a_tricky_file_parses() {
        let ruby = "ruby";
        let mut src = String::from("module Outer\n  class Host\n    TEXT = <<~EOS\n      class Fake\n      end\n    EOS\n\n=begin\nclass Nope\n=end\n");
        for i in 0..12 {
            src.push_str(&format!("\n    def m{i}(x)\n      x\n    end\n"));
        }
        src.push_str("  end\nend\n");
        let mut valid = |h: &str| match crate::ruby_syntax::check_source(ruby, h) {
            Some(Ok(())) | None => Ok(()),
            Some(Err(e)) => Err(e.to_string()),
        };
        if crate::ruby_syntax::check_source(ruby, "1").is_none() {
            return eprintln!("skipped: no ruby");
        }
        let hunks = split_checked(&src, 14, &mut |_| true, &mut valid).unwrap();
        assert!(hunks.len() > 1);
        for h in &hunks {
            assert_eq!(crate::ruby_syntax::check_source(ruby, h), Some(Ok(())), "{h}");
        }
    }
}
