use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, mpsc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};
use synthentic_sample::{
    Pair, PairFactory, Settings, Store, collection, github,
    highlight::Highlighter,
    pair::{Check, Compiled},
    registry,
    ui::*,
};

const SAVE_DEBOUNCE: Duration = Duration::from_secs(5);

struct State {
    store: Store,
    factories: Vec<Arc<dyn PairFactory>>,
    factory: usize,
    /// Index into `store.pairs`. May name a pair the filter currently hides.
    selected: Option<usize>,
    dirty: bool,
    /// Compile jobs in flight, by pair id.
    pending: HashMap<String, usize>,
    /// `Pair::check()` per pair id, cached so refreshing the list never re-parses RBS.
    verdicts: HashMap<String, Check>,
    /// List row → index into `store.pairs`.
    visible: Vec<usize>,
    only_mismatches: bool,
}

impl State {
    fn recheck(&mut self, id: &str) {
        if let Some(p) = self.store.pairs.iter().find(|p| p.id == id) {
            self.verdicts.insert(id.to_string(), p.check());
        }
    }

    /// The list-row verdict: gray while a pair that has ground truth is compiling.
    fn verdict_code(&self, p: &Pair) -> i32 {
        if !p.expected.is_empty() && self.pending.contains_key(&p.id) {
            return 3;
        }
        self.verdicts.get(&p.id).map_or(0, verdict_code)
    }

    fn is_mismatch(&self, p: &Pair) -> bool {
        matches!(self.verdicts.get(&p.id), Some(Check::Mismatch(_)))
    }
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

/// Progress from a background import / sync, applied on the UI thread.
enum ImportMsg {
    Status(String),
    Pairs { label: String, pairs: Vec<Pair>, files: usize },
    /// Everything is done; re-enables the import controls.
    Finished(String),
}

/// What the import box was asked for.
enum Source {
    Repo(String),
    /// `gem:NAME` or `gem:NAME/VERSION`
    Gem(String),
    AllGems,
}

fn parse_source(spec: &str) -> Source {
    let spec = spec.trim();
    match spec.strip_prefix("gem:") {
        Some(g) => Source::Gem(g.trim().to_string()),
        None if spec == "gems" => Source::AllGems,
        None => Source::Repo(spec.to_string()),
    }
}

/// Mirrors the in-flight compile count into the UI.
fn sync_pending(app: &App, s: &State) {
    app.set_pending_compiles(s.pending.values().sum::<usize>() as i32);
    let selected = s.selected.and_then(|i| s.store.pairs.get(i));
    app.set_compiling(selected.is_some_and(|p| s.pending.contains_key(&p.id)));
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
    let next_seed = store.pairs.iter().filter(|p| p.expected.is_empty()).map(|p| p.seed + 1).max().unwrap_or(0);

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

    let verdicts = store.pairs.iter().map(|p| (p.id.clone(), p.check())).collect();
    let rows = Rc::new(VecModel::<Row>::default());
    app.set_rows(ModelRc::from(rows.clone()));
    let state = Rc::new(RefCell::new(State {
        store,
        factories,
        factory: 0,
        selected: None,
        dirty: false,
        pending: HashMap::new(),
        verdicts,
        visible: Vec::new(),
        only_mismatches: false,
    }));
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

    // Rebuilds the list from the store, honouring the filter. When the visible set is
    // unchanged only the rows that differ are touched, so scrolling isn't disturbed.
    let refresh = {
        let (state, rows, app) = (state.clone(), rows.clone(), app.as_weak());
        Rc::new(move || {
            let Some(app) = app.upgrade() else { return };
            let mut s = state.borrow_mut();
            let visible: Vec<usize> = (0..s.store.pairs.len())
                .filter(|&i| !s.only_mismatches || s.is_mismatch(&s.store.pairs[i]))
                .collect();
            let new_rows: Vec<Row> =
                visible.iter().map(|&i| row_for(&s.store.pairs[i], s.verdict_code(&s.store.pairs[i]))).collect();
            if visible == s.visible {
                for (k, row) in new_rows.into_iter().enumerate() {
                    if rows.row_data(k).as_ref() != Some(&row) {
                        rows.set_row_data(k, row);
                    }
                }
            } else {
                rows.set_vec(new_rows);
                s.visible = visible;
            }
            let position = s.selected.and_then(|sel| s.visible.iter().position(|&i| i == sel));
            app.set_selected(position.map_or(-1, |p| p as i32));
            app.set_has_selection(s.selected.is_some());

            let mismatched = s.store.pairs.iter().filter(|p| s.is_mismatch(p)).count();
            let matched = s.verdicts.values().filter(|c| **c == Check::Match).count();
            app.set_counts_text(
                format!(
                    "{} shown of {} pairs — {matched} match, {mismatched} mismatch",
                    s.visible.len(),
                    s.store.pairs.len()
                )
                .into(),
            );
        })
    };
    refresh();

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
                }
                None => {
                    app.set_instruction("".into());
                    app.set_input_text("".into());
                    app.set_output_text("".into());
                    clear_compiled(&app, &hl);
                }
            }
            sync_pending(&app, &s);
        })
    };

    // Adds pairs to the store; returns the new ids and the duplicate count.
    // Callers queue compiles and call `refresh` once afterwards.
    let add_pairs = {
        let state = state.clone();
        Rc::new(move |pairs: Vec<Pair>| {
            let (mut new_ids, mut skipped) = (Vec::new(), 0);
            let mut s = state.borrow_mut();
            for pair in pairs {
                let id = pair.id.clone();
                if s.store.add(pair) {
                    s.recheck(&id);
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

    // Queues a compile; callers `refresh` afterwards (once per batch).
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

    // Imports clone and download, so they run off the UI thread and stream results back.
    let (import_tx, import_rx) = mpsc::channel::<ImportMsg>();

    let poll = Timer::default();
    poll.start(TimerMode::Repeated, Duration::from_millis(30), {
        let (state, app, hl, save) = (state.clone(), app.as_weak(), hl.clone(), schedule_save.clone());
        let (add_pairs, queue, refresh) = (add_pairs.clone(), queue.clone(), refresh.clone());
        move || {
            let Some(app) = app.upgrade() else { return };
            let mut touched = false;

            while let Ok(msg) = import_rx.try_recv() {
                match msg {
                    ImportMsg::Status(text) => app.set_status(text.into()),
                    ImportMsg::Pairs { label, pairs, files } => {
                        let found = pairs.len();
                        let (new_ids, dupes) = add_pairs(pairs);
                        for id in &new_ids {
                            queue(id);
                        }
                        if !new_ids.is_empty() {
                            save();
                            touched = true;
                        }
                        app.set_status(
                            format!("{label}: {} new pairs ({found} found in {files} files, {dupes} already present)", new_ids.len())
                                .into(),
                        );
                    }
                    ImportMsg::Finished(text) => {
                        app.set_importing(false);
                        app.set_status(text.into());
                    }
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
                    if changed {
                        s.recheck(&done.id);
                    }
                    sync_pending(&app, &s);
                    changed
                };
                touched = true;
                if changed {
                    save();
                }
            }

            if touched {
                refresh();
            }
        }
    });

    // Queue the compile for whichever pair is selected.
    let queue_selected = {
        let (state, queue, refresh) = (state.clone(), queue.clone(), refresh.clone());
        Rc::new(move || {
            let id = {
                let s = state.borrow();
                s.selected.and_then(|i| s.store.pairs.get(i)).map(|p| p.id.clone())
            };
            if let Some(id) = id {
                queue(&id);
                refresh();
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
    refresh();

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
        let (state, app, save) = (state.clone(), app.as_weak(), schedule_save.clone());
        let (queue, add_pairs, refresh) = (queue.clone(), add_pairs.clone(), refresh.clone());
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
            // The pairs are listed right away; compiling happens in the background.
            for id in &new_ids {
                queue(id);
            }
            refresh();
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
        let (github_settings, collection_settings) = (settings.github.clone(), settings.collection.clone());
        let tx = import_tx.clone();
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
            let (gh, cs, tx, source) =
                (github_settings.clone(), collection_settings.clone(), tx.clone(), parse_source(&spec));
            app.set_status(
                match &source {
                    Source::Repo(r) => format!("cloning {r}…"),
                    Source::Gem(g) => format!("importing gem {g}…"),
                    Source::AllGems => "importing every gem in the collection…".to_string(),
                }
                .into(),
            );
            thread::spawn(move || {
                let finished = match source {
                    Source::Repo(spec) => match github::import(factory.as_ref(), &spec, &gh) {
                        Ok(i) => {
                            let _ = tx.send(ImportMsg::Pairs { label: i.label.clone(), pairs: i.pairs, files: i.files });
                            format!("imported {}", i.label)
                        }
                        Err(e) => format!("import failed: {e}"),
                    },
                    Source::Gem(_) | Source::AllGems => import_from_collection(&factory, &source, &cs, &gh, &tx),
                };
                let _ = tx.send(ImportMsg::Finished(finished));
            });
        }
    });

    app.on_sync_collection({
        let app = app.as_weak();
        let (github_settings, collection_settings) = (settings.github.clone(), settings.collection.clone());
        let tx = import_tx.clone();
        move || {
            let Some(app) = app.upgrade() else { return };
            if app.get_importing() {
                return;
            }
            app.set_importing(true);
            app.set_status("syncing gem_rbs_collection…".into());
            let (gh, cs, tx) = (github_settings.clone(), collection_settings.clone(), tx.clone());
            thread::spawn(move || {
                let msg = match collection::sync(&cs, &gh) {
                    Ok(s) => format!("synced gem_rbs_collection @ {}: {} gems ({})", s.commit, s.gems, s.root.display()),
                    Err(e) => format!("sync failed: {e}"),
                };
                let _ = tx.send(ImportMsg::Finished(msg));
            });
        }
    });

    app.on_filter_changed({
        let (state, app, refresh) = (state.clone(), app.as_weak(), refresh.clone());
        move || {
            if let Some(app) = app.upgrade() {
                state.borrow_mut().only_mismatches = app.get_only_mismatches();
                refresh();
            }
        }
    });

    app.on_select({
        let (state, show, refresh, queue_selected) = (state.clone(), show.clone(), refresh.clone(), queue_selected.clone());
        move |row| {
            {
                let mut s = state.borrow_mut();
                s.selected = s.visible.get(row as usize).copied();
            }
            refresh();
            show(); // render the pair first…
            queue_selected(); // …then kick off the compile
        }
    });

    app.on_delete_selected({
        let (state, show, refresh, save) = (state.clone(), show.clone(), refresh.clone(), schedule_save.clone());
        move || {
            let removed = {
                let mut s = state.borrow_mut();
                match s.selected.take() {
                    Some(i) if i < s.store.pairs.len() => {
                        let pair = s.store.pairs.remove(i);
                        s.verdicts.remove(&pair.id);
                        true
                    }
                    _ => false,
                }
            };
            refresh();
            show();
            if removed {
                save();
            }
        }
    });

    // Edits: write through to the store, then debounce a save.
    macro_rules! on_edit {
        ($setter:ident, $field:ident) => { on_edit!($setter, $field, |_: &Rc<RefCell<State>>| {}) };
        ($setter:ident, $field:ident, $after:expr) => {{
            let (state, refresh, save) = (state.clone(), refresh.clone(), schedule_save.clone());
            app.$setter(move |text| {
                {
                    let mut s = state.borrow_mut();
                    let Some(i) = s.selected else { return };
                    s.store.pairs[i].$field = text.to_string();
                }
                refresh(); // the row's summary may have changed
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

/// Syncs the collection, then imports one gem or all of them, streaming each
/// gem's pairs to the UI as it finishes. Returns the closing status line.
fn import_from_collection(
    factory: &Arc<dyn PairFactory>,
    source: &Source,
    cs: &synthentic_sample::settings::CollectionSettings,
    gh: &synthentic_sample::settings::GithubSettings,
    tx: &mpsc::Sender<ImportMsg>,
) -> String {
    let _ = tx.send(ImportMsg::Status("syncing gem_rbs_collection…".into()));
    let root = match collection::ensure(cs, gh) {
        Ok(r) => r,
        Err(e) => return format!("sync failed: {e}"),
    };
    let gems = match source {
        Source::Gem(spec) => match collection::find(&root, spec) {
            Ok(g) => vec![g],
            Err(e) => return format!("import failed: {e}"),
        },
        _ => collection::latest(&collection::list_gems(&root)),
    };

    let (finished, failed, total_pairs) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let first_error = std::sync::Mutex::new(None::<String>);
    collection::import_gems(factory.as_ref(), &gems, cs, gh, &|g, result| {
        let n = finished.fetch_add(1, Ordering::SeqCst) + 1;
        match result {
            Ok(i) => {
                total_pairs.fetch_add(i.pairs.len(), Ordering::SeqCst);
                let _ = tx.send(ImportMsg::Pairs { label: format!("[{n}/{}] {}", gems.len(), i.label), pairs: i.pairs, files: i.files });
            }
            Err(e) => {
                failed.fetch_add(1, Ordering::SeqCst);
                let msg = format!("{}/{}: {e}", g.name, g.version);
                let _ = tx.send(ImportMsg::Status(format!("[{n}/{}] {msg}", gems.len())));
                first_error.lock().unwrap().get_or_insert(msg);
            }
        }
    });
    let failed = failed.into_inner();
    let mut text = format!("imported {} pairs from {} gems", total_pairs.into_inner(), gems.len() - failed);
    if let Some(e) = first_error.into_inner().unwrap() {
        text.push_str(&format!("; {failed} failed (first: {e})"));
    }
    text
}
