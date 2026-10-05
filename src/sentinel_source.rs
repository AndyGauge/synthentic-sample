//! Getting the sentinel a scenario tests.
//!
//! * `rubygems`: the latest published `rbs-sentinel` gem. A `.gem` is a tar archive whose
//!   `data.tar.gz` holds one prebuilt binary per platform (`exe/sentinel-x86_64-darwin`,
//!   ...); the one for this machine is extracted into the cache, per version.
//! * `git`: clone a ref and `cargo build --release` it, to test code that isn't released.
//! * `path` / `installed`: use a binary that is already there.
//!
//! Whatever the source, the result is probed: `sentinel lsp` is started and the version it
//! reports in its `initialize` reply is recorded, which also proves the binary runs.

use crate::{
    collection::{cache_dir, version_key},
    lsp::LspClient,
    settings::{CollectionSettings, GithubSettings, SentinelSettings, SentinelSource, Tool},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// A sentinel binary ready to be run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedSentinel {
    /// What to execute (an absolute path, or a bare command for `installed`).
    pub command: String,
    /// The version the binary reports, plus a revision for `git` sources.
    pub version: String,
    /// How it was obtained, e.g. `rubygems rbs-sentinel 0.6.0`.
    pub origin: String,
}

/// `sentinel-<arch>-<os>` as the gem names its binaries.
pub fn gem_binary_name(arch: &str, os: &str) -> Result<String, String> {
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => return Err(format!("no sentinel binary for CPU `{other}`")),
    };
    let (os, ext) = match os {
        "macos" => ("darwin", ""),
        "linux" => ("linux", ""),
        "windows" => ("windows", ".exe"),
        other => return Err(format!("no sentinel binary for OS `{other}`")),
    };
    Ok(format!("sentinel-{arch}-{os}{ext}"))
}

/// Pulls `exe/<member>` out of a `.gem` and writes it to `dest` (executable).
pub fn extract_gem_binary(gem: &Path, member: &str, dest: &Path) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    // gem (tar) -> data.tar.gz -> exe/<member>
    let mut outer = Command::new("tar")
        .arg("-xOf")
        .arg(gem)
        .arg("data.tar.gz")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to run `tar`: {e}"))?;
    let inner = Command::new("tar")
        .args(["-xzf", "-", "-C"])
        .arg(tmp.path())
        .arg(format!("exe/{member}"))
        .stdin(outer.stdout.take().ok_or("tar: no stdout")?)
        .output()
        .map_err(|e| format!("failed to run `tar`: {e}"))?;
    let _ = outer.wait();
    let extracted = tmp.path().join("exe").join(member);
    if !inner.status.success() || !extracted.is_file() {
        return Err(format!(
            "{} has no exe/{member} (tar: {})",
            gem.display(),
            String::from_utf8_lossy(&inner.stderr).lines().next().unwrap_or("")
        ));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Copy then rename, so a half-written file is never mistaken for a good one.
    let part = dest.with_extension("part");
    fs::copy(&extracted, &part).map_err(|e| format!("copy: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&part, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    fs::rename(&part, dest).map_err(|e| e.to_string())
}

/// Version of the newest published `gem_name`, from `gem list --remote`.
fn latest_gem_version(gem: &str, gem_name: &str) -> Result<String, String> {
    let out = Command::new(gem)
        .args(["list", "--remote", "--exact", gem_name])
        .output()
        .map_err(|e| format!("failed to run `{gem} list`: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    // "rbs-sentinel (0.6.0)" — the newest version is listed first.
    parse_gem_list_version(&text, gem_name).ok_or_else(|| {
        let err = String::from_utf8_lossy(&out.stderr);
        format!("could not find `{gem_name}` on rubygems ({} {})", text.trim(), err.trim())
    })
}

fn parse_gem_list_version(text: &str, gem_name: &str) -> Option<String> {
    let line = text.lines().find(|l| l.starts_with(&format!("{gem_name} (")))?;
    let inside = line.split_once('(')?.1.split(')').next()?;
    Some(inside.split(',').next()?.trim().to_string())
}

/// The newest sentinel already extracted into `dir` (one subdirectory per version).
fn newest_cached(dir: &Path) -> Option<(String, PathBuf)> {
    let binary = if cfg!(windows) { "sentinel.exe" } else { "sentinel" };
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let version = e.file_name().to_string_lossy().to_string();
            let path = e.path().join(binary);
            path.is_file().then_some((version, path))
        })
        .max_by_key(|(v, _)| version_key(v))
}

/// Asks the binary for its version over LSP; also proves it starts.
pub fn probe_version(command: &str) -> Result<String, String> {
    let tool = Tool { command: command.into(), args: vec!["lsp".into()] };
    let client = LspClient::start(&tool)?;
    client
        .server_version
        .clone()
        .ok_or_else(|| format!("`{command} lsp` did not report a version"))
}

/// Resolves the sentinel described by `s`, fetching or building it when needed.
pub fn resolve(
    s: &SentinelSettings,
    collection: &CollectionSettings,
    github: &GithubSettings,
    progress: &dyn Fn(String),
) -> Result<ResolvedSentinel, String> {
    match s.source {
        SentinelSource::Installed => {
            let version = probe_version(&s.command)?;
            Ok(ResolvedSentinel { command: s.command.clone(), version, origin: format!("installed `{}`", s.command) })
        }
        SentinelSource::Path => {
            if !Path::new(&s.path).is_file() {
                return Err(format!("sentinel.path {:?} is not a file", s.path));
            }
            let version = probe_version(&s.path)?;
            Ok(ResolvedSentinel { command: s.path.clone(), version, origin: format!("binary {}", s.path) })
        }
        SentinelSource::Rubygems => {
            progress(format!("asking rubygems for the latest {}", s.gem_name));
            let cache = cache_dir(collection).join("sentinel");
            let latest = match latest_gem_version(&collection.gem, &s.gem_name) {
                Ok(v) => v,
                // Offline (or rubygems is down): the newest one we already have is still a sentinel.
                Err(e) => match newest_cached(&cache) {
                    Some((version, _)) => {
                        progress(format!("rubygems unreachable ({e}); using cached {version}"));
                        version
                    }
                    None => return Err(e),
                },
            };
            let member = gem_binary_name(std::env::consts::ARCH, std::env::consts::OS)?;
            let dir = cache.join(&latest);
            let binary = dir.join(if cfg!(windows) { "sentinel.exe" } else { "sentinel" });
            if !binary.is_file() {
                progress(format!("downloading {} {latest}", s.gem_name));
                fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
                let out = Command::new(&collection.gem)
                    .args(["fetch", &s.gem_name, "--version", &latest])
                    .current_dir(&dir)
                    .output()
                    .map_err(|e| format!("failed to run `{} fetch`: {e}", collection.gem))?;
                let gem_file = dir.join(format!("{}-{latest}.gem", s.gem_name));
                if !out.status.success() || !gem_file.is_file() {
                    return Err(format!(
                        "gem fetch {} {latest} failed: {}",
                        s.gem_name,
                        String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").trim()
                    ));
                }
                extract_gem_binary(&gem_file, &member, &binary)?;
            }
            let command = binary.to_string_lossy().to_string();
            let version = probe_version(&command)?;
            Ok(ResolvedSentinel { command, version, origin: format!("rubygems {} {latest}", s.gem_name) })
        }
        SentinelSource::Git => {
            let dir = cache_dir(collection).join("sentinel-git");
            let dest = dir.to_string_lossy().to_string();
            if dir.join(".git").exists() {
                progress(format!("updating {} @ {}", s.git_url, s.git_ref));
                crate::github::git(github, &["fetch", "--quiet", "--depth", "1", "origin", &s.git_ref], Some(&dir))?;
                crate::github::git(github, &["reset", "--quiet", "--hard", "FETCH_HEAD"], Some(&dir))?;
            } else {
                progress(format!("cloning {} @ {}", s.git_url, s.git_ref));
                fs::create_dir_all(cache_dir(collection)).map_err(|e| e.to_string())?;
                crate::github::git(
                    github,
                    &["clone", "--quiet", "--depth", "1", "--branch", &s.git_ref, "--", &s.git_url, &dest],
                    None,
                )?;
            }
            let sha = crate::github::git(github, &["rev-parse", "--short=7", "HEAD"], Some(&dir))?;
            progress(format!("building sentinel {sha} (cargo build --release)"));
            let build = Command::new(&s.cargo)
                .args(["build", "--release"])
                .current_dir(&dir)
                .output()
                .map_err(|e| format!("failed to run `{} build`: {e}", s.cargo))?;
            if !build.status.success() {
                let err = String::from_utf8_lossy(&build.stderr);
                return Err(format!("cargo build failed: {}", err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("")));
            }
            let binary: PathBuf = dir.join("target/release").join(if cfg!(windows) { "sentinel-rb.exe" } else { "sentinel-rb" });
            let command = binary.to_string_lossy().to_string();
            let version = format!("{}+{sha}", probe_version(&command)?);
            Ok(ResolvedSentinel { command, version, origin: format!("git {} @ {}", s.git_url, s.git_ref) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_names_match_the_gem_layout() {
        assert_eq!(gem_binary_name("x86_64", "macos").unwrap(), "sentinel-x86_64-darwin");
        assert_eq!(gem_binary_name("aarch64", "macos").unwrap(), "sentinel-aarch64-darwin");
        assert_eq!(gem_binary_name("x86_64", "linux").unwrap(), "sentinel-x86_64-linux");
        assert_eq!(gem_binary_name("aarch64", "linux").unwrap(), "sentinel-aarch64-linux");
        assert_eq!(gem_binary_name("x86_64", "windows").unwrap(), "sentinel-x86_64-windows.exe");
        assert!(gem_binary_name("riscv64", "linux").is_err() && gem_binary_name("x86_64", "freebsd").is_err());
    }

    #[test]
    fn parses_the_newest_version_from_gem_list() {
        let out = "\n*** REMOTE GEMS ***\n\nrbs-sentinel (0.6.0, 0.5.0, 0.4.2)\n";
        assert_eq!(parse_gem_list_version(out, "rbs-sentinel").as_deref(), Some("0.6.0"));
        assert_eq!(parse_gem_list_version("rbs-sentinel (1.2.3)\n", "rbs-sentinel").as_deref(), Some("1.2.3"));
        assert_eq!(parse_gem_list_version("something-else (1.0)\n", "rbs-sentinel"), None);
        assert_eq!(parse_gem_list_version("", "rbs-sentinel"), None);
    }

    /// Builds a minimal `.gem` (a tar holding `data.tar.gz`) and pulls a binary out of it.
    #[test]
    fn extracts_a_platform_binary_from_a_gem() {
        if Command::new("tar").arg("--version").output().is_err() {
            return eprintln!("skipped: no tar");
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        fs::create_dir_all(root.join("exe")).unwrap();
        fs::write(root.join("exe/sentinel-x86_64-darwin"), "#!/bin/sh\necho mac\n").unwrap();
        fs::write(root.join("exe/sentinel-x86_64-linux"), "#!/bin/sh\necho linux\n").unwrap();
        let tar = |args: &[&str], cwd: &Path| {
            assert!(Command::new("tar").args(args).current_dir(cwd).status().unwrap().success());
        };
        tar(&["-czf", "../data.tar.gz", "exe"], &root);
        fs::write(dir.path().join("metadata.gz"), "x").unwrap();
        tar(&["-cf", "fake.gem", "data.tar.gz", "metadata.gz"], dir.path());

        let dest = dir.path().join("out/sentinel");
        extract_gem_binary(&dir.path().join("fake.gem"), "sentinel-x86_64-linux", &dest).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "#!/bin/sh\necho linux\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&dest).unwrap().permissions().mode() & 0o111, 0o111, "executable");
        }
        // A platform the gem doesn't ship is a clear error, and leaves nothing behind.
        let missing = dir.path().join("out/other");
        let err = extract_gem_binary(&dir.path().join("fake.gem"), "sentinel-aarch64-linux", &missing).unwrap_err();
        assert!(err.contains("sentinel-aarch64-linux"), "{err}");
        assert!(!missing.exists());
    }

    #[test]
    fn newest_cached_picks_the_highest_version_numerically() {
        let dir = tempfile::tempdir().unwrap();
        assert!(newest_cached(dir.path()).is_none());
        let bin = if cfg!(windows) { "sentinel.exe" } else { "sentinel" };
        for v in ["0.9.0", "0.10.1", "0.7.0"] {
            fs::create_dir_all(dir.path().join(v)).unwrap();
            fs::write(dir.path().join(v).join(bin), "x").unwrap();
        }
        // A version directory with no binary (an interrupted download) is ignored.
        fs::create_dir_all(dir.path().join("9.9.9")).unwrap();
        assert_eq!(newest_cached(dir.path()).unwrap().0, "0.10.1", "0.10.1 is newer than 0.9.0");
    }

    #[test]
    fn a_path_that_is_not_a_file_is_rejected() {
        let s = SentinelSettings { source: SentinelSource::Path, path: "/nonexistent/sentinel".into(), ..Default::default() };
        let err = resolve(&s, &CollectionSettings::default(), &GithubSettings::default(), &|_| {}).unwrap_err();
        assert!(err.contains("not a file"), "{err}");
    }
}
