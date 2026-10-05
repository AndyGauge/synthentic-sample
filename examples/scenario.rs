//! Run a scenario without the GUI.
//!
//! ```text
//! cargo run --release --example scenario -- [SCENARIO] [--out pairs.jsonl] [--settings settings.json] [--fail-on-regression]
//! ```
//!
//! `SCENARIO` is `gem-rbs-collection` (default) or `synthetic`. Prints each step as it runs,
//! the match rate, and what changed since the previous run (the "sentinel diffs"). With
//! `--fail-on-regression` the exit status is 1 if any pair that matched last time no longer
//! does, so this can gate a CI job.

use std::sync::Mutex;
use synthentic_sample::{Settings, Store, scenario::{Context, Event}, scenarios};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut id, mut out, mut settings_path, mut fail) = (None, None, "settings.json".to_string(), false);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next(),
            "--settings" => settings_path = args.next().ok_or("--settings needs a path")?,
            "--fail-on-regression" => fail = true,
            _ if id.is_none() => id = Some(a),
            _ => return Err(format!("unexpected argument {a:?}").into()),
        }
    }
    let settings = Settings::load(settings_path.as_ref())?;
    let id = id.unwrap_or_else(|| settings.scenario.default.clone());
    let scenario = scenarios::find(&id).ok_or_else(|| {
        format!("no scenario {id:?}; have: {}", scenarios::all().iter().map(|s| s.id).collect::<Vec<_>>().join(", "))
    })?;
    println!("{}\n{}\n", scenario.title, scenario.description);

    let started = std::time::Instant::now();
    let last_progress = Mutex::new(std::time::Instant::now());
    let ctx = scenario
        .run(Context::new(settings), &|event| match event {
            Event::Started { title, .. } => println!("▶ {title}"),
            Event::Progress { step, text } => {
                // Progress can be chatty; show about one line a second per run.
                let mut last = last_progress.lock().unwrap();
                if last.elapsed().as_millis() > 1000 {
                    println!("    [{step}] {text}");
                    *last = std::time::Instant::now();
                }
            }
            Event::Finished { summary, millis, .. } => println!("✔ {summary}  ({millis} ms)"),
            Event::Failed { step, error } => println!("✘ {step}: {error}"),
        })
        .map_err(|e| e.to_string())?;

    let report = ctx.report.as_ref().ok_or("the scenario produced no report")?;
    println!("\n{} pairs in {:?}: {} match, {} mismatch, {} unchecked", report.total, started.elapsed(), report.matched, report.mismatched, report.unchecked);
    let mut regressions = 0;
    match &ctx.diff {
        None => println!("no previous run to compare with (this one is now the baseline)"),
        Some(d) => {
            println!("\nSentinel diff: {}", d.headline());
            for (title, ids) in [("regressed (matched before, mismatch now)", &d.regressed), ("changed mismatch", &d.changed)] {
                if !ids.is_empty() {
                    println!("  {title}:");
                    for id in ids.iter().take(10) {
                        println!("    {id}");
                    }
                    if ids.len() > 10 {
                        println!("    … and {} more", ids.len() - 10);
                    }
                }
            }
            regressions = d.regressed.len();
        }
    }

    if let Some(path) = out {
        let mut store = Store::open(&path)?;
        for p in &ctx.pairs {
            store.add(p.clone());
        }
        store.save()?;
        println!("\nwrote {} pairs to {path}", store.pairs.len());
    }
    if fail && regressions > 0 {
        eprintln!("{regressions} regressions");
        std::process::exit(1);
    }
    Ok(())
}
