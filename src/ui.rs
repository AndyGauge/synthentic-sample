//! Glue between the data model and the generated Slint component.

use crate::{
    Pair,
    highlight::{Highlighter, Rgb},
    pair::Check,
};
use slint::{Color, ModelRc, SharedString, VecModel};

slint::include_modules!();

fn color((r, g, b): Rgb) -> Color {
    Color::from_rgb_u8(r, g, b)
}

fn seg(text: &str, rgb: Rgb) -> Seg {
    Seg { text: SharedString::from(text), color: color(rgb) }
}

/// List-row verdict codes (see `Row.verdict` in the Slint file).
pub fn verdict_code(check: &Check) -> i32 {
    match check {
        Check::NotApplicable => 0,
        Check::Match => 1,
        Check::Mismatch(_) => 2,
        Check::Pending => 3,
    }
}

/// One line of the pair list: id plus the first line of the input.
pub fn row_for(p: &Pair, verdict: i32, change: i32) -> Row {
    let first = p.input.lines().next().unwrap_or("");
    Row { id: p.id.as_str().into(), summary: format!("{}  {first}", p.id).into(), verdict, change }
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

    // Does the compiled RBS match the signatures the annotations came from?
    let (verdict, color_, lines) = match pair.check() {
        Check::NotApplicable | Check::Pending => (String::new(), (0x88, 0x88, 0x88), Vec::new()),
        Check::Match => ("✓ matches the source RBS".to_string(), (0x1a, 0x7f, 0x37), Vec::new()),
        Check::Mismatch(why) => (
            format!("✗ MISMATCH with the source RBS ({} difference{})", why.len(), if why.len() == 1 { "" } else { "s" }),
            (0xcf, 0x22, 0x2e),
            why,
        ),
    };
    app.set_verdict_text(verdict.into());
    app.set_verdict_color(color(color_));
    app.set_mismatch_lines(ModelRc::new(VecModel::from(
        lines.into_iter().map(|l| Diag { text: l.into(), color: color((0xcf, 0x22, 0x2e)) }).collect::<Vec<_>>(),
    )));
}

/// Clears the compiled panel.
pub fn clear_compiled(app: &App, hl: &Highlighter) {
    show_compiled(app, &Pair {
        id: String::new(), factory: String::new(), seed: 0, instruction: String::new(),
        input: String::new(), output: String::new(), expected: String::new(), compiled: String::new(),
        compile_error: None, diagnostics: Vec::new(), generated: 0,
    }, hl);
}
