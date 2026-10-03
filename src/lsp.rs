//! Minimal stdio LSP client: just enough to open a document and collect the
//! `publishDiagnostics` the server sends back.

use crate::{
    pair::{Compiled, Diagnostic},
    settings::Tool,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{Receiver, RecvTimeoutError, channel},
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(5);

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    root: tempfile::TempDir,
    root_path: PathBuf,
    counter: u64,
    next_id: u64,
}

impl LspClient {
    /// Spawns the server in a throwaway project dir and performs the handshake.
    pub fn start(tool: &Tool) -> Result<Self, String> {
        let root = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
        let root_path = root.path().canonicalize().map_err(|e| e.to_string())?;
        std::fs::create_dir(root_path.join("app")).map_err(|e| e.to_string())?;
        let mut child = Command::new(&tool.command)
            .args(&tool.args)
            .current_dir(&root_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to run `{} {}`: {e}", tool.command, tool.args.join(" ")))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Some(msg) = read_frame(&mut r) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let mut c = Self { child, stdin, rx, root, root_path, counter: 0, next_id: 1 };
        c.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "processId": null, "rootUri": file_uri(&c.root_path), "capabilities": {}}}))?;
        c.wait(|m| m["id"] == 1)?;
        c.send(json!({"jsonrpc":"2.0","method":"initialized","params":{}}))?;
        Ok(c)
    }

    /// `sentinel/transpile`: transpile `source` in memory, with no files involved.
    /// Fails with a `-32601` error on servers that predate the request.
    pub fn transpile(&mut self, source: &str) -> Result<Compiled, String> {
        let result = self.request("sentinel/transpile", json!({ "text": source }))?;
        Ok(Compiled {
            output: result["rbs"].as_str().ok_or("sentinel/transpile: no rbs in reply")?.into(),
            diagnostics: result["diagnostics"]
                .as_array()
                .map(|a| a.iter().map(to_diagnostic).collect())
                .unwrap_or_default(),
        })
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        let reply = self.wait(|m| m["id"] == id)?;
        match reply.get("error") {
            Some(e) => Err(format!("lsp error {}: {}", e["code"], e["message"].as_str().unwrap_or(""))),
            None => Ok(reply["result"].clone()),
        }
    }

    /// For servers without `sentinel/transpile`: opens `source` as a fresh
    /// document (written to the temp project, since those servers read from
    /// disk) and returns the server's diagnostics for it.
    pub fn diagnose(&mut self, source: &str) -> Result<Vec<Diagnostic>, String> {
        self.counter += 1;
        let name = format!("check_{}.rb", self.counter);
        // The server reads the file from disk, so the text must exist there too.
        let file = self.root_path.join("app").join(&name);
        std::fs::write(&file, source).map_err(|e| format!("write {name}: {e}"))?;
        let uri = file_uri(&file);
        let result = self.open_and_wait(&uri, &name, source);
        let _ = std::fs::remove_file(&file);
        result
    }

    fn open_and_wait(&mut self, uri: &str, name: &str, source: &str) -> Result<Vec<Diagnostic>, String> {
        self.send(json!({"jsonrpc":"2.0","method":"textDocument/didOpen","params":{
            "textDocument": {"uri": uri, "languageId": "ruby", "version": 1, "text": source}}}))?;
        let msg = self.wait(|m| {
            m["method"] == "textDocument/publishDiagnostics"
                && m["params"]["uri"].as_str().is_some_and(|u| u.ends_with(name))
        })?;
        let _ = self.send(json!({"jsonrpc":"2.0","method":"textDocument/didClose","params":{
            "textDocument": {"uri": uri}}}));
        Ok(msg["params"]["diagnostics"]
            .as_array()
            .map(|a| a.iter().map(to_diagnostic).collect())
            .unwrap_or_default())
    }

    fn send(&mut self, msg: Value) -> Result<(), String> {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| format!("lsp write failed: {e}"))
    }

    /// Waits for the first message matching `pred`, discarding others.
    fn wait(&self, pred: impl Fn(&Value) -> bool) -> Result<Value, String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok(m) if pred(&m) => return Ok(m),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Err("lsp timed out".into()),
                Err(RecvTimeoutError::Disconnected) => return Err("lsp server exited".into()),
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = &self.root; // keep the temp dir alive until the process is gone
    }
}

fn file_uri(p: &std::path::Path) -> String {
    format!("file://{}", p.display())
}

fn to_diagnostic(d: &Value) -> Diagnostic {
    Diagnostic {
        line: d["range"]["start"]["line"].as_u64().unwrap_or(0) as u32 + 1,
        severity: match d["severity"].as_u64() {
            Some(1) => "error",
            Some(3) => "info",
            Some(4) => "hint",
            _ => "warning",
        }
        .into(),
        message: d["message"].as_str().unwrap_or("").into(),
    }
}

fn read_frame(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse::<usize>().ok();
        }
    }
    let mut buf = vec![0; len?];
    r.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_parse() {
        let body = r#"{"id":1}"#;
        let raw = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        assert_eq!(read_frame(&mut raw.as_bytes()).unwrap()["id"], 1);
    }

    /// Needs a sentinel with `sentinel/transpile` (set `SENTINEL_BIN`); skipped otherwise.
    #[test]
    fn transpile_in_memory() {
        let command = std::env::var("SENTINEL_BIN").unwrap_or_else(|_| "sentinel".into());
        let tool = Tool { command, args: vec!["lsp".into()] };
        let mut c = match LspClient::start(&tool) {
            Ok(c) => c,
            Err(e) => return eprintln!("skipped: {e}"),
        };
        let bad = "class B\n  #: (Integer) -> oops(\n  def bad(x); end\n\n  #: (Integer) -> String\n  def ok(x); end\nend\n";
        match c.transpile(bad) {
            Ok(out) => {
                assert!(out.output.contains("def ok: (Integer) -> String"), "{}", out.output);
                assert!(!out.output.contains("def bad"));
                assert_eq!(out.diagnostics.len(), 1);
                assert_eq!(out.diagnostics[0].line, 3);
                // Nothing was written anywhere.
                assert!(!c.root_path.join("sig").exists());
            }
            Err(e) if e.contains("-32601") => eprintln!("skipped: server lacks sentinel/transpile"),
            Err(e) => panic!("{e}"),
        }
    }

    /// Needs `sentinel lsp` on PATH; skipped silently when absent.
    #[test]
    fn reports_malformed_signature() {
        let tool = Tool { command: "sentinel".into(), args: vec!["lsp".into()] };
        let mut c = match LspClient::start(&tool) {
            Ok(c) => c,
            Err(e) => return eprintln!("skipped: {e}"),
        };
        let bad = "class B\n  #: (Integer) -> oops(\n  def bad(x); end\nend\n";
        let d = c.diagnose(bad).unwrap();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].line, 3);
        assert!(c.diagnose("class B\nend\n").unwrap().is_empty());
    }
}
