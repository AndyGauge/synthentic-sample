//! Importing pairs from a GitHub repository: shallow-clone it, read the files the
//! factory cares about, and let the factory turn them into pairs.

use crate::{
    Pair, PairFactory,
    pair::{ImportOptions, SourceFile},
    settings::GithubSettings,
};
use std::{
    fs,
    path::Path,
    process::Command,
};

/// A parsed repository spec.
#[derive(Debug, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub name: String,
    /// Branch or tag (`owner/repo@ref`).
    pub reference: Option<String>,
    pub clone_url: String,
}

fn valid_segment(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Accepts `owner/repo`, `https://github.com/owner/repo[.git]`,
/// `git@github.com:owner/repo[.git]`, each with an optional `@ref` suffix.
/// Anything else is rejected, which also keeps exotic git transports
/// (`ext::…`, `file://…`) and option-looking values out of the clone command.
pub fn parse_repo(spec: &str) -> Result<Repo, String> {
    let spec = spec.trim();
    let (ssh, path) = if let Some(p) = spec.strip_prefix("git@github.com:") {
        (true, p)
    } else {
        let p = spec
            .strip_prefix("https://github.com/")
            .or_else(|| spec.strip_prefix("github.com/"))
            .unwrap_or(spec);
        (false, p)
    };
    let (path, reference) = match path.split_once('@') {
        Some((p, r)) => (p, Some(r.to_string())),
        None => (path, None),
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let bad = || format!("not a GitHub repository: {spec:?} (expected owner/repo[@ref])");
    let (owner, name) = path.split_once('/').ok_or_else(bad)?;
    if !valid_segment(owner) || !valid_segment(name) {
        return Err(bad());
    }
    if let Some(r) = &reference {
        let ok = !r.is_empty() && !r.starts_with('-') && r.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
        if !ok {
            return Err(format!("invalid ref {r:?}"));
        }
    }
    let clone_url = if ssh {
        format!("git@github.com:{owner}/{name}.git")
    } else {
        format!("https://github.com/{owner}/{name}.git")
    };
    Ok(Repo { owner: owner.into(), name: name.into(), reference, clone_url })
}

/// Result of an import.
#[derive(Debug)]
pub struct Imported {
    /// `owner/repo@sha`
    pub label: String,
    pub pairs: Vec<Pair>,
    /// Files read (of the factory's extensions).
    pub files: usize,
}

pub(crate) fn git(settings: &GithubSettings, args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let mut cmd = Command::new(&settings.git);
    cmd.args(args)
        // Never wait on a credentials prompt, and only speak https/ssh.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ALLOW_PROTOCOL", "https:ssh");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd.output().map_err(|e| format!("failed to run `{}`: {e}", settings.git))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!("git {} failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Clones `spec` (shallow) and builds pairs with `factory`. Blocking: run it off the UI thread.
pub fn import(factory: &dyn PairFactory, spec: &str, settings: &GithubSettings) -> Result<Imported, String> {
    let repo = parse_repo(spec)?;
    if factory.import_extensions().is_empty() {
        return Err(format!("factory `{}` cannot import from repositories", factory.id()));
    }
    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let dest = tmp.path().join("repo");
    let dest_str = dest.to_string_lossy().to_string();
    let mut args = vec!["clone", "--depth", "1", "--quiet"];
    if let Some(r) = &repo.reference {
        args.extend(["--branch", r]);
    }
    args.extend(["--", &repo.clone_url, &dest_str]);
    git(settings, &args, None)?;
    let sha = git(settings, &["rev-parse", "--short=7", "HEAD"], Some(&dest))?;
    let label = format!("{}/{}@{sha}", repo.owner, repo.name);
    let (pairs, files) = import_dir(factory, &dest, &format!("gh:{label}"), settings);
    Ok(Imported { label, pairs, files })
}

/// Reads the factory's files under `root` and builds pairs. Returns the pairs and
/// the number of files read. Split out from [`import`] so it can be tested without git.
pub fn import_dir(factory: &dyn PairFactory, root: &Path, label: &str, settings: &GithubSettings) -> (Vec<Pair>, usize) {
    let mut files = Vec::new();
    let skip = |name: &str| settings.exclude_dirs.iter().any(|d| d == name);
    collect(root, root, factory.import_extensions(), &skip, settings.max_file_bytes, &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path)); // deterministic order
    let pairs = factory.import_files(label, &files, &ImportOptions { max_lines: settings.max_lines });
    (pairs, files.len())
}

/// Reads files under `dir` with one of `exts` into `out` (paths relative to `root`).
/// Directories for which `skip` returns true are not entered; symlinks, files over
/// `max_bytes` and non-UTF-8 files are ignored.
pub(crate) fn collect(
    root: &Path,
    dir: &Path,
    exts: &[&str],
    skip: &dyn Fn(&str) -> bool,
    max_bytes: u64,
    out: &mut Vec<SourceFile>,
) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if kind.is_symlink() {
            continue; // never follow links out of the checkout
        }
        if kind.is_dir() {
            if !skip(&name) {
                collect(root, &path, exts, skip, max_bytes, out);
            }
        } else if kind.is_file()
            && exts.iter().any(|e| name.ends_with(e))
            && entry.metadata().is_ok_and(|m| m.len() <= max_bytes)
        {
            if let (Ok(text), Ok(rel)) = (fs::read_to_string(&path), path.strip_prefix(root)) {
                let path = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                out.push(SourceFile { path, text });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factories::ruby_rbs::RubyRbsFactory;

    #[test]
    fn parses_specs() {
        let want = |o: &str, n: &str, r: Option<&str>, u: &str| Repo {
            owner: o.into(), name: n.into(), reference: r.map(Into::into), clone_url: u.into(),
        };
        let https = "https://github.com/AndyGauge/rbs-sentinel.git";
        assert_eq!(parse_repo("AndyGauge/rbs-sentinel").unwrap(), want("AndyGauge", "rbs-sentinel", None, https));
        assert_eq!(parse_repo("https://github.com/AndyGauge/rbs-sentinel").unwrap(), want("AndyGauge", "rbs-sentinel", None, https));
        assert_eq!(parse_repo("github.com/AndyGauge/rbs-sentinel.git/").unwrap(), want("AndyGauge", "rbs-sentinel", None, https));
        assert_eq!(parse_repo("a/b@v1.2").unwrap(), want("a", "b", Some("v1.2"), "https://github.com/a/b.git"));
        assert_eq!(
            parse_repo("git@github.com:a/b.git@feat/x").unwrap(),
            want("a", "b", Some("feat/x"), "git@github.com:a/b.git")
        );
    }

    #[test]
    fn rejects_everything_else() {
        for bad in [
            "", "just-a-name", "-oops/repo", "a/-b", "a/b c", "ext::sh -c id/x", "file:///etc/passwd",
            "https://gitlab.com/a/b", "a/b@--upload-pack=x", "a/b@", "../../x",
        ] {
            assert!(parse_repo(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn imports_a_checkout_without_git() {
        let dir = tempfile::tempdir().unwrap();
        let w = |p: &str, t: &str| {
            let f = dir.path().join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, t).unwrap();
        };
        w("lib/shop/cart.rb", "module Shop\n  class Cart\n    attr_reader :items\n\n    def add(item)\n    end\n  end\nend\n");
        w("lib/shop/other.rb", "class Other\nend\n");
        w("sig/shop/cart.rbs", "module Shop\n  class Cart\n    attr_reader items: Array[String]\n    def add: (String) -> void\n  end\nend\n");
        w("vendor/bundle/x.rb", "module Shop\n  class Cart\n    def add(i); end\n  end\nend\n");
        w("README.md", "ignored");

        let (pairs, files) = import_dir(&RubyRbsFactory::default(), dir.path(), "gh:a/b@abc1234", &GithubSettings::default());
        assert_eq!(files, 3, "vendor/ and README are not read");
        assert_eq!(pairs.len(), 1, "only the file with matching signatures: {pairs:#?}");
        let p = &pairs[0];
        assert_eq!(p.id, "ruby-rbs:gh:a/b@abc1234:lib/shop/cart.rb");
        assert!(!p.input.contains("#:"));
        assert!(p.output.contains("attr_reader :items #: Array[String]"));
        assert!(p.output.contains("#: (String) -> void\n    def add(item)"));
        // Importing again gives identical pairs (stable ids and text).
        let (again, _) = import_dir(&RubyRbsFactory::default(), dir.path(), "gh:a/b@abc1234", &GithubSettings::default());
        assert_eq!((again[0].id.clone(), again[0].output.clone(), again[0].instruction.clone()), (p.id.clone(), p.output.clone(), p.instruction.clone()));
    }

    #[test]
    fn large_file_becomes_hunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut rb = String::from("class Big\n");
        let mut rbs = String::from("class Big\n");
        for i in 0..40 {
            rb.push_str(&format!("\n  def m{i}(x)\n    x\n  end\n"));
            rbs.push_str(&format!("  def m{i}: (Integer) -> Integer\n"));
        }
        rb.push_str("end\n");
        rbs.push_str("end\n");
        fs::create_dir_all(dir.path().join("sig")).unwrap();
        fs::write(dir.path().join("big.rb"), &rb).unwrap();
        fs::write(dir.path().join("sig/big.rbs"), &rbs).unwrap();

        let s = GithubSettings { max_lines: 30, ..Default::default() };
        let (pairs, _) = import_dir(&RubyRbsFactory::default(), dir.path(), "gh:a/b@1", &s);
        assert!(pairs.len() > 3, "{}", pairs.len());
        assert!(pairs[0].id.ends_with("big.rb#1"));
        // Every method is annotated exactly once across the hunks.
        for i in 0..40 {
            let n = pairs.iter().filter(|p| p.output.contains(&format!("#: (Integer) -> Integer\n  def m{i}("))).count();
            assert_eq!(n, 1, "m{i}");
        }
    }
}
