//! Headless import. Usage:
//!
//! ```text
//! cargo run --example import -- owner/repo[@ref] [out.jsonl]   # a GitHub repo
//! cargo run --example import -- gem:NAME[/VERSION] [out.jsonl] # one gem from gem_rbs_collection
//! cargo run --example import -- gems [out.jsonl]               # every gem in the collection
//! ```
//!
//! Reverse-compiles the signatures into inline annotations, compiles each pair with
//! sentinel, checks the compiled RBS against the source signatures, and prints a summary.

use std::sync::Mutex;
use synthentic_sample::{Settings, Store, collection, github, pair::Check, registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let spec = args.next().ok_or("usage: import <owner/repo | gem:NAME | gems> [out.jsonl]")?;
    let settings = Settings::load("settings.json".as_ref())?;
    let factories = registry(&settings);
    let factory = factories[0].as_ref();
    let mut store = Store::open(args.next().map(Into::into).unwrap_or_else(|| std::env::temp_dir().join("import-example.jsonl")))?;

    let started = std::time::Instant::now();
    let pairs = if spec == "gems" || spec.starts_with("gem:") {
        let root = collection::ensure(&settings.collection, &settings.github)?;
        let gems = match spec.strip_prefix("gem:") {
            Some(one) => vec![collection::find(&root, one)?],
            None => collection::latest(&collection::list_gems(&root)),
        };
        println!("{} gems to import (jobs={})", gems.len(), settings.collection.jobs);
        let found = Mutex::new(Vec::new());
        collection::import_gems(factory, &gems, &settings.collection, &settings.github, &|g, r| {
            match r {
                Ok(i) => {
                    println!("  {:<28} {:>4} pairs from {} files", i.label, i.pairs.len(), i.files);
                    found.lock().unwrap().extend(i.pairs);
                }
                Err(e) => println!("  {}/{}: {e}", g.name, g.version),
            }
        });
        let mut pairs = found.into_inner()?;
        pairs.sort_by(|a, b| a.id.cmp(&b.id));
        pairs
    } else {
        let i = github::import(factory, &spec, &settings.github)?;
        println!("{}: {} pairs from {} files", i.label, i.pairs.len(), i.files);
        i.pairs
    };
    println!("{} pairs built in {:?}", pairs.len(), started.elapsed());

    let started = std::time::Instant::now();
    let (mut matched, mut mismatched, mut failed) = (0, 0, 0);
    for mut pair in pairs {
        pair.recompile(factory);
        failed += pair.compile_error.is_some() as usize;
        match pair.check() {
            Check::Match => matched += 1,
            Check::Mismatch(why) => {
                mismatched += 1;
                println!("  MISMATCH {}", pair.id);
                for line in why.iter().take(3) {
                    println!("      {line}");
                }
            }
            _ => {}
        }
        store.add(pair);
    }
    println!(
        "compiled in {:?}: {matched} match, {mismatched} mismatch ({failed} compile errors)",
        started.elapsed()
    );
    store.save()?;
    println!("wrote {} pairs to {}", store.pairs.len(), store.path().display());
    Ok(())
}
