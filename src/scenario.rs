//! Scenarios: a workflow of steps that turns "a sentinel and a source of Ruby" into
//! reviewed training pairs.
//!
//! A [`PairFactory`](crate::PairFactory) makes pairs from a seed. A [`Scenario`] is the
//! level above: a small dependency graph of [`Step`]s, each producing an artifact (a resolved
//! sentinel binary, the synced collection, the pairs, a [`Report`]). Steps whose
//! dependencies are met run in parallel, steps can wrap factories (generate, import,
//! compile), and the whole thing can be run headless or from the GUI.
//!
//! Steps never mutate shared state: each reads the [`Context`] and returns an [`Output`]
//! that the runner merges in afterwards, which is what makes running them concurrently safe.

use crate::{Pair, pair::Check, settings::Settings, sentinel_source::ResolvedSentinel};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    time::Instant,
};

/// The artifacts steps produce and later steps read.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub settings: Settings,
    pub sentinel: Option<ResolvedSentinel>,
    pub collection: Option<CollectionInfo>,
    pub pairs: Vec<Pair>,
    pub report: Option<Report>,
    pub diff: Option<Diff>,
}

impl Context {
    pub fn new(settings: Settings) -> Self {
        Self { settings, ..Default::default() }
    }

    /// The settings with the resolved sentinel substituted for the configured command, so
    /// everything built from them (factories, compile) runs the sentinel under test.
    pub fn effective_settings(&self) -> Settings {
        let mut s = self.settings.clone();
        if let Some(r) = &self.sentinel {
            s.sentinel.command = r.command.clone();
            s.lsp.command = r.command.clone();
        }
        s
    }
}

/// A synced checkout of the signature collection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectionInfo {
    pub root: PathBuf,
    pub commit: String,
    pub gems: usize,
}

/// What a step contributes to the [`Context`].
pub enum Output {
    Sentinel(ResolvedSentinel),
    Collection(CollectionInfo),
    /// Replaces the pairs (a step that transforms them returns the whole set).
    Pairs(Vec<Pair>),
    Report { report: Report, diff: Option<Diff> },
}

/// One node of a scenario.
pub trait Step: Send + Sync {
    /// Unique within the scenario.
    fn id(&self) -> &'static str;
    fn title(&self) -> String;
    /// Ids of the steps that must finish first.
    fn needs(&self) -> &'static [&'static str] {
        &[]
    }
    /// Do the work. `progress` reports what is happening; the returned string is a one-line
    /// summary shown when the step finishes.
    fn run(&self, ctx: &Context, progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String>;
}

/// A named workflow.
pub struct Scenario {
    pub id: &'static str,
    pub title: String,
    pub description: String,
    pub steps: Vec<Box<dyn Step>>,
}

/// Progress reported while a scenario runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started { step: String, title: String },
    Progress { step: String, text: String },
    Finished { step: String, summary: String, millis: u128 },
    Failed { step: String, error: String },
}

/// A scenario that stopped, and where.
#[derive(Debug)]
pub struct RunError {
    pub step: String,
    pub error: String,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "step `{}` failed: {}", self.step, self.error)
    }
}

impl Scenario {
    /// Checks that step ids are unique, every dependency exists, and there is no cycle.
    pub fn validate(&self) -> Result<(), String> {
        let ids: HashSet<&str> = self.steps.iter().map(|s| s.id()).collect();
        if ids.len() != self.steps.len() {
            return Err("duplicate step id".into());
        }
        for s in &self.steps {
            for n in s.needs() {
                if !ids.contains(n) {
                    return Err(format!("step `{}` needs unknown step `{n}`", s.id()));
                }
            }
        }
        self.levels().map(|_| ())
    }

    /// Steps grouped into waves; everything in a wave can run at the same time.
    fn levels(&self) -> Result<Vec<Vec<usize>>, String> {
        let mut done: HashSet<&str> = HashSet::new();
        let mut remaining: Vec<usize> = (0..self.steps.len()).collect();
        let mut levels = Vec::new();
        while !remaining.is_empty() {
            let ready: Vec<usize> = remaining
                .iter()
                .copied()
                .filter(|&i| self.steps[i].needs().iter().all(|n| done.contains(n)))
                .collect();
            if ready.is_empty() {
                return Err("the steps contain a cycle".into());
            }
            for &i in &ready {
                done.insert(self.steps[i].id());
            }
            remaining.retain(|i| !ready.contains(i));
            levels.push(ready);
        }
        Ok(levels)
    }

    /// Runs every step, wave by wave, and returns the final context. Steps in a wave run
    /// concurrently; if any fails the run stops after that wave.
    pub fn run(&self, mut ctx: Context, on_event: &(dyn Fn(Event) + Sync)) -> Result<Context, RunError> {
        self.validate().map_err(|error| RunError { step: self.id.into(), error })?;
        let levels = self.levels().expect("validated");
        for level in levels {
            let results: Vec<(usize, Result<(Output, String), String>, u128)> = std::thread::scope(|scope| {
                let handles: Vec<_> = level
                    .iter()
                    .map(|&i| {
                        let (step, ctx_ref) = (&self.steps[i], &ctx);
                        scope.spawn(move || {
                            let id = step.id().to_string();
                            on_event(Event::Started { step: id.clone(), title: step.title() });
                            let started = Instant::now();
                            let progress = |text: String| on_event(Event::Progress { step: id.clone(), text });
                            let result = step.run(ctx_ref, &progress);
                            (i, result, started.elapsed().as_millis())
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().expect("step panicked")).collect()
            });

            let mut failure = None;
            for (i, result, millis) in results {
                let id = self.steps[i].id().to_string();
                match result {
                    Ok((output, summary)) => {
                        on_event(Event::Finished { step: id, summary, millis });
                        match output {
                            Output::Sentinel(s) => ctx.sentinel = Some(s),
                            Output::Collection(c) => ctx.collection = Some(c),
                            Output::Pairs(p) => ctx.pairs = p,
                            Output::Report { report, diff } => {
                                ctx.report = Some(report);
                                ctx.diff = diff;
                            }
                        }
                    }
                    Err(error) => {
                        on_event(Event::Failed { step: id.clone(), error: error.clone() });
                        failure.get_or_insert(RunError { step: id, error });
                    }
                }
            }
            if let Some(f) = failure {
                return Err(f);
            }
        }
        Ok(ctx)
    }
}

// ---- reports and diffs ------------------------------------------------------------

/// One pair's outcome in a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// `match`, `mismatch`, or `n/a` (no ground truth).
    pub verdict: String,
    /// Identifies *what* mismatched, so a mismatch that changes is noticed.
    pub detail: u64,
    /// Fingerprint of the pair's input, so the same id over *different* text (a file that
    /// was split differently, say) is not mistaken for the same pair. 0 means unknown, as in
    /// reports written before this field existed.
    #[serde(default)]
    pub src: u64,
}

/// The outcome of a scenario run, kept so the next run can be compared with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub scenario: String,
    /// The sentinel version, plus where it came from.
    pub sentinel: String,
    /// Unix seconds.
    pub at: u64,
    pub total: usize,
    pub matched: usize,
    pub mismatched: usize,
    pub unchecked: usize,
    pub entries: BTreeMap<String, Entry>,
}

impl Report {
    pub fn from_pairs(scenario: &str, sentinel: &str, pairs: &[Pair]) -> Self {
        let mut entries = BTreeMap::new();
        let (mut matched, mut mismatched, mut unchecked) = (0, 0, 0);
        for p in pairs {
            let src = crate::rng::hash64(&p.input);
            let entry = match p.check() {
                Check::Match => {
                    matched += 1;
                    Entry { verdict: "match".into(), detail: 0, src }
                }
                Check::Mismatch(lines) => {
                    mismatched += 1;
                    Entry { verdict: "mismatch".into(), detail: crate::rng::hash64(&lines.join("\n")), src }
                }
                Check::NotApplicable | Check::Pending => {
                    unchecked += 1;
                    Entry { verdict: "n/a".into(), detail: 0, src }
                }
            };
            entries.insert(p.id.clone(), entry);
        }
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Report { scenario: scenario.into(), sentinel: sentinel.into(), at, total: pairs.len(), matched, mismatched, unchecked, entries }
    }
}

/// What changed between two runs of a scenario: the "sentinel diffs" worth an alert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub previous_sentinel: String,
    pub sentinel: String,
    /// Mismatched before, matching now.
    pub fixed: Vec<String>,
    /// Matched before, mismatching now. The ones that need attention.
    pub regressed: Vec<String>,
    /// Mismatched both times, but not for the same reason.
    pub changed: Vec<String>,
    pub added: usize,
    pub removed: usize,
}

impl Diff {
    /// True if sentinel behaved differently in a way a human should look at.
    pub fn is_alarming(&self) -> bool {
        !self.regressed.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.fixed.is_empty() && self.regressed.is_empty() && self.changed.is_empty()
    }

    /// One line for the status area.
    pub fn headline(&self) -> String {
        let versions = if self.previous_sentinel == self.sentinel {
            format!("sentinel {}", self.sentinel)
        } else {
            format!("sentinel {} → {}", self.previous_sentinel, self.sentinel)
        };
        format!(
            "{versions}: {} fixed, {} regressed, {} changed mismatch{}",
            self.fixed.len(),
            self.regressed.len(),
            self.changed.len(),
            if self.added + self.removed > 0 { format!(" ({} new pairs, {} gone)", self.added, self.removed) } else { String::new() }
        )
    }
}

/// Compares `now` with the `previous` run of the same scenario.
pub fn diff(previous: &Report, now: &Report) -> Diff {
    let mut d = Diff { previous_sentinel: previous.sentinel.clone(), sentinel: now.sentinel.clone(), ..Default::default() };
    let mut replaced = 0;
    for (id, new) in &now.entries {
        match previous.entries.get(id) {
            None => d.added += 1,
            // Same id, different text: a different pair that happens to share a name.
            Some(old) if old.src != 0 && new.src != 0 && old.src != new.src => {
                d.added += 1;
                replaced += 1;
            }
            Some(old) => match (old.verdict.as_str(), new.verdict.as_str()) {
                ("mismatch", "match") => d.fixed.push(id.clone()),
                ("match", "mismatch") => d.regressed.push(id.clone()),
                ("mismatch", "mismatch") if old.detail != new.detail => d.changed.push(id.clone()),
                _ => {}
            },
        }
    }
    d.removed = previous.entries.keys().filter(|id| !now.entries.contains_key(*id)).count() + replaced;
    d
}

/// Where a scenario's last report is kept.
pub fn report_path(cache: &std::path::Path, scenario: &str) -> PathBuf {
    cache.join("scenarios").join(scenario).join("report.json")
}

pub fn load_report(path: &std::path::Path) -> Option<Report> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub fn save_report(path: &std::path::Path, report: &Report) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(report).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A step that records when it ran and returns a canned output.
    struct Fake {
        id: &'static str,
        needs: &'static [&'static str],
        fail: bool,
        log: &'static Mutex<Vec<String>>,
    }

    impl Step for Fake {
        fn id(&self) -> &'static str {
            self.id
        }
        fn title(&self) -> String {
            format!("step {}", self.id)
        }
        fn needs(&self) -> &'static [&'static str] {
            self.needs
        }
        fn run(&self, ctx: &Context, progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
            self.log.lock().unwrap().push(format!("{} sees {} pairs", self.id, ctx.pairs.len()));
            progress("working".into());
            if self.fail {
                return Err("boom".into());
            }
            let output = match self.id {
                "pairs" => Output::Pairs(vec![pair("a"), pair("b")]),
                _ => Output::Collection(CollectionInfo { root: PathBuf::from("/x"), commit: self.id.into(), gems: 1 }),
            };
            Ok((output, "ok".into()))
        }
    }

    fn pair(id: &str) -> Pair {
        Pair {
            id: id.into(), factory: "f".into(), seed: 0, instruction: String::new(), input: String::new(),
            output: String::new(), expected: String::new(), compiled: String::new(), compile_error: None, diagnostics: vec![], generated: 0,
        }
    }

    fn scenario(steps: Vec<Box<dyn Step>>) -> Scenario {
        Scenario { id: "t", title: "t".into(), description: String::new(), steps }
    }

    #[test]
    fn steps_run_after_what_they_need_and_see_its_output() {
        static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let s = scenario(vec![
            Box::new(Fake { id: "later", needs: &["pairs"], fail: false, log: &LOG }),
            Box::new(Fake { id: "pairs", needs: &[], fail: false, log: &LOG }),
        ]);
        let events = Mutex::new(Vec::new());
        let ctx = s.run(Context::default(), &|e| events.lock().unwrap().push(e)).unwrap();
        assert_eq!(ctx.pairs.len(), 2);
        let log = LOG.lock().unwrap().clone();
        assert_eq!(log, ["pairs sees 0 pairs", "later sees 2 pairs"], "dependency order, not declaration order");
        let events = events.into_inner().unwrap();
        assert!(events.contains(&Event::Progress { step: "pairs".into(), text: "working".into() }));
        assert!(events.iter().any(|e| matches!(e, Event::Finished { step, .. } if step == "later")));
    }

    #[test]
    fn independent_steps_share_a_wave() {
        static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let s = scenario(vec![
            Box::new(Fake { id: "a", needs: &[], fail: false, log: &LOG }),
            Box::new(Fake { id: "b", needs: &[], fail: false, log: &LOG }),
            Box::new(Fake { id: "c", needs: &["a", "b"], fail: false, log: &LOG }),
        ]);
        assert_eq!(s.levels().unwrap(), vec![vec![0, 1], vec![2]]);
    }

    #[test]
    fn a_failure_stops_the_run_and_is_reported() {
        static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let s = scenario(vec![
            Box::new(Fake { id: "pairs", needs: &[], fail: true, log: &LOG }),
            Box::new(Fake { id: "later", needs: &["pairs"], fail: false, log: &LOG }),
        ]);
        let events = Mutex::new(Vec::new());
        let err = s.run(Context::default(), &|e| events.lock().unwrap().push(e)).unwrap_err();
        assert_eq!((err.step.as_str(), err.error.as_str()), ("pairs", "boom"));
        assert!(!LOG.lock().unwrap().iter().any(|l| l.starts_with("later")), "dependent step must not run");
        assert!(events.lock().unwrap().contains(&Event::Failed { step: "pairs".into(), error: "boom".into() }));
    }

    #[test]
    fn bad_graphs_are_rejected() {
        static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let unknown = scenario(vec![Box::new(Fake { id: "a", needs: &["ghost"], fail: false, log: &LOG })]);
        assert!(unknown.validate().unwrap_err().contains("unknown step `ghost`"));
        let cycle = scenario(vec![
            Box::new(Fake { id: "a", needs: &["b"], fail: false, log: &LOG }),
            Box::new(Fake { id: "b", needs: &["a"], fail: false, log: &LOG }),
        ]);
        assert!(cycle.validate().unwrap_err().contains("cycle"));
        let dup = scenario(vec![
            Box::new(Fake { id: "a", needs: &[], fail: false, log: &LOG }),
            Box::new(Fake { id: "a", needs: &[], fail: false, log: &LOG }),
        ]);
        assert!(dup.validate().unwrap_err().contains("duplicate"));
    }

    fn report(sentinel: &str, entries: &[(&str, &str, u64)]) -> Report {
        Report {
            scenario: "s".into(), sentinel: sentinel.into(), at: 0, total: entries.len(), matched: 0, mismatched: 0, unchecked: 0,
            entries: entries.iter().map(|(id, v, d)| (id.to_string(), Entry { verdict: v.to_string(), detail: *d, src: 0 })).collect(),
        }
    }

    #[test]
    fn diff_finds_fixes_regressions_and_changed_mismatches() {
        let before = report("0.6.0", &[("fixed", "mismatch", 1), ("regressed", "match", 0), ("same", "mismatch", 7), ("moved", "mismatch", 5), ("ok", "match", 0), ("gone", "match", 0)]);
        let now = report("0.7.0", &[("fixed", "match", 0), ("regressed", "mismatch", 9), ("same", "mismatch", 7), ("moved", "mismatch", 6), ("ok", "match", 0), ("new", "match", 0)]);
        let d = diff(&before, &now);
        assert_eq!(d.fixed, ["fixed"]);
        assert_eq!(d.regressed, ["regressed"]);
        assert_eq!(d.changed, ["moved"]);
        assert_eq!((d.added, d.removed), (1, 1));
        assert!(d.is_alarming() && !d.is_empty());
        assert_eq!(d.headline(), "sentinel 0.6.0 → 0.7.0: 1 fixed, 1 regressed, 1 changed mismatch (1 new pairs, 1 gone)");
        // Identical runs are quiet.
        let quiet = diff(&now, &now);
        assert!(quiet.is_empty() && !quiet.is_alarming());
        assert_eq!(quiet.headline(), "sentinel 0.7.0: 0 fixed, 0 regressed, 0 changed mismatch");
    }

    #[test]
    fn the_same_id_over_different_text_is_a_new_pair_not_a_regression() {
        let entry = |verdict: &str, src: u64| Entry { verdict: verdict.into(), detail: 1, src };
        let mut before = report("0.7.0", &[]);
        before.entries.insert("a#3".into(), entry("match", 111));
        before.entries.insert("b".into(), entry("match", 222));
        let mut now = report("0.7.0", &[]);
        now.entries.insert("a#3".into(), entry("mismatch", 999)); // re-split: different slice, same name
        now.entries.insert("b".into(), entry("mismatch", 222)); // genuinely regressed
        let d = diff(&before, &now);
        assert_eq!(d.regressed, ["b"], "only the pair whose text is unchanged regressed");
        assert_eq!((d.added, d.removed), (1, 1), "the re-split slice counts as one gone, one new");

        // Reports saved before the fingerprint existed (src = 0) still compare by id alone.
        let mut legacy = report("0.6.0", &[]);
        legacy.entries.insert("a#3".into(), entry("match", 0));
        assert_eq!(diff(&legacy, &now).regressed, ["a#3"]);
    }

    #[test]
    fn report_round_trips_and_summarises_pairs() {
        let mut matching = pair("m");
        matching.expected = "class A\n  def f: () -> void\nend\n".into();
        matching.compiled = "class A\n  def f: () -> void\nend\n".into();
        let mut failing = pair("x");
        failing.expected = "class A\n  def f: () -> void\nend\n".into();
        failing.compiled = "class A\nend\n".into();
        let plain = pair("synthetic");
        let r = Report::from_pairs("s", "0.7.0", &[matching, failing, plain]);
        assert_eq!((r.total, r.matched, r.mismatched, r.unchecked), (3, 1, 1, 1));
        assert_eq!(r.entries["m"].verdict, "match");
        assert_eq!(r.entries["x"].verdict, "mismatch");
        assert_ne!(r.entries["x"].detail, 0);

        let dir = tempfile::tempdir().unwrap();
        let path = report_path(dir.path(), "s");
        assert!(load_report(&path).is_none());
        save_report(&path, &r).unwrap();
        assert_eq!(load_report(&path).unwrap(), r);
    }
}
