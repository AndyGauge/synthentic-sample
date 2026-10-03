//! Runs the `sentinel` transpiler (rbs-sentinel gem) over a Ruby source string.
//!
//! Sentinel is directory based: `sentinel init` transpiles `./app/**/*.rb` into
//! `./sig/generated/**/*.rbs` relative to the working directory, so each call gets
//! a throwaway project directory. The command comes from the settings file.

use crate::settings::Tool;
use std::{fs, process::Command};

pub fn transpile(tool: &Tool, source: &str) -> Result<String, String> {
    let bin = &tool.command;
    let dir = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    fs::create_dir(dir.path().join("app")).map_err(|e| e.to_string())?;
    fs::write(dir.path().join("app/pair.rb"), source).map_err(|e| e.to_string())?;

    let out = Command::new(&bin)
        .args(&tool.args)
        .current_dir(dir.path())
        .output()
        .map_err(|e| format!("failed to run `{bin} {}`: {e}", tool.args.join(" ")))?;
    let log = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.status.success() {
        return Err(format!("`{bin} {}` exited with {}\n{log}", tool.args.join(" "), out.status));
    }
    match fs::read_to_string(dir.path().join("sig/generated/pair.rbs")) {
        Ok(rbs) => Ok(rbs),
        Err(_) => Err(format!("sentinel produced no pair.rbs\n{log}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the `sentinel` gem on PATH; skipped silently when absent.
    #[test]
    fn transpiles_method_signature() {
        let src = "class A\n  #: (Integer) -> String\n  def f(x); end\nend\n";
        match transpile(&Tool { command: "sentinel".into(), args: vec!["init".into()] }, src) {
            Ok(rbs) => assert!(rbs.contains("def f: (Integer) -> String"), "{rbs}"),
            Err(e) if e.contains("failed to run") => eprintln!("skipped: {e}"),
            Err(e) => panic!("{e}"),
        }
    }
}
