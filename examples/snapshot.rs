//! Renders the GUI to raw RGBA (dev aid) with the pairs from a JSONL file:
//! `cargo run --example snapshot -- out pairs.jsonl [id-substring]`.
//! Selects the first pair whose id contains the substring (default: the first mismatch).

use slint::{ComponentHandle, ModelRc, VecModel};
use synthentic_sample::{Settings, Store, highlight::Highlighter, pair::Check, ui::*};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "snapshot".into());
    let store = Store::open(args.next().ok_or("usage: snapshot out pairs.jsonl [id]")?)?;
    let wanted = args.next();

    let settings = Settings::load("settings.json".as_ref())?;
    let hl = Highlighter::new(&settings.highlight.theme);
    let app = App::new()?;
    app.set_highlight_enabled(settings.highlight.enabled);

    let checks: Vec<Check> = store.pairs.iter().map(|p| p.check()).collect();
    let rows: Vec<Row> = store.pairs.iter().zip(&checks).map(|(p, c)| row_for(p, verdict_code(c))).collect();
    let mismatched = checks.iter().filter(|c| matches!(c, Check::Mismatch(_))).count();
    let matched = checks.iter().filter(|c| **c == Check::Match).count();
    app.set_counts_text(format!("{0} shown of {0} pairs — {matched} match, {mismatched} mismatch", rows.len()).into());
    app.set_rows(ModelRc::new(VecModel::from(rows)));

    let pick = store
        .pairs
        .iter()
        .position(|p| match &wanted {
            Some(w) => p.id.contains(w.as_str()),
            None => matches!(p.check(), Check::Mismatch(_)),
        })
        .unwrap_or(0);
    let pair = &store.pairs[pick];
    app.set_selected(pick as i32);
    app.set_has_selection(true);
    app.set_instruction(pair.instruction.clone().into());
    app.set_input_text(pair.input.clone().into());
    app.set_output_text(pair.output.clone().into());
    show_compiled(&app, pair, &hl);

    app.window().set_size(slint::LogicalSize::new(1400.0, 800.0));
    app.show()?;
    slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
        let buf = app.window().take_snapshot().expect("snapshot");
        std::fs::write(format!("{out}.rgba"), buf.as_bytes()).unwrap();
        println!("{}x{}", buf.width(), buf.height());
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop()?;
    Ok(())
}
