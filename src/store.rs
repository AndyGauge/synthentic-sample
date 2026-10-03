use crate::Pair;
use std::{fs, io, path::{Path, PathBuf}};

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
            compiled: String::new(),
            compile_error: None,
            diagnostics: Vec::new(),
        }
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
