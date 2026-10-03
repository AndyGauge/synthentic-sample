//! Glue between the data model and the generated Slint component.

use crate::{Pair, highlight::{Highlighter, Rgb}};
use slint::{Color, ModelRc, SharedString, VecModel};

slint::include_modules!();

fn color((r, g, b): Rgb) -> Color {
    Color::from_rgb_u8(r, g, b)
}

fn seg(text: &str, rgb: Rgb) -> Seg {
    Seg { text: SharedString::from(text), color: color(rgb) }
}

/// Pushes a pair's compile result and diagnostics into the compiled panel.
pub fn show_compiled(app: &App, pair: &Pair, hl: &Highlighter) {
    let failed = pair.compile_error.is_some();
    let text = pair.compile_error.as_deref().unwrap_or(&pair.compiled);
    app.set_compile_failed(failed);
    app.set_compiled_text(text.into());
    app.set_pane_background(color(hl.background));

    let lines: Vec<Line> = if failed {
        text.lines().map(|l| Line { segs: ModelRc::new(VecModel::from(vec![seg(l, hl.error)])) }).collect()
    } else {
        hl.lines(text)
            .into_iter()
            .map(|spans| Line {
                segs: ModelRc::new(VecModel::from(
                    spans.iter().map(|s| seg(&s.text, hl.color(s.kind))).collect::<Vec<_>>(),
                )),
            })
            .collect()
    };
    app.set_compiled_lines(ModelRc::new(VecModel::from(lines)));

    let diags: Vec<Diag> = pair
        .diagnostics
        .iter()
        .map(|d| Diag {
            text: format!("output line {} {}: {}", d.line, d.severity, d.message).into(),
            color: color(if d.severity == "error" { hl.error } else { hl.warning }),
        })
        .collect();
    app.set_diagnostics(ModelRc::new(VecModel::from(diags)));
}

/// Clears the compiled panel.
pub fn clear_compiled(app: &App, hl: &Highlighter) {
    show_compiled(app, &Pair {
        id: String::new(), factory: String::new(), seed: 0, instruction: String::new(),
        input: String::new(), output: String::new(), compiled: String::new(),
        compile_error: None, diagnostics: Vec::new(),
    }, hl);
}
