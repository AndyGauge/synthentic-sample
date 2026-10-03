//! Headless import: `cargo run --example import -- owner/repo[@ref] [out.jsonl]`.
//! Clones, reverse-compiles the repo's `.rbs` into inline annotations, compiles each
//! pair with sentinel and prints a summary (writes the pairs if an output path is given).

use synthentic_sample::{Settings, Store, github, registry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let spec = args.next().ok_or("usage: import owner/repo[@ref] [out.jsonl]")?;
    let settings = Settings::load("settings.json".as_ref())?;
    let factories = registry(&settings);
    let factory = &factories[0];

    let started = std::time::Instant::now();
    let imported = github::import(factory.as_ref(), &spec, &settings.github)?;
    println!("{}: {} pairs from {} files in {:?}", imported.label, imported.pairs.len(), imported.files, started.elapsed());

    let mut store = match args.next() {
        Some(p) => Store::open(p)?,
        None => Store::open(std::env::temp_dir().join("import-example.jsonl"))?,
    };
    let (mut ok, mut failed, mut with_diags) = (0, 0, 0);
    let started = std::time::Instant::now();
    for mut pair in imported.pairs {
        pair.recompile(factory.as_ref());
        match &pair.compile_error {
            Some(e) => {
                failed += 1;
                println!("  compile error in {}: {}", pair.id, e.lines().next().unwrap_or(""));
            }
            None => ok += 1,
        }
        if !pair.diagnostics.is_empty() {
            with_diags += 1;
            for d in &pair.diagnostics {
                println!("  {} line {}: {}", pair.id, d.line, d.message);
            }
        }
        store.add(pair);
    }
    println!("compiled {ok} ok, {failed} failed, {with_diags} with diagnostics in {:?}", started.elapsed());
    store.save()?;
    println!("wrote {} pairs to {}", store.pairs.len(), store.path().display());
    Ok(())
}
