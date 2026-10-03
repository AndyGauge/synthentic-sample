use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};
use synthentic_sample::{
    Pair, PairFactory, Settings, Store, github, highlight::Highlighter, pair::Compiled, registry, ui::*,
};

const SAVE_DEBOUNCE: Duration = Duration::from_secs(5);

struct State {
    store: Store,
    factories: Vec<Arc<dyn PairFactory>>,
    factory: usize,
    selected: Option<usize>,
    dirty: bool,
    /// Compile jobs in flight, by pair id.
    pending: HashMap<String, usize>,
}

/// Compiles run on one background thread so the window never waits on sentinel.
struct Job {
    id: String,
    source: String,
    factory: Arc<dyn PairFactory>,
}

struct Done {
    id: String,
    /// The source that was compiled; the result is dropped if the pair has moved on.
    source: String,
    result: Result<Compiled, String>,
}

/// Mirrors the in-flight compile count into the UI.
fn sync_pending(app: &App, s: &State) {
    app.set_pending_compiles(s.pending.values().sum::<usize>() as i32);
    let selected = s.selected.and_then(|i| s.store.pairs.get(i));
    app.set_compiling(selected.is_some_and(|p| s.pending.contains_key(&p.id)));
}

fn summary(p: &synthentic_sample::Pair) -> String {
    let first = p.input.lines().next().unwrap_or("");
    format!("{}  {first}", p.id)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Usage: synthentic-sample [pairs.jsonl] [--settings settings.json]
    let mut args = std::env::args_os().skip(1);
    let (mut path, mut settings_path) = (PathBuf::from("pairs.jsonl"), PathBuf::from("settings.json"));
    while let Some(a) = args.next() {
        if a == "--settings" {
            settings_path = args.next().ok_or("--settings needs a path")?.into();
        } else {
            path = a.into();
        }
    }
    let settings = Settings::load(&settings_path)?;
    let hl = Rc::new(Highlighter::new(&settings.highlight.theme));
    let store = Store::open(&path)?;
    let next_seed = store.pairs.iter().map(|p| p.seed + 1).max().unwrap_or(0);

    let app = App::new()?;
    app.set_highlight_enabled(settings.highlight.enabled);
    app.set_pane_background(slint::Color::from_rgb_u8(hl.background.0, hl.background.1, hl.background.2));
    let factories: Vec<Arc<dyn PairFactory>> = registry(&settings).into_iter().map(Arc::from).collect();
    app.set_factories(ModelRc::new(VecModel::from(
        factories.iter().map(|f| SharedString::from(f.id())).collect::<Vec<_>>(),
    )));
    app.set_factory_description(factories[0].description().into());
    app.set_seed_text(next_seed.to_string().into());
    app.set_status(format!("{} — {} pairs loaded", path.display(), store.pairs.len()).into());

    let rows = Rc::new(VecModel::from(store.pairs.iter().map(|p| Row { id: p.id.clone().into(), summary: summary(p).into() }).collect::<Vec<_>>()));
    app.set_rows(ModelRc::from(rows.clone()));

    let state = Rc::new(RefCell::new(State { store, factories, factory: 0, selected: None, dirty: false, pending: HashMap::new() }));
    let timer = Rc::new(Timer::default());

    // Restarting a running Timer resets its deadline: that is the debounce.
    let schedule_save = {
        let (state, timer, app) = (state.clone(), timer.clone(), app.as_weak());
        Rc::new(move || {
            state.borrow_mut().dirty = true;
            if let Some(app) = app.upgrade() {
                app.set_status("unsaved changes — saving in 5s…".into());
            }
            let (state, app) = (state.clone(), app.clone());
            timer.start(TimerMode::SingleShot, SAVE_DEBOUNCE, move || flush(&state, &app));
        })
    };

    fn flush(state: &Rc<RefCell<State>>, app: &slint::Weak<App>) {
        let mut s = state.borrow_mut();
        if !s.dirty {
            return;
        }
        let msg = match s.store.save() {
            Ok(()) => {
                s.dirty = false;
                format!("saved {} pairs to {}", s.store.pairs.len(), s.store.path().display())
            }
            Err(e) => format!("SAVE FAILED: {e}"),
        };
        if let Some(app) = app.upgrade() {
            app.set_status(msg.into());
        }
    }

    // Shows the selected pair (or clears the editor).
    let show = {
        let (state, app, hl) = (state.clone(), app.as_weak(), hl.clone());
        Rc::new(move || {
            let (Some(app), s) = (app.upgrade(), state.borrow()) else { return };
            match s.selected.and_then(|i| s.store.pairs.get(i)) {
                Some(p) => {
                    app.set_instruction(p.instruction.clone().into());
                    app.set_input_text(p.input.clone().into());
                    app.set_output_text(p.output.clone().into());
                    show_compiled(&app, p, &hl);
                    app.set_selected(s.selected.unwrap() as i32);
                }
                None => {
                    app.set_instruction("".into());
                    app.set_input_text("".into());
                    app.set_output_text("".into());
                    clear_compiled(&app, &hl);
                    app.set_selected(-1);
                }
            }
            sync_pending(&app, &s);
        })
    };

    // Adds pairs to the store and the list; returns the new ids and the duplicate count.
    let add_pairs = {
        let (state, rows) = (state.clone(), rows.clone());
        Rc::new(move |pairs: Vec<Pair>| {
            let (mut new_ids, mut skipped) = (Vec::new(), 0);
            let mut s = state.borrow_mut();
            for pair in pairs {
                let row = Row { id: pair.id.clone().into(), summary: summary(&pair).into() };
                let id = pair.id.clone();
                if s.store.add(pair) {
                    rows.push(row);
                    new_ids.push(id);
                } else {
                    skipped += 1;
                }
            }
            (new_ids, skipped)
        })
    };

    // Background compile: a worker thread takes jobs; a UI-thread timer applies results.
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let (done_tx, done_rx) = mpsc::channel::<Done>();
    thread::spawn(move || {
        for job in job_rx {
            let result = job.factory.compile(&job.source);
            if done_tx.send(Done { id: job.id, source: job.source, result }).is_err() {
                break;
            }
        }
    });

    let queue = {
        let (state, app) = (state.clone(), app.as_weak());
        Rc::new(move |id: &str| {
            let mut s = state.borrow_mut();
            let Some(pair) = s.store.pairs.iter().find(|p| p.id == id) else { return };
            let Some(factory) = s.factories.iter().find(|f| f.id() == pair.factory).cloned() else { return };
            let job = Job { id: pair.id.clone(), source: pair.output.clone(), factory };
            *s.pending.entry(job.id.clone()).or_default() += 1;
            let _ = job_tx.send(job);
            if let Some(app) = app.upgrade() {
                sync_pending(&app, &s);
            }
        })
    };

    // Repository imports clone over the network, so they run off the UI thread too.
    let (import_tx, import_rx) = mpsc::channel::<Result<github::Imported, String>>();

    let poll = Timer::default();
    poll.start(TimerMode::Repeated, Duration::from_millis(30), {
        let (state, app, hl, save) = (state.clone(), app.as_weak(), hl.clone(), schedule_save.clone());
        let (add_pairs, queue) = (add_pairs.clone(), queue.clone());
        move || {
            let Some(app) = app.upgrade() else { return };
            while let Ok(result) = import_rx.try_recv() {
                app.set_importing(false);
                match result {
                    Ok(imported) => {
                        let found = imported.pairs.len();
                        let (new_ids, dupes) = add_pairs(imported.pairs);
                        for id in &new_ids {
                            queue(id);
                        }
                        if !new_ids.is_empty() {
                            save();
                        }
                        app.set_status(
                            format!(
                                "imported {} new pairs from {} ({found} found in {} files, {dupes} already present) — compiling…",
                                new_ids.len(), imported.label, imported.files
                            )
                            .into(),
                        );
                    }
                    Err(e) => app.set_status(format!("import failed: {e}").into()),
                }
            }
            while let Ok(done) = done_rx.try_recv() {
                let changed = {
                    let mut s = state.borrow_mut();
                    if let Some(n) = s.pending.get_mut(&done.id) {
                        *n -= 1;
                        if *n == 0 {
                            s.pending.remove(&done.id);
                        }
                    }
                    let State { store, selected, .. } = &mut *s;
                    let mut changed = false;
                    if let Some(i) = store.pairs.iter().position(|p| p.id == done.id) {
                        let pair = &mut store.pairs[i];
                        if pair.output == done.source {
                            changed = pair.apply_compiled(done.result);
                            if *selected == Some(i) {
                                show_compiled(&app, pair, &hl);
                            }
                        }
                    }
                    sync_pending(&app, &s);
                    changed
                };
                if changed {
                    save();
                }
            }
        }
    });

    // Queue the compile for whichever pair is selected.
    let queue_selected = {
        let (state, queue) = (state.clone(), queue.clone());
        Rc::new(move || {
            let id = {
                let s = state.borrow();
                s.selected.and_then(|i| s.store.pairs.get(i)).map(|p| p.id.clone())
            };
            if let Some(id) = id {
                queue(&id);
            }
        })
    };

    // Resume anything a previous session never finished compiling.
    let unfinished: Vec<String> = state
        .borrow()
        .store
        .pairs
        .iter()
        .filter(|p| p.compiled.is_empty() && p.compile_error.is_none())
        .map(|p| p.id.clone())
        .collect();
    for id in &unfinished {
        queue(id);
    }

    app.on_factory_changed({
        let (state, app) = (state.clone(), app.as_weak());
        move |i| {
            let mut s = state.borrow_mut();
            s.factory = i as usize;
            if let Some(app) = app.upgrade() {
                app.set_factory_description(s.factories[s.factory].description().into());
            }
        }
    });

    app.on_generate({
        let (state, app, save, queue, add_pairs) = (state.clone(), app.as_weak(), schedule_save.clone(), queue.clone(), add_pairs.clone());
        move |seed, count| {
            let Some(app) = app.upgrade() else { return };
            let (Ok(seed), Ok(count)) = (seed.trim().parse::<u64>(), count.trim().parse::<u64>()) else {
                app.set_status("seed and count must be non-negative integers".into());
                return;
            };
            let pairs: Vec<Pair> = {
                let s = state.borrow();
                (seed..seed.saturating_add(count)).map(|sd| s.factories[s.factory].generate(sd)).collect()
            };
            let (new_ids, skipped) = add_pairs(pairs);
            let added = new_ids.len();
            // The pairs are already listed; compiling happens in the background.
            for id in &new_ids {
                queue(id);
            }
            app.set_seed_text(seed.saturating_add(count).to_string().into());
            if added > 0 {
                save();
            }
            let suffix = if added > 0 { " — saving in 5s…" } else { "" };
            app.set_status(format!("generated {added}, skipped {skipped} duplicate seeds{suffix}").into());
        }
    });

    app.on_import_repo({
        let (state, app) = (state.clone(), app.as_weak());
        let github_settings = settings.github.clone();
        move |spec| {
            let Some(app) = app.upgrade() else { return };
            if app.get_importing() || spec.trim().is_empty() {
                return;
            }
            let factory = {
                let s = state.borrow();
                s.factories[s.factory].clone()
            };
            app.set_importing(true);
            app.set_status(format!("cloning {}…", spec.trim()).into());
            let (spec, settings, tx) = (spec.to_string(), github_settings.clone(), import_tx.clone());
            thread::spawn(move || {
                let _ = tx.send(github::import(factory.as_ref(), &spec, &settings));
            });
        }
    });

    app.on_select({
        let (state, show, queue_selected) = (state.clone(), show.clone(), queue_selected.clone());
        move |i| {
            state.borrow_mut().selected = Some(i as usize);
            show(); // render the pair first…
            queue_selected(); // …then kick off the compile
        }
    });

    app.on_delete_selected({
        let (state, rows, show, save) = (state.clone(), rows.clone(), show.clone(), schedule_save.clone());
        move || {
            let removed = {
                let mut s = state.borrow_mut();
                match s.selected.take() {
                    Some(i) if i < s.store.pairs.len() => {
                        s.store.pairs.remove(i);
                        rows.remove(i);
                        true
                    }
                    _ => false,
                }
            };
            show();
            if removed {
                save();
            }
        }
    });

    // Edits: write through to the store and the list row, then debounce a save.
    macro_rules! on_edit {
        ($setter:ident, $field:ident) => { on_edit!($setter, $field, |_: &Rc<RefCell<State>>| {}) };
        ($setter:ident, $field:ident, $after:expr) => {{
            let (state, rows, save) = (state.clone(), rows.clone(), schedule_save.clone());
            app.$setter(move |text| {
                {
                    let mut s = state.borrow_mut();
                    let Some(i) = s.selected else { return };
                    s.store.pairs[i].$field = text.to_string();
                    if let Some(mut row) = rows.row_data(i) {
                        row.summary = summary(&s.store.pairs[i]).into();
                        rows.set_row_data(i, row);
                    }
                }
                save();
                ($after)(&state);
            });
        }};
    }
    on_edit!(on_instruction_edited, instruction);
    on_edit!(on_input_edited, input);

    app.on_recompile({
        let queue_selected = queue_selected.clone();
        move || queue_selected()
    });
    // Recompile shortly after the user stops typing in the output panel.
    let compile_timer = Rc::new(Timer::default());
    on_edit!(on_output_edited, output, {
        let (queue_selected, compile_timer) = (queue_selected.clone(), compile_timer.clone());
        move |_: &Rc<RefCell<State>>| {
            let queue_selected = queue_selected.clone();
            compile_timer.start(TimerMode::SingleShot, Duration::from_millis(800), move || queue_selected());
        }
    });

    app.window().on_close_requested({
        let (state, app) = (state.clone(), app.as_weak());
        move || {
            flush(&state, &app);
            slint::CloseRequestResponse::HideWindow
        }
    });

    app.run()?;
    Ok(())
}
