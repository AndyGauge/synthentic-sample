//! Renders the GUI to raw RGBA (dev aid) with the pairs from a JSONL file:
//! `cargo run --example snapshot -- out pairs.jsonl [id-substring]`.
//! Selects the first pair whose id contains the substring (default: the first mismatch).

use slint::{ComponentHandle, ModelRc, VecModel};
use slint::SharedString;
use synthentic_sample::{
    Settings, Store, collection, highlight::Highlighter, pair::Check, scenario::{Report, diff}, scenarios, ui::*,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "snapshot".into());
    let store = Store::open(args.next().ok_or("usage: snapshot out pairs.jsonl [id]")?)?;
    let wanted = args.next();

    let settings = Settings::load("settings.json".as_ref())?;
    let hl = Highlighter::new(&settings.highlight.theme);
    let app = App::new()?;
    app.set_highlight_enabled(settings.highlight.enabled);
    let gems = collection::latest(&collection::list_gems(&collection::checkout(&settings.collection)));
    app.set_gem_names(ModelRc::new(VecModel::from(gems.iter().map(|g| SharedString::from(g.name.as_str())).collect::<Vec<_>>())));

    let checks: Vec<Check> = store.pairs.iter().map(|p| p.check()).collect();

    // With PREV=previous.jsonl (an earlier run), mark what changed since, as a scenario run would.
    let mut changes = std::collections::HashMap::new();
    if let Ok(prev_path) = std::env::var("PREV") {
        let prev = Store::open(prev_path)?;
        let (before, now) = (
            Report::from_pairs("gem-rbs-collection", "0.6.0", &prev.pairs),
            Report::from_pairs("gem-rbs-collection", "0.6.0+7e95c08", &store.pairs),
        );
        let d = diff(&before, &now);
        for (ids, code) in [(&d.fixed, 1), (&d.regressed, 2), (&d.changed, 3)] {
            for id in ids {
                changes.insert(id.clone(), code);
            }
        }
        app.set_alert_text(d.headline().into());
        app.set_alert_color(slint::Color::from_rgb_u8(0x1a, 0x7f, 0x37));
        let sc = scenarios::gem_rbs_collection();
        app.set_scenario_names(ModelRc::new(VecModel::from(vec![SharedString::from(sc.title.as_str())])));
        app.set_scenario_description(sc.description.as_str().into());
        let done = [
            ("sentinel 0.6.0+7e95c08 from git master", 96.5),
            ("174 gems @ 33602f9", 0.5),
            ("3433 pairs from 172 gems", 11.7),
            ("3433 pairs through sentinel (in memory via sentinel lsp)", 2.3),
            ("3407 of 3433 match the source signatures (99.2%)", 0.1),
        ];
        app.set_steps(ModelRc::new(VecModel::from(
            sc.steps
                .iter()
                .zip(done)
                .map(|(st, (detail, secs))| StepRow { title: st.title().into(), detail: format!("{detail} ({secs:.1}s)").into(), state: 2 })
                .collect::<Vec<_>>(),
        )));
    }
    let rows: Vec<Row> = store
        .pairs
        .iter()
        .zip(&checks)
        .map(|(p, c)| row_for(p, verdict_code(c), changes.get(&p.id).copied().unwrap_or(0)))
        .collect();
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
