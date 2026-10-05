use crate::Pair;
use std::{fs, io, path::{Path, PathBuf}};

/// What [`Store::merge`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub added: usize,
    /// Existing, unedited pairs replaced by what the generator produces now.
    pub updated: usize,
    /// Existing pairs whose input changed: a different pair that shares the id, replaced outright.
    pub replaced: usize,
    /// Existing pairs whose output differs from the incoming one (a human edit): kept as they are.
    pub kept_edited: usize,
    /// Ids that still need compiling with the current sentinel (the kept, edited ones).
    pub recompile: Vec<String>,
    /// Pairs from a source this run covered (same factory and gem or repo version) that the run
    /// no longer produced, such as a hunk the splitter or importer now skips. [`Store::remove`]
    /// prunes them. A pair a person edited is never listed.
    pub stale: Vec<String>,
}

/// `factory:kind:label` of an imported pair's id (`ruby-rbs:gem:redis@5.0.0:lib/redis.rb#2` →
/// `ruby-rbs:gem:redis@5.0.0`): the source a run re-imports as a whole. Ids without a path part,
/// like the synthetic `ruby-rbs:7`, have none and are never stale.
fn source_of(id: &str) -> Option<&str> {
    let mut at = id.match_indices(':').map(|(i, _)| i);
    let (_, _, third) = (at.next()?, at.next()?, at.next()?);
    Some(&id[..third])
}

/// JSONL-backed collection of pairs (one JSON object per line).
pub struct Store {
    path: PathBuf,
    pub pairs: Vec<Pair>,
}

impl Store {
    /// Opens `path`, treating a missing file as empty.
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let pairs = match fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .enumerate()
                .map(|(i, l)| {
                    serde_json::from_str(l).map_err(|e| {
                        io::Error::new(io::ErrorKind::InvalidData, format!("line {}: {e}", i + 1))
                    })
                })
                .collect::<io::Result<_>>()?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path, pairs })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds `pair` unless one with the same id exists. Returns whether it was added.
    pub fn add(&mut self, pair: Pair) -> bool {
        if self.pairs.iter().any(|p| p.id == pair.id) {
            return false;
        }
        self.pairs.push(pair);
        true
    }

    /// Folds the pairs from a scenario run into the store without losing human work.
    ///
    /// * New ids are added.
    /// * An id whose **input differs** is a different pair that happens to share a name (a file
    ///   that was split differently, say), and its old `expected` would no longer describe it,
    ///   so it is replaced outright.
    /// * Otherwise, a pair nobody edited is replaced by what the generator produces now: new
    ///   instruction, output, ground truth and compile result. "Edited" means the text no longer
    ///   matches [`Pair::generated`]; for a legacy pair without one, an output that differs from
    ///   the incoming one counts as an edit.
    /// * An edited pair keeps its text and old compile result and has only `expected`
    ///   refreshed; it is reported in `recompile`.
    pub fn merge(&mut self, incoming: Vec<Pair>) -> MergeStats {
        let mut index: std::collections::HashMap<String, usize> =
            self.pairs.iter().enumerate().map(|(i, p)| (p.id.clone(), i)).collect();
        let mut stats = MergeStats::default();
        let seen: std::collections::HashSet<&str> = incoming.iter().map(|p| p.id.as_str()).collect();
        let sources: std::collections::HashSet<&str> = incoming.iter().filter_map(|p| source_of(&p.id)).collect();
        stats.stale = self
            .pairs
            .iter()
            .filter(|p| !seen.contains(p.id.as_str()))
            .filter(|p| source_of(&p.id).is_some_and(|s| sources.contains(s)))
            .filter(|p| p.generated == 0 || p.fingerprint() == p.generated)
            .map(|p| p.id.clone())
            .collect();
        for mut new in incoming {
            new.stamp();
            let Some(&i) = index.get(&new.id) else {
                index.insert(new.id.clone(), self.pairs.len());
                self.pairs.push(new);
                stats.added += 1;
                continue;
            };
            let old = &mut self.pairs[i];
            if old.input != new.input {
                *old = new;
                stats.replaced += 1;
                continue;
            }
            let edited = if old.generated != 0 { old.fingerprint() != old.generated } else { old.output != new.output };
            if edited {
                old.expected = new.expected;
                stats.kept_edited += 1;
                stats.recompile.push(old.id.clone());
            } else {
                *old = new;
                stats.updated += 1;
            }
        }
        stats
    }

    /// Removes the pairs with these ids. Returns how many were removed.
    pub fn remove(&mut self, ids: &[String]) -> usize {
        let before = self.pairs.len();
        self.pairs.retain(|p| !ids.contains(&p.id));
        before - self.pairs.len()
    }

    /// Writes atomically (temp file + rename) so a crash never truncates the dataset.
    pub fn save(&self) -> io::Result<()> {
        let mut out = String::new();
        for p in &self.pairs {
            out.push_str(&serde_json::to_string(p).map_err(io::Error::other)?);
            out.push('\n');
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        fs::write(&tmp, out)?;
        fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(seed: u64) -> Pair {
        Pair {
            id: Pair::make_id("t", seed),
            factory: "t".into(),
            seed,
            instruction: "i\nmultiline".into(),
            input: "a".into(),
            output: "b".into(),
            expected: String::new(),
            compiled: String::new(),
            compile_error: None,
            diagnostics: Vec::new(),
            generated: 0,
        }
    }

    fn fresh(seed: u64, output: &str) -> Pair {
        let mut p = pair(seed);
        p.output = output.into();
        p.compiled = "new compile".into();
        p.expected = "new expected".into();
        p.instruction = "generated instruction".into();
        p
    }

    #[test]
    fn merge_adds_updates_and_never_overwrites_a_human_edit() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("p.jsonl")).unwrap();
        let mut untouched = pair(1);
        untouched.compiled = "old".into();
        untouched.expected = "old expected".into();
        // Legacy pair (no fingerprint) whose output differs from the incoming one: treated as edited.
        let mut edited = pair(2);
        edited.output = "human wrote this".into();
        edited.instruction = "my instruction".into();
        edited.compiled = "compiled from the human version".into();
        s.add(untouched);
        s.add(edited);

        let stats = s.merge(vec![fresh(1, "b"), fresh(2, "generated"), fresh(3, "c")]);
        assert_eq!(stats, MergeStats { added: 1, updated: 1, kept_edited: 1, recompile: vec!["t:2".into()], ..Default::default() });

        assert_eq!((s.pairs[0].compiled.as_str(), s.pairs[0].expected.as_str()), ("new compile", "new expected"));
        assert_eq!(s.pairs[1].output, "human wrote this");
        assert_eq!(s.pairs[1].instruction, "my instruction");
        assert_eq!(s.pairs[1].compiled, "compiled from the human version");
        assert_eq!(s.pairs[1].expected, "new expected");
        assert_eq!(s.pairs[2].id, "t:3");
        assert_eq!(s.pairs.len(), 3);
    }

    #[test]
    fn an_unedited_pair_follows_the_generator_when_its_output_changes() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("p.jsonl")).unwrap();
        s.merge(vec![fresh(1, "old generated output")]);
        assert_eq!(s.pairs[0].generated, s.pairs[0].fingerprint());

        // The generator improved: same id and input, different output. Nobody touched the pair.
        let stats = s.merge(vec![fresh(1, "better generated output")]);
        assert_eq!((stats.updated, stats.kept_edited), (1, 0));
        assert_eq!(s.pairs[0].output, "better generated output");
        assert_eq!(s.pairs[0].generated, s.pairs[0].fingerprint());
    }

    #[test]
    fn an_edit_made_after_the_pair_was_stored_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("p.jsonl")).unwrap();
        s.merge(vec![fresh(1, "generated")]);
        s.pairs[0].output = "hand edited".into();
        s.pairs[0].compiled = "compiled from the edit".into();

        let stats = s.merge(vec![fresh(1, "generated")]);
        assert_eq!((stats.updated, stats.kept_edited, stats.recompile), (0, 1, vec!["t:1".to_string()]));
        assert_eq!((s.pairs[0].output.as_str(), s.pairs[0].compiled.as_str()), ("hand edited", "compiled from the edit"));
        assert_eq!(s.pairs[0].expected, "new expected");
        // Still marked as edited next time, so it keeps being protected.
        assert_ne!(s.pairs[0].generated, s.pairs[0].fingerprint());
    }

    #[test]
    fn a_pair_whose_input_changed_is_replaced_even_if_edited() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("p.jsonl")).unwrap();
        s.merge(vec![fresh(1, "generated")]);
        s.pairs[0].output = "hand edited".into();

        // Same id, but the file was split differently: different input, so a different pair.
        let mut resplit = fresh(1, "generated for the new slice");
        resplit.input = "a different slice".into();
        let stats = s.merge(vec![resplit]);
        assert_eq!((stats.replaced, stats.updated, stats.kept_edited), (1, 0, 0));
        assert_eq!((s.pairs[0].input.as_str(), s.pairs[0].output.as_str()), ("a different slice", "generated for the new slice"));
        assert_eq!(s.pairs[0].expected, "new expected");
        assert_eq!(s.pairs.len(), 1);
    }

    #[test]
    fn merge_reports_pairs_a_run_stopped_producing_and_remove_prunes_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("p.jsonl")).unwrap();
        let gem = |path: &str, n: u32| {
            let mut p = pair(0);
            p.id = format!("ruby-rbs:gem:redis@5.0.0:{path}#{n}");
            p
        };
        let mut other_version = pair(0);
        other_version.id = "ruby-rbs:gem:redis@4.0.0:lib/redis.rb#1".into();
        let mut synthetic = pair(0);
        synthetic.id = "ruby-rbs:7".into();
        let mut edited = gem("lib/edited.rb", 1);
        edited.output = "hand edited".into();
        for p in [gem("lib/redis.rb", 1), gem("lib/redis.rb", 2), gem("lib/gone.rb", 1), other_version, synthetic] {
            s.add(p);
        }
        s.merge(vec![gem("lib/redis.rb", 1), gem("lib/redis.rb", 2)]); // stamps the stored pairs
        s.add(edited); // legacy, no fingerprint: not edited as far as anyone can tell
        s.pairs.last_mut().unwrap().stamp();
        s.pairs.last_mut().unwrap().output = "now hand edited".into();

        // The next run no longer produces redis.rb#2, gone.rb#1 or edited.rb#1.
        let stats = s.merge(vec![gem("lib/redis.rb", 1)]);
        let mut stale = stats.stale.clone();
        stale.sort();
        // The edited one is protected; redis@4.0.0 and the synthetic pair are outside this run.
        assert_eq!(stale, vec!["ruby-rbs:gem:redis@5.0.0:lib/gone.rb#1", "ruby-rbs:gem:redis@5.0.0:lib/redis.rb#2"]);

        assert_eq!(s.remove(&stats.stale), 2);
        assert!(s.pairs.iter().all(|p| !stats.stale.contains(&p.id)));
        assert!(s.pairs.iter().any(|p| p.id == "ruby-rbs:7") && s.pairs.iter().any(|p| p.id.contains("4.0.0")));
    }

    #[test]
    fn roundtrip_and_dedupe() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        let mut s = Store::open(&path).unwrap();
        assert!(s.add(pair(1)));
        assert!(!s.add(pair(1)));
        s.add(pair(2));
        s.save().unwrap();
        assert_eq!(Store::open(&path).unwrap().pairs, s.pairs);
    }
}
