//! Renders the GUI with one sample pair to a PNG (dev aid): `cargo run --example snapshot -- out.png [seed]`.

use slint::ComponentHandle;
use synthentic_sample::{Settings, highlight::Highlighter, registry, ui::*};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "snapshot.png".into());
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(7);

    let settings = Settings::load("settings.json".as_ref())?;
    let hl = Highlighter::new(&settings.highlight.theme);
    let factories = registry(&settings);
    // First seed at/after `seed` that uses `#:` comments, with one signature broken
    // so the diagnostics strip has something to show.
    let mut pair = (seed..).map(|s| factories[0].generate(s)).find(|p| p.output.contains("\n  #: (")).unwrap();
    pair.output = pair.output.replacen(") -> ", ") -> oops(", 1);
    pair.recompile(factories[0].as_ref());

    let app = App::new()?;
    app.set_highlight_enabled(settings.highlight.enabled);
    app.set_instruction(pair.instruction.clone().into());
    app.set_input_text(pair.input.clone().into());
    app.set_output_text(pair.output.clone().into());
    app.set_selected(0);
    show_compiled(&app, &pair, &hl);
    app.window().set_size(slint::LogicalSize::new(1400.0, 800.0));
    app.show()?;
    // Let layout settle, then capture.
    slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
        let buf = app.window().take_snapshot().expect("snapshot");
        std::fs::write(format!("{out}.rgba"), buf.as_bytes()).unwrap();
        println!("{}x{}", buf.width(), buf.height());
        slint::quit_event_loop().unwrap();
    });
    slint::run_event_loop()?;
    Ok(())
}
