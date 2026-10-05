//! Asking Ruby whether a source parses, without paying for a Ruby start-up each time.
//!
//! `ruby -c` costs a whole process launch per check (tens of milliseconds, more under a
//! version manager), which adds up over thousands of hunks. Instead a worker process is kept
//! **warm**: it reads one JSON request per line on stdin, parses the source (with Prism, which
//! ships with Ruby 3.3+; older Rubies fall back to compiling it), and answers one JSON line.
//! Each thread keeps its own worker, so parallel imports don't queue behind each other, and
//! the worker is killed when the thread ends.

use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

/// What Ruby objected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    /// 1-based line of the first error (0 if unknown).
    pub line: u32,
    pub message: String,
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// The program the worker runs. One request per line: `{"s": "<ruby source>"}`.
const SCRIPT: &str = r#"
require "json"
$stdout.sync = true
prism = begin; require "prism"; true; rescue LoadError; false; end
STDIN.each_line do |line|
  src = JSON.parse(line)["s"]
  if prism
    e = Prism.parse(src).errors.first
    puts(e ? JSON.generate({ok: false, line: e.location.start_line, msg: e.message}) : '{"ok":true}')
  else
    begin
      RubyVM::InstructionSequence.compile(src)
      puts '{"ok":true}'
    rescue SyntaxError => e
      first = e.message.lines.first.to_s.strip
      puts JSON.generate({ok: false, line: first[/:(\d+):/, 1].to_i, msg: first})
    end
  end
end
"#;

/// A running worker.
pub struct RubySyntax {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl RubySyntax {
    /// Starts `ruby` with the worker script and checks it answers.
    pub fn start(ruby: &str) -> Result<Self, String> {
        let mut child = Command::new(ruby)
            .args(["-e", SCRIPT])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to run `{ruby}`: {e}"))?;
        let (stdin, stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
        let mut worker = Self { child, stdin, stdout: BufReader::new(stdout) };
        // A version manager with no usable Ruby starts the process but never answers.
        match worker.check("1") {
            Ok(Ok(())) => Ok(worker),
            other => Err(format!("`{ruby}` did not start a syntax worker: {other:?}")),
        }
    }

    /// `Ok(Ok(()))` if `source` parses, `Ok(Err(..))` with the first error if it doesn't,
    /// and `Err` if the worker itself failed.
    pub fn check(&mut self, source: &str) -> Result<Result<(), SyntaxError>, String> {
        let request = json!({ "s": source }).to_string();
        writeln!(self.stdin, "{request}")
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("syntax worker write failed: {e}"))?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line).map_err(|e| format!("syntax worker read failed: {e}"))? == 0 {
            return Err("syntax worker exited".into());
        }
        let reply: Value = serde_json::from_str(&line).map_err(|e| format!("bad reply {line:?}: {e}"))?;
        if reply["ok"].as_bool() == Some(true) {
            Ok(Ok(()))
        } else {
            Ok(Err(SyntaxError {
                line: reply["line"].as_u64().unwrap_or(0) as u32,
                message: reply["msg"].as_str().unwrap_or("syntax error").to_string(),
            }))
        }
    }
}

impl Drop for RubySyntax {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

thread_local! {
    /// This thread's worker, with the command it was started for. `None` inside means
    /// that command could not start one, which is remembered so it isn't retried per call.
    static WORKER: RefCell<Option<(String, Option<RubySyntax>)>> = const { RefCell::new(None) };
}

/// Checks `source` with this thread's warm worker for `ruby`, starting it on first use.
/// `None` means no worker is available (no usable Ruby), so the source could not be checked.
pub fn check_source(ruby: &str, source: &str) -> Option<Result<(), SyntaxError>> {
    WORKER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref().is_none_or(|(cmd, _)| cmd != ruby) {
            *slot = Some((ruby.to_string(), RubySyntax::start(ruby).ok()));
        }
        let (_, worker) = slot.as_mut().unwrap();
        let result = worker.as_mut()?.check(source);
        match result {
            Ok(r) => Some(r),
            Err(_) => {
                // The worker died mid-run: start a fresh one and retry once.
                *worker = RubySyntax::start(ruby).ok();
                worker.as_mut()?.check(source).ok()
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ruby_available() -> bool {
        Command::new("ruby").arg("-v").output().is_ok_and(|o| o.status.success())
    }

    #[test]
    fn reports_valid_and_invalid_sources() {
        if !ruby_available() {
            return eprintln!("skipped: no ruby");
        }
        let mut w = RubySyntax::start("ruby").unwrap();
        assert_eq!(w.check("class A\n  def f; end\nend\n").unwrap(), Ok(()));
        let err = w.check("module A\n  module B\n    def f; end\n  end\n").unwrap().unwrap_err();
        assert!(err.line >= 4, "the error is where the missing end was noticed: {err}");
        // The worker survives errors, and handles awkward input (newlines, quotes, unicode, empty).
        assert_eq!(w.check("puts \"é\\n\"\n# \"quoted\"\n").unwrap(), Ok(()));
        assert_eq!(w.check("").unwrap(), Ok(()));
        assert!(w.check("def f(a, b; end").unwrap().is_err());
        assert_eq!(w.check("x = 1\n").unwrap(), Ok(()), "still answering after errors");
    }

    #[test]
    fn a_missing_ruby_is_reported_not_panicked() {
        assert!(RubySyntax::start("/nonexistent/ruby").is_err());
        assert!(check_source("/nonexistent/ruby", "1").is_none());
        // And not retried into a different answer.
        assert!(check_source("/nonexistent/ruby", "1").is_none());
    }

    #[test]
    fn the_thread_local_worker_is_reused_and_checks_are_fast() {
        if !ruby_available() {
            return eprintln!("skipped: no ruby");
        }
        let sample = "module A\n  class B\n    def f(x)\n      x + 1\n    end\n  end\nend\n";
        let started = std::time::Instant::now();
        for _ in 0..500 {
            assert_eq!(check_source("ruby", sample), Some(Ok(())));
        }
        // 500 cold `ruby -c` runs would take many seconds; a warm worker takes a fraction of one.
        assert!(started.elapsed().as_secs_f64() < 3.0, "{:?}", started.elapsed());
        assert!(check_source("ruby", "def (").unwrap().is_err());
    }

    /// Dev aid: `BENCH_JSONL=pairs.jsonl cargo test --lib bench -- --ignored --nocapture`
    /// times cold `ruby -c` against the warm worker on a dataset's outputs.
    #[test]
    #[ignore]
    fn bench_cold_versus_warm() {
        let Ok(path) = std::env::var("BENCH_JSONL") else { return };
        let store = crate::Store::open(path).unwrap();
        let sources: Vec<&str> = store.pairs.iter().map(|p| p.output.as_str()).collect();

        let started = std::time::Instant::now();
        let warm_invalid = sources.iter().filter(|s| check_source("ruby", s).is_some_and(|r| r.is_err())).count();
        let warm = started.elapsed();

        let cold_n = 60.min(sources.len());
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        for (i, src) in sources.iter().take(cold_n).enumerate() {
            let f = dir.path().join(format!("{i}.rb"));
            std::fs::write(&f, src).unwrap();
            let _ = Command::new("ruby").arg("-c").arg(&f).output();
        }
        let cold = started.elapsed();
        println!(
            "warm: {} sources in {warm:?} ({:.2} ms each, {warm_invalid} invalid)\ncold `ruby -c`: {cold_n} in {cold:?} ({:.0} ms each) -> all {} would take ~{:.0}s",
            sources.len(),
            warm.as_secs_f64() * 1000.0 / sources.len() as f64,
            cold.as_secs_f64() * 1000.0 / cold_n as f64,
            sources.len(),
            cold.as_secs_f64() / cold_n as f64 * sources.len() as f64,
        );
    }
}
