//! The built-in scenarios and their steps.
//!
//! * **gem-rbs-collection**: fetch a sentinel and sync the signature collection (in
//!   parallel), reverse-compile every gem's signatures into annotated Ruby, compile each
//!   with that sentinel, then check the results against the source signatures and compare
//!   with the previous run.
//! * **synthetic**: the same, but the pairs come from the seeded generator.

use crate::{
    Pair, PairFactory, collection,
    registry,
    scenario::{CollectionInfo, Context, Output, Report, Scenario, Step, diff, load_report, report_path, save_report},
    sentinel_source,
};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

/// Every built-in scenario.
pub fn all() -> Vec<Scenario> {
    vec![gem_rbs_collection(), synthetic()]
}

pub fn find(id: &str) -> Option<Scenario> {
    all().into_iter().find(|s| s.id == id)
}

pub fn gem_rbs_collection() -> Scenario {
    Scenario {
        id: "gem-rbs-collection",
        title: "gem_rbs_collection → sentinel".into(),
        description: "Fetches the latest sentinel and syncs ruby/gem_rbs_collection, rebuilds each gem's Ruby with \
                      its signatures as inline RBS, compiles every pair with that sentinel, and flags where the \
                      result differs from the signatures (and from the previous run)."
            .into(),
        steps: vec![
            Box::new(SentinelStep),
            Box::new(CollectionStep),
            Box::new(GemPairsStep),
            Box::new(CompileStep { needs: &["sentinel", "pairs"] }),
            Box::new(CheckStep { scenario: "gem-rbs-collection" }),
        ],
    }
}

pub fn synthetic() -> Scenario {
    Scenario {
        id: "synthetic",
        title: "synthetic Ruby/RBS → sentinel".into(),
        description: "Generates seeded Ruby/RBS pairs, compiles them with the latest sentinel, and compares with the \
                      previous run. Synthetic pairs have no source signatures, so they are never flagged as mismatches."
            .into(),
        steps: vec![
            Box::new(SentinelStep),
            Box::new(GeneratePairsStep),
            Box::new(CompileStep { needs: &["sentinel", "pairs"] }),
            Box::new(CheckStep { scenario: "synthetic" }),
        ],
    }
}

fn factory_for(ctx: &Context, id: &str) -> Result<std::sync::Arc<dyn PairFactory>, String> {
    registry(&ctx.effective_settings())
        .into_iter()
        .map(std::sync::Arc::from)
        .find(|f: &std::sync::Arc<dyn PairFactory>| f.id() == id)
        .ok_or_else(|| format!("no factory `{id}`"))
}

/// Resolve the sentinel under test (rubygems, git, a path, or the installed one).
pub struct SentinelStep;

impl Step for SentinelStep {
    fn id(&self) -> &'static str {
        "sentinel"
    }
    fn title(&self) -> String {
        "Fetch sentinel".into()
    }
    fn run(&self, ctx: &Context, progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
        let s = &ctx.settings;
        let resolved = sentinel_source::resolve(&s.sentinel, &s.collection, &s.github, progress)?;
        let summary = format!("sentinel {} from {}", resolved.version, resolved.origin);
        Ok((Output::Sentinel(resolved), summary))
    }
}

/// Sync the signature collection.
pub struct CollectionStep;

impl Step for CollectionStep {
    fn id(&self) -> &'static str {
        "collection"
    }
    fn title(&self) -> String {
        "Sync gem_rbs_collection".into()
    }
    fn run(&self, ctx: &Context, progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
        progress(format!("syncing {}", ctx.settings.collection.url));
        let synced = collection::open(&ctx.settings.collection, &ctx.settings.github)?;
        let summary = format!("{} gems @ {}", synced.gems, synced.commit);
        Ok((Output::Collection(CollectionInfo { root: synced.root, commit: synced.commit, gems: synced.gems }), summary))
    }
}

/// Download each gem's source and reverse-compile the collection's signatures into it.
pub struct GemPairsStep;

impl Step for GemPairsStep {
    fn id(&self) -> &'static str {
        "pairs"
    }
    fn title(&self) -> String {
        "Fetch gems, build pairs".into()
    }
    fn needs(&self) -> &'static [&'static str] {
        &["collection"]
    }
    fn run(&self, ctx: &Context, progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
        let info = ctx.collection.as_ref().ok_or("the collection step has not run")?;
        let wanted = &ctx.settings.scenario.gems;
        let mut gems = collection::latest(&collection::list_gems(&info.root));
        if !wanted.is_empty() {
            gems.retain(|g| wanted.contains(&g.name));
            if gems.is_empty() {
                return Err(format!("none of scenario.gems {wanted:?} is in the collection"));
            }
        }
        let factory = factory_for(ctx, "ruby-rbs")?;
        let (found, done, failed) = (Mutex::new(Vec::<Pair>::new()), AtomicUsize::new(0), Mutex::new(Vec::<String>::new()));
        let (skipped, notes) = (Mutex::new(Vec::<String>::new()), Mutex::new(std::collections::BTreeSet::<String>::new()));
        collection::import_gems(factory.as_ref(), &gems, &ctx.settings.collection, &ctx.settings.github, &|g, result| {
            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            match result {
                Ok(imported) => {
                    progress(format!("[{n}/{}] {} ({} pairs)", gems.len(), imported.label, imported.pairs.len()));
                    found.lock().unwrap().extend(imported.pairs);
                    skipped.lock().unwrap().extend(imported.skipped.into_iter().map(|s| format!("{}: {s}", imported.label)));
                    notes.lock().unwrap().extend(imported.notes);
                }
                Err(e) => {
                    progress(format!("[{n}/{}] {}/{}: {e}", gems.len(), g.name, g.version));
                    failed.lock().unwrap().push(format!("{}: {e}", g.name));
                }
            }
        });
        let mut pairs = found.into_inner().unwrap();
        pairs.sort_by(|a, b| a.id.cmp(&b.id)); // deterministic
        let failed = failed.into_inner().unwrap();
        let mut summary = format!("{} pairs from {} gems", pairs.len(), gems.len() - failed.len());
        if let Some(first) = failed.first() {
            summary.push_str(&format!("; {} gems unavailable (first: {first})", failed.len()));
        }
        let skipped = skipped.into_inner().unwrap();
        if let Some(first) = skipped.first() {
            summary.push_str(&format!("; {} files skipped as invalid Ruby (first: {first})", skipped.len()));
        }
        for note in notes.into_inner().unwrap() {
            summary.push_str(&format!("; {note}"));
        }
        Ok((Output::Pairs(pairs), summary))
    }
}

/// Generate synthetic pairs from the seeded factory.
pub struct GeneratePairsStep;

impl Step for GeneratePairsStep {
    fn id(&self) -> &'static str {
        "pairs"
    }
    fn title(&self) -> String {
        "Generate pairs".into()
    }
    fn run(&self, ctx: &Context, _progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
        let s = &ctx.settings.scenario;
        let factory = factory_for(ctx, "ruby-rbs")?;
        let pairs: Vec<Pair> = (s.synthetic_seed..s.synthetic_seed.saturating_add(s.synthetic_count))
            .map(|seed| factory.generate(seed))
            .collect();
        let summary = format!("{} pairs, seeds {}..{}", pairs.len(), s.synthetic_seed, s.synthetic_seed.saturating_add(s.synthetic_count));
        Ok((Output::Pairs(pairs), summary))
    }
}

/// Compile every pair with the sentinel the scenario resolved.
pub struct CompileStep {
    pub needs: &'static [&'static str],
}

impl Step for CompileStep {
    fn id(&self) -> &'static str {
        "compile"
    }
    fn title(&self) -> String {
        "Compile with sentinel".into()
    }
    fn needs(&self) -> &'static [&'static str] {
        self.needs
    }
    fn run(&self, ctx: &Context, progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
        let sentinel = ctx.sentinel.as_ref().ok_or("the sentinel step has not run")?;
        let factory = factory_for(ctx, "ruby-rbs")?;
        let pairs: Vec<Mutex<Pair>> = ctx.pairs.iter().cloned().map(Mutex::new).collect();
        let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let total = pairs.len();
        std::thread::scope(|scope| {
            for _ in 0..ctx.settings.collection.jobs.clamp(1, total.max(1)) {
                scope.spawn(|| {
                    while let Some(slot) = pairs.get(next.fetch_add(1, Ordering::SeqCst)) {
                        slot.lock().unwrap().recompile(factory.as_ref());
                        let n = done.fetch_add(1, Ordering::SeqCst) + 1;
                        if n % 250 == 0 || n == total {
                            progress(format!("{n}/{total}"));
                        }
                    }
                });
            }
        });
        let pairs: Vec<Pair> = pairs.into_iter().map(|m| m.into_inner().unwrap()).collect();
        let errors = pairs.iter().filter(|p| p.compile_error.is_some()).count();
        let summary = format!(
            "{total} pairs through sentinel {} ({}){}",
            sentinel.version,
            factory.compile_mode(),
            if errors > 0 { format!(", {errors} compile errors") } else { String::new() }
        );
        Ok((Output::Pairs(pairs), summary))
    }
}

/// Judge the compiled pairs and compare the run with the previous one.
pub struct CheckStep {
    pub scenario: &'static str,
}

impl Step for CheckStep {
    fn id(&self) -> &'static str {
        "check"
    }
    fn title(&self) -> String {
        "Check against source, diff with last run".into()
    }
    fn needs(&self) -> &'static [&'static str] {
        &["compile"]
    }
    fn run(&self, ctx: &Context, _progress: &(dyn Fn(String) + Sync)) -> Result<(Output, String), String> {
        let version = ctx.sentinel.as_ref().map(|s| s.version.clone()).unwrap_or_else(|| "unknown".into());
        let report = Report::from_pairs(self.scenario, &version, &ctx.pairs);
        let path = report_path(&collection::cache_dir(&ctx.settings.collection), self.scenario);
        let diff = load_report(&path).map(|previous| diff(&previous, &report));
        save_report(&path, &report)?;
        let checked = report.matched + report.mismatched;
        let mut summary = if checked == 0 {
            format!("{} pairs, none with source signatures to check", report.total)
        } else {
            format!(
                "{} of {checked} match the source signatures ({:.1}%), {} mismatch",
                report.matched,
                100.0 * report.matched as f64 / checked as f64,
                report.mismatched
            )
        };
        match &diff {
            Some(d) => summary.push_str(&format!("; vs last run: {}", d.headline())),
            None => summary.push_str("; first run, nothing to compare with"),
        }
        Ok((Output::Report { report, diff }, summary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        scenario::Event,
        settings::{SentinelSource, Settings},
    };
    use std::sync::Mutex;

    /// Settings that use the sentinel on this machine (honouring `SENTINEL_BIN`) and keep
    /// every cache inside `dir`, so the test needs no network.
    fn offline(dir: &std::path::Path, count: u64) -> Settings {
        let mut s = Settings::load(std::path::Path::new("/nonexistent")).unwrap();
        s.sentinel.source = SentinelSource::Installed;
        s.collection.cache_dir = dir.to_string_lossy().into();
        s.scenario.synthetic_count = count;
        s
    }

    #[test]
    fn every_scenario_is_a_valid_graph() {
        for s in all() {
            s.validate().unwrap_or_else(|e| panic!("{}: {e}", s.id));
            assert!(find(s.id).is_some());
        }
        assert!(find("nope").is_none());
    }

    /// Runs the synthetic scenario twice against the installed sentinel (skipped without one):
    /// the first run is the baseline, the second compares with it and finds nothing changed.
    #[test]
    fn synthetic_scenario_runs_end_to_end_and_diffs_with_the_last_run() {
        let dir = tempfile::tempdir().unwrap();
        let settings = offline(dir.path(), 12);
        let events = Mutex::new(Vec::new());
        let run = || synthetic().run(Context::new(settings.clone()), &|e| events.lock().unwrap().push(e));

        let first = match run() {
            Ok(ctx) => ctx,
            Err(e) if e.error.contains("failed to run") => return eprintln!("skipped: {e}"),
            Err(e) => panic!("{e}"),
        };
        assert_eq!(first.pairs.len(), 12);
        assert!(first.pairs.iter().all(|p| !p.compiled.is_empty() || p.compile_error.is_some()), "every pair was compiled");
        let report = first.report.as_ref().unwrap();
        assert_eq!((report.total, report.unchecked), (12, 12), "synthetic pairs have no source signatures to check");
        assert!(first.diff.is_none(), "nothing to compare with the first time");
        assert!(first.sentinel.is_some());

        let second = run().unwrap();
        let diff = second.diff.as_ref().expect("the second run compares with the first");
        assert!(diff.is_empty() && !diff.is_alarming(), "{diff:?}");

        // Steps announced themselves in dependency order, and the independent ones started together.
        let events = events.into_inner().unwrap();
        let started: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                Event::Started { step, .. } => Some(step.as_str()),
                _ => None,
            })
            .take(4)
            .collect();
        assert_eq!(started, ["sentinel", "pairs", "compile", "check"]);
    }

    /// The previous report is the baseline for the next run, so a sentinel that starts failing
    /// a pair it used to handle shows up as a regression.
    #[test]
    fn a_pair_that_stops_matching_is_reported_as_a_regression() {
        use crate::scenario::{Report, diff};
        let mut good = crate::registry(&Settings::default())[0].generate(1);
        good.expected = "class A\n  def f: () -> void\nend\n".into();
        good.compiled = good.expected.clone();
        let mut bad = good.clone();
        bad.compiled = "class A\nend\n".into();

        let before = Report::from_pairs("s", "0.6.0", &[good.clone()]);
        let after = Report::from_pairs("s", "0.7.0", &[bad]);
        let d = diff(&before, &after);
        assert_eq!(d.regressed, [good.id.clone()]);
        assert!(d.is_alarming());
        let fixed = diff(&after, &before);
        assert_eq!(fixed.fixed, [good.id]);
        assert!(!fixed.is_alarming());
    }
}
