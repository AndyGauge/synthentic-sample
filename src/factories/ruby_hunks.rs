//! Splitting a long Ruby file into standalone hunks.
//!
//! Heuristic and indentation based, not a parser: cut points are blank lines that
//! precede a sibling member of the innermost `class`/`module`. Each hunk after the
//! first is prefixed with its enclosing `class`/`module` header lines and followed
//! by the matching `end`s, so it stands alone (and can be annotated and compiled).

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn opens_scope(t: &str) -> bool {
    (t.starts_with("class ") || t.starts_with("module ")) && !t.ends_with(" end") && !t.contains("; end")
}

fn closes_scope(t: &str) -> bool {
    t == "end" || t.starts_with("end ") || t.starts_with("end#")
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

    // Open scope headers (as line indices) at the start of every line, plus at EOF.
    let mut stacks: Vec<Vec<usize>> = Vec::with_capacity(lines.len() + 1);
    let mut stack: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        stacks.push(stack.clone());
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
            lines[i - 1].trim().is_empty()
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
}
