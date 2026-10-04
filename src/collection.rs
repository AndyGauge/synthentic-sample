//! `ruby/gem_rbs_collection` as an import source.
//!
//! The collection holds `gems/<gem>/<version>/*.rbs` — signatures only. The Ruby
//! they describe is the gem's own source, so each gem is downloaded with
//! `gem unpack` (cached on disk) and handed to the factory together with the
//! collection's `.rbs`, which reverse-compiles them into inline annotations.

use crate::{
    Pair, PairFactory,
    github::{Imported, collect, git},
    pair::ImportOptions,
    settings::{CollectionSettings, GithubSettings},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

/// One `gems/<name>/<version>` directory of the collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gem {
    pub name: String,
    /// Directory name, e.g. `4.2` (a `~>` series, not an exact release).
    pub version: String,
    pub dir: PathBuf,
}

/// Result of [`sync`].
#[derive(Debug)]
pub struct Synced {
    pub root: PathBuf,
    pub commit: String,
    pub gems: usize,
}

pub fn cache_dir(s: &CollectionSettings) -> PathBuf {
    if !s.cache_dir.is_empty() {
        return PathBuf::from(&s.cache_dir);
    }
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home).join(".cache/synthentic-sample"),
        None => std::env::temp_dir().join("synthentic-sample"),
    }
}

fn checkout(s: &CollectionSettings) -> PathBuf {
    cache_dir(s).join("gem_rbs_collection")
}

/// Clones the collection into the cache, or fast-forwards an existing clone to the
/// latest commit.
pub fn sync(s: &CollectionSettings, gh: &GithubSettings) -> Result<Synced, String> {
    let root = checkout(s);
    if root.join(".git").exists() {
        git(gh, &["fetch", "--quiet", "--depth", "1", "origin", "HEAD"], Some(&root))?;
        git(gh, &["reset", "--quiet", "--hard", "FETCH_HEAD"], Some(&root))?;
    } else {
        fs::create_dir_all(cache_dir(s)).map_err(|e| format!("create cache dir: {e}"))?;
        let dest = root.to_string_lossy().to_string();
        git(gh, &["clone", "--quiet", "--depth", "1", "--", &s.url, &dest], None)?;
    }
    let commit = git(gh, &["rev-parse", "--short=7", "HEAD"], Some(&root))?;
    Ok(Synced { gems: list_gems(&root).iter().map(|g| &g.name).collect::<std::collections::BTreeSet<_>>().len(), root, commit })
}

/// The collection checkout to import from: freshly synced when `auto_sync` is on
/// (falling back to an existing checkout if the network is down), else as cached.
pub fn ensure(s: &CollectionSettings, gh: &GithubSettings) -> Result<PathBuf, String> {
    let root = checkout(s);
    if s.auto_sync || !root.join("gems").is_dir() {
        match sync(s, gh) {
            Ok(synced) => return Ok(synced.root),
            Err(e) if root.join("gems").is_dir() => eprintln!("collection sync failed, using cached copy: {e}"),
            Err(e) => return Err(e),
        }
    }
    Ok(root)
}

/// Sort key for dotted versions (`10.0` after `9.1`; non-numeric parts count as 0).
fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Every `gems/<name>/<version>` in the checkout, sorted by name then version.
pub fn list_gems(root: &Path) -> Vec<Gem> {
    let mut out = Vec::new();
    let Ok(names) = fs::read_dir(root.join("gems")) else { return out };
    for name in names.flatten().filter(|e| e.path().is_dir()) {
        let gem = name.file_name().to_string_lossy().to_string();
        if !valid_name(&gem) {
            continue;
        }
        let Ok(versions) = fs::read_dir(name.path()) else { continue };
        for v in versions.flatten().filter(|e| e.path().is_dir()) {
            let version = v.file_name().to_string_lossy().to_string();
            if valid_name(&version) {
                out.push(Gem { name: gem.clone(), version, dir: v.path() });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| version_key(&a.version).cmp(&version_key(&b.version))));
    out
}

/// The newest version directory of each gem.
pub fn latest(gems: &[Gem]) -> Vec<Gem> {
    let mut out: Vec<Gem> = Vec::new();
    for g in gems {
        match out.last_mut() {
            Some(last) if last.name == g.name => *last = g.clone(), // sorted ascending: later is newer
            _ => out.push(g.clone()),
        }
    }
    out
}

/// Resolves `gem:NAME` (newest version) or `gem:NAME/VERSION` against the checkout.
pub fn find(root: &Path, spec: &str) -> Result<Gem, String> {
    let (name, version) = match spec.split_once('/') {
        Some((n, v)) => (n, Some(v)),
        None => (spec, None),
    };
    let mut all: Vec<Gem> = list_gems(root).into_iter().filter(|g| g.name == name).collect();
    match version {
        Some(v) => all.into_iter().find(|g| g.version == v).ok_or_else(|| format!("no {name}/{v} in the collection")),
        None => all.pop().ok_or_else(|| format!("no gem named {name:?} in the collection")), // sorted ascending: last is newest
    }
}

/// `gem unpack` requirement for a collection version directory: `4.2` → `~> 4.2.0`.
fn requirement(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    if !parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        return ">= 0".into();
    }
    match parts.len() {
        1 | 2 => format!("~> {version}.0"),
        _ => format!("= {version}"),
    }
}

/// An already-unpacked `NAME-<version>` in `gems_dir` belonging to the `version` series.
fn find_cached(gems_dir: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let prefix = format!("{name}-");
    let mut best: Option<(Vec<u64>, PathBuf)> = None;
    for e in fs::read_dir(gems_dir).ok()?.flatten().filter(|e| e.path().is_dir()) {
        let file = e.file_name().to_string_lossy().to_string();
        let Some(rest) = file.strip_prefix(&prefix) else { continue };
        if rest == version || rest.starts_with(&format!("{version}.")) {
            let key = version_key(rest);
            if best.as_ref().is_none_or(|(k, _)| key > *k) {
                best = Some((key, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// The gem's source for the series `g.version`: from the cache, else `gem unpack`.
fn gem_source(g: &Gem, s: &CollectionSettings) -> Result<PathBuf, String> {
    let gems_dir = cache_dir(s).join("gems");
    fs::create_dir_all(&gems_dir).map_err(|e| format!("create {}: {e}", gems_dir.display()))?;
    if let Some(dir) = find_cached(&gems_dir, &g.name, &g.version) {
        return Ok(dir);
    }
    let out = Command::new(&s.gem)
        .args(["unpack", &g.name, "--version", &requirement(&g.version), "--target"])
        .arg(&gems_dir)
        .output()
        .map_err(|e| format!("failed to run `{} unpack`: {e}", s.gem))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("gem unpack {} ({}) failed: {}", g.name, requirement(&g.version), err.lines().next().unwrap_or("").trim()));
    }
    // "Unpacked gem: '/path/to/name-1.2.3'"
    let stdout = String::from_utf8_lossy(&out.stdout);
    let path = stdout
        .split('\'')
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .ok_or_else(|| {
            // `gem` can exit 0 yet unpack nothing (e.g. a gem that isn't on rubygems.org).
            let err = String::from_utf8_lossy(&out.stderr);
            let why = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("no gem was unpacked");
            format!("gem unpack {} ({}): {}", g.name, requirement(&g.version), why.trim())
        })?;
    Ok(path)
}

/// Builds pairs for one gem: its Ruby source plus the collection's signatures.
pub fn import_gem(
    factory: &dyn PairFactory,
    g: &Gem,
    s: &CollectionSettings,
    gh: &GithubSettings,
) -> Result<Imported, String> {
    let src = gem_source(g, s)?;
    let dirname = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let resolved = dirname.strip_prefix(&format!("{}-", g.name)).unwrap_or(&dirname);
    let label = format!("gem:{}@{resolved}", g.name);

    let mut files = Vec::new();
    // The gem's Ruby (its own .rbs, if it ships any, is not ground truth we trust here).
    let skip_gem = |name: &str| s.exclude_dirs.iter().any(|d| d == name);
    collect(&src, &src, &[".rb"], &skip_gem, gh.max_file_bytes, &mut files);
    // The collection's signatures; `_src`/`_test` hold tooling, not signatures.
    let mut sigs = Vec::new();
    collect(&g.dir, &g.dir, &[".rbs"], &|name: &str| name.starts_with('_'), gh.max_file_bytes, &mut sigs);
    for f in &mut sigs {
        f.path = format!("sig/{}", f.path);
    }
    files.extend(sigs);
    files.sort_by(|a, b| a.path.cmp(&b.path)); // deterministic

    let count = files.len();
    let pairs: Vec<Pair> = factory.import_files(&label, &files, &ImportOptions { max_lines: gh.max_lines });
    Ok(Imported { label, pairs, files: count })
}

/// Imports `gems` with up to `s.jobs` at a time, calling `on_done` (from worker
/// threads) as each finishes. Blocks until all are done.
pub fn import_gems(
    factory: &dyn PairFactory,
    gems: &[Gem],
    s: &CollectionSettings,
    gh: &GithubSettings,
    on_done: &(dyn Fn(&Gem, Result<Imported, String>) + Sync),
) {
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..s.jobs.clamp(1, gems.len().max(1)) {
            scope.spawn(|| {
                while let Some(g) = gems.get(next.fetch_add(1, Ordering::SeqCst)) {
                    on_done(g, import_gem(factory, g, s, gh));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factories::ruby_rbs::RubyRbsFactory;
    use std::sync::Mutex;

    fn write(root: &Path, rel: &str, text: &str) {
        let f = root.join(rel);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, text).unwrap();
    }

    /// A fake collection plus a pre-populated gem cache, so no `gem`/network is needed.
    fn fixture() -> (tempfile::TempDir, CollectionSettings) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("gem_rbs_collection");
        write(&root, "gems/shop/1.0/cart.rbs", "class Shop::Cart\n  def add: (String) -> void\nend\n");
        write(&root, "gems/shop/1.2/cart.rbs", "class Shop::Cart\n  def add: (String) -> void\n  def clear: () -> void\nend\n");
        write(&root, "gems/shop/1.2/_test/x.rbs", "class Ignored\n  def x: () -> void\nend\n");
        write(&root, "gems/shop/README.md", "not a version dir");
        write(&root, "gems/other/9.0/o.rbs", "class Other\n  def o: () -> void\nend\n");
        write(&root, "gems/other/10.0/o.rbs", "class Other\n  def o: () -> void\nend\n");
        let gems = dir.path().join("gems");
        write(&gems, "shop-1.2.7/lib/shop/cart.rb", "module Shop\n  class Cart\n    def add(item)\n    end\n\n    def clear\n    end\n  end\nend\n");
        write(&gems, "shop-1.2.7/test/cart_test.rb", "module Shop\n  class Cart\n    def add(i); end\n  end\nend\n");
        write(&gems, "shop-1.2.3/lib/old.rb", "class Old\nend\n");
        let s = CollectionSettings { cache_dir: dir.path().to_string_lossy().into(), ..Default::default() };
        (dir, s)
    }

    #[test]
    fn lists_gems_with_numeric_version_order() {
        let (dir, _) = fixture();
        let all = list_gems(&dir.path().join("gem_rbs_collection"));
        let ids: Vec<_> = all.iter().map(|g| format!("{}/{}", g.name, g.version)).collect();
        assert_eq!(ids, ["other/9.0", "other/10.0", "shop/1.0", "shop/1.2"], "README.md is not a version");
        let newest: Vec<_> = latest(&all).iter().map(|g| format!("{}/{}", g.name, g.version)).collect();
        assert_eq!(newest, ["other/10.0", "shop/1.2"], "10.0 is newer than 9.0");
        let root = dir.path().join("gem_rbs_collection");
        assert_eq!(find(&root, "shop").unwrap().version, "1.2");
        assert_eq!(find(&root, "shop/1.0").unwrap().version, "1.0");
        assert!(find(&root, "shop/9.9").is_err() && find(&root, "nope").is_err());
    }

    #[test]
    fn requirements() {
        assert_eq!(requirement("4.2"), "~> 4.2.0");
        assert_eq!(requirement("6"), "~> 6.0");
        assert_eq!(requirement("1.2.3"), "= 1.2.3");
        assert_eq!(requirement("x.y"), ">= 0");
    }

    #[test]
    fn cache_picks_newest_matching_release() {
        let (dir, _) = fixture();
        let gems = dir.path().join("gems");
        assert_eq!(find_cached(&gems, "shop", "1.2").unwrap().file_name().unwrap(), "shop-1.2.7");
        assert!(find_cached(&gems, "shop", "2.0").is_none());
        assert!(find_cached(&gems, "sho", "1.2").is_none(), "prefix of another name");
    }

    #[test]
    fn imports_a_gem_from_cache() {
        let (dir, s) = fixture();
        let g = find(&dir.path().join("gem_rbs_collection"), "shop").unwrap();
        let imported = import_gem(&RubyRbsFactory::default(), &g, &s, &GithubSettings::default()).unwrap();
        assert_eq!(imported.label, "gem:shop@1.2.7");
        assert_eq!(imported.pairs.len(), 1, "test/ is excluded: {:#?}", imported.pairs);
        let p = &imported.pairs[0];
        assert_eq!(p.id, "ruby-rbs:gem:shop@1.2.7:lib/shop/cart.rb");
        assert!(p.output.contains("#: (String) -> void\n    def add(item)"));
        assert!(p.output.contains("#: () -> void\n    def clear"));
        assert!(!p.input.contains("#:"));
        assert!(p.expected.contains("class Shop::Cart") && p.expected.contains("def clear: () -> void"));
    }

    #[test]
    fn imports_many_in_parallel() {
        let (dir, mut s) = fixture();
        s.gem = "/nonexistent/gem".into(); // keep the test off the network
        let gems = latest(&list_gems(&dir.path().join("gem_rbs_collection")));
        let done: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());
        import_gems(&RubyRbsFactory::default(), &gems, &s, &GithubSettings::default(), &|g, r| {
            done.lock().unwrap().push((g.name.clone(), r.is_ok()));
        });
        let mut done = done.into_inner().unwrap();
        done.sort();
        // `shop` imports from the cache; `other` has no cached source and no `gem`, so it fails cleanly.
        assert_eq!(done.len(), 2);
        assert_eq!(done, [("other".to_string(), false), ("shop".to_string(), true)]);
    }
}
