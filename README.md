# synthentic-sample

Deterministic, seeded **training-pair generator** for teaching a model to write
[inline RBS](https://github.com/ruby/rbs/blob/master/docs/inline.md) comments in Ruby, built around
[sentinel](https://github.com/AndyGauge/rbs-sentinel) (which transpiles those comments into `.rbs` files).

Each row is an *instruction*, a plain Ruby file (*input*), and the same file with inline RBS added (*output*),
plus what sentinel makes of that output. The rows are meant to feed a fine-tuning pipeline (for example for
gpt-oss-120b) that needs to learn a code style it doesn't already know.

![The synthentic-sample GUI: a list of pairs, plain Ruby input, annotated output, and sentinel's highlighted RBS](docs/screenshot.png)

## What it does

- **Generates pairs from a seed.** The same seed gives the same pair on every platform, so a dataset is
  reproducible from `(factory, seed range)`.
- **Runs scenarios.** A scenario is a workflow: fetch the latest sentinel, sync a source of Ruby, build the
  pairs, compile them, check the results and diff them with the previous run. One click runs the whole thing,
  and a human then reviews the pairs with alerts about what sentinel changed (see [Scenarios](#scenarios)).
- **Is built on an abstract factory.** `PairFactory` is the interface; the Ruby/RBS generator is just one
  implementation. Another language or dialect is another implementation.
- **Compiles every output through sentinel**, so each row carries the real RBS sentinel produced and any
  diagnostics it raised. Rows with warnings are the suspect ones.
- **Imports pairs from the [`ruby/gem_rbs_collection`](https://github.com/ruby/gem_rbs_collection)** (synced by
  the app itself) or from any GitHub repo, by reverse-compiling the real `.rbs` files back into inline RBS.
- **Verifies every imported pair.** The compiled RBS must match the signatures the annotations came from;
  mismatches are flagged in the list and spelled out next to the compiled panel.
- **Has a GUI** to generate, read, edit and delete pairs. Changes are saved to a JSONL file five seconds
  after you stop making them.

## Scenarios

![A scenario run: steps with timings, the sentinel diff alert, and a pair whose compiled RBS matches](docs/scenario.png)

A [`PairFactory`](#adding-a-factory) makes pairs from a seed. A **scenario** is the level above: a small
dependency graph of steps that turns *a sentinel and a source of Ruby* into reviewed pairs. Each step
produces an artifact (a resolved sentinel binary, the synced collection, the pairs, a report); steps whose
dependencies are met run in parallel, and steps wrap the factories.

| Scenario | Steps |
|---|---|
| `gem-rbs-collection` (default) | **Fetch sentinel** ∥ **Sync gem_rbs_collection** → **Fetch gems, build pairs** → **Compile with sentinel** → **Check against source, diff with last run** |
| `synthetic` | **Fetch sentinel** → **Generate pairs** → **Compile with sentinel** → **Check, diff with last run** |

Pick a scenario at the top of the window and press **Run scenario**. Each step shows its result and timing as
it finishes. When the run ends:

- the pairs are **merged into your dataset without overwriting any human work**: new pairs are added, and a
  pair you haven't touched follows the generator (new instruction, input, output, ground truth and compile
  result). Each pair remembers a fingerprint of the text the generator produced, so an edit is detected by
  comparing against it: an edited pair keeps your text, has only `expected` refreshed, and is recompiled with
  the new sentinel. A pair whose **input** changed (a file split differently under the same id) is a different
  pair and is replaced outright;
- **stale pairs** are listed: pairs from a source the run covered (the same gem or repo version) that the run
  no longer produced. A **Remove N stale pairs** button deletes them; hand-edited and synthetic pairs are never
  listed;
- the app switches to the sentinel the run used, so later edits and **Recompile** use it too;
- an **alert** compares the run with the previous one. For example `sentinel 0.6.0 → 0.7.0: 1188 fixed,
  0 regressed, 7 changed mismatch`. It turns red when something that used to match no longer does, listing the
  first regressions and what differs, and each list row is marked ▲ fixed, ▼ regressed or ◆ mismatch changed.

Then review: **show mismatches only** filters to the pairs sentinel got wrong, and you edit those or delete
them (see [Review](#review-does-the-compiled-rbs-match)).

### Where the sentinel comes from

`sentinel.source` in the [settings](#settings) decides what "fetch sentinel" means:

| `source` | What it does |
|---|---|
| `rubygems` (default) | Asks rubygems for the newest published `rbs-sentinel`, downloads the gem, and extracts the binary for this machine from it (cached per version, so a repeat run only asks for the version). If rubygems can't be reached it uses the newest cached one. |
| `git` | Clones `sentinel.git_url` at `sentinel.git_ref` and runs `cargo build --release`: tests code that hasn't been released yet, e.g. `master`. |
| `path` | Uses the binary at `sentinel.path`. |
| `installed` | Uses `sentinel.command` as it is on this machine. |

Whichever it is, the binary is started once to read the version it reports, which also proves it runs.

### Headless, and as a regression test

```sh
cargo run --release --example scenario -- gem-rbs-collection --out pairs.jsonl
cargo run --release --example scenario -- gem-rbs-collection --fail-on-regression   # exit 1 if a pair stopped matching
```

The report from each run is kept in the cache, so the next run (GUI or headless) is compared with it. That
makes the collection a regression suite for sentinel: a few thousand real files, each with a known-correct
answer, checked in seconds.

## Requirements

- A Rust toolchain (edition 2024). The first build compiles Slint and takes a few minutes.
- [`sentinel`](https://github.com/AndyGauge/rbs-sentinel) on your `PATH` (or configured, see
  [Settings](#settings)). It needs to transpile trailing `attr_*` types, `# @rbs` tags and ivars (see
  [rbs-sentinel#33](https://github.com/AndyGauge/rbs-sentinel/issues/33)); older builds silently drop those.
  If it also supports the `sentinel/transpile` request over `sentinel lsp`, compiling happens in memory in
  about a millisecond per pair; otherwise the app falls back to running `sentinel init` in a temp directory per pair. The compiled panel says
  which of the two is in use ("compiled in memory via sentinel lsp" or "compiled fallback: sentinel init…").
- `git`, for syncing the collection and importing from GitHub.
- `gem` (RubyGems), for downloading gem sources when importing from the collection, and for fetching the
  latest sentinel (`sentinel.source = "rubygems"`). `tar` extracts the binary from the gem. `cargo` is needed
  only for `sentinel.source = "git"`.

Importing from the collection needs the network (to sync and to download gems). Developed and tested on macOS. Slint is cross-platform, but nothing else has been tried.

## Quick start

```sh
cargo run --release                          # uses ./pairs.jsonl and ./settings.json (if present)
cargo run --release -- data/ruby.jsonl       # a different dataset file
cargo run --release -- --settings my.json    # a different settings file
```

In the window:

1. Pick a factory (`ruby-rbs`), set a start **seed** and a **count**, and click **Generate**. The pairs
   appear immediately and compile in the background ("compiling N…" under the list). The seed field advances
   to the next unused seed, and seeds you already have are skipped.
2. Click a pair to see its instruction, input, output and sentinel's compiled RBS.
   Selecting a pair starts a fresh compile.
3. Edit any of the three text boxes. Editing the output recompiles about 0.8 s after you stop typing;
   **Recompile** does it on demand.
4. **Delete selected** removes a pair. Everything is saved on its own; closing the window saves immediately.

The compiled panel is syntax highlighted. Sentinel's warnings appear under it (for example
`output line 7 warning: malformed signature for 'initialize'…`), anchored to lines in the output.

## Data format

One JSON object per line. This is a real row:

```json
{
  "id": "ruby-rbs:65",
  "factory": "ruby-rbs",
  "seed": 65,
  "instruction": "Add inline RBS type comments using `#:` method type comments, keeping the code itself unchanged.",
  "input": "class Member\n  attr_reader :email\n  attr_reader :age\n\n  def initialize(age, email = \"a@example.com\")\n    @email = email\n    @age = age\n  end\nend\n",
  "output": "class Member\n  attr_reader :email #: String\n  attr_reader :age #: Integer\n\n  #: (Integer, ?String) -> void\n  def initialize(age, email = \"a@example.com\")\n    @email = email\n    @age = age\n  end\nend\n",
  "expected": "",
  "compiled": "# Generated by Sentinel - Do not edit manually\n\nclass Member\n  attr_reader email: String\n  attr_reader age: Integer\n  def initialize: (Integer, ?String) -> void\nend\n",
  "compile_error": null,
  "diagnostics": []
}
```

- `id` is `factory:seed` for generated pairs and unique within a file.
- `expected` is the RBS the output should compile to. It is empty for synthetic pairs and set for imported
  ones, where it is the part of the source RBS the annotations came from.
- `compiled` is sentinel's output for `output`. `compile_error` is set (and `compiled` empty) if sentinel
  failed. `diagnostics` holds `{line, severity, message}` entries from sentinel's language server.
- The file is rewritten atomically (temp file, then rename), so a crash can't truncate it.

## The Ruby/RBS factory

It builds one random class model (typed attributes plus a few methods with required, optional and keyword
parameters, nilable and union types, `Array[...]`, `Hash[...]`) and renders it twice: bare, and annotated.
Because both come from one model, the input and output differ **only** by comments, which is the signal a
model should learn. A test checks this over 500 seeds.

Each sample uses one of two styles for methods:

| Style | Example |
|---|---|
| `#:` method type | `#: (Integer, ?String) -> void` above the `def` |
| `# @rbs` tags | `# @rbs name: String` / `# @rbs return: void` above the `def` |

Attributes get a trailing `#: Type` in both styles.

## Importing

The left-hand panel has two import sections:

- **gem_rbs_collection** (the main source):
  - **Sync** downloads (or updates) the collection and fills the gem picker. A first-time user sees
    "Press Sync to list the collection's gems"; importing syncs on its own too.
  - Pick a gem and press **Import gem** for one gem, or press **Import all gems** for the newest version of
    every gem in the collection (about 170 gems, and several minutes of downloading).
- **GitHub repository**: `owner/repo`, optionally `owner/repo@branch-or-tag`, or a `https://github.com/…` /
  `git@github.com:…` URL, then **Import**.

Imports run in the background and stream their pairs into the list as each gem finishes. The headless
equivalents are `cargo run --release --example import -- gems out.jsonl`, `… -- gem:redis out.jsonl` and
`… -- owner/repo out.jsonl`.

### The gem_rbs_collection

The collection holds signatures only (`gems/<gem>/<version>/*.rbs`), not the Ruby they describe. So for each
gem the app:

1. **Syncs** the collection into a cache directory (`~/.cache/synthentic-sample` by default): a shallow clone
   the first time, a fast-forward afterwards. This happens automatically before an import
   (`collection.auto_sync`), and the **Sync** button does it on its own. If the network is down it
   falls back to the cached copy.
2. **Downloads the gem's source** with `gem unpack NAME --version '~> X.Y.0'` for the collection's `X.Y`
   directory, and caches it. Several gems are fetched at once (`collection.jobs`, default 4).
3. **Reverse compiles** the gem's Ruby against the collection's `.rbs` (below).

### Reverse compiling

Every Ruby file with matching signatures becomes a pair whose input is the plain Ruby and whose output is the
same Ruby with the signatures put back as inline RBS: the reverse of what sentinel does. Signatures are matched
to Ruby by fully-qualified class name, not by file path.

- `def name: SIG` becomes `#: SIG` above the `def`.
- `attr_reader name: T` becomes a trailing `#: T`.
- `@name: T` (when no attribute already covers it) becomes `# @rbs @name: T` at the top of the class.
- Files longer than `github.max_lines` (default 120) are split into **hunks** at member boundaries. Each hunk
  is re-wrapped in its `class`/`module` and `end`s so it stands alone, and its id gets a `#n` suffix.
  Heredoc bodies, `=begin`/`=end` blocks and `__END__` are treated as text, not code, when choosing the cuts.
- Every hunk that becomes a pair is **checked for valid Ruby** (`github.check_syntax`, on by default). A cut can
  still land somewhere the heuristics don't understand, so if a split produces an invalid hunk the file is
  split into bigger hunks, then not at all, and if even the whole file is invalid it is skipped and reported in
  the import summary. The check uses a **warm worker**: one long-lived `ruby` process per thread that parses
  each source with Prism, about 0.5 ms per check against about 65 ms for a fresh `ruby -c` each time (3,433
  hunks take 1.6 s instead of roughly four minutes).
- Ids look like `ruby-rbs:gem:redis@4.2.5:lib/redis.rb#2` or `ruby-rbs:gh:owner/repo@sha:path/file.rb`, so
  importing the same revision again adds nothing new.

Skipped: Ruby files with no matching signatures (no ground truth), overloaded methods, and `attr_*` lines
naming several symbols.

### Review: does the compiled RBS match?

Each imported pair stores `expected`, the signatures its annotations were generated from. When sentinel
compiles the output, the app parses both and compares them member by member (whitespace and the trailing comma
sentinel adds when it wraps long parameter lists don't count; extra members in sentinel's output are fine).

- A **green dot** in the list means the compiled RBS matches, **red** means it doesn't, **gray** means it is
  still compiling. Synthetic pairs have no dot: they have no ground truth.
- Select a pair to see **✓ matches the source RBS** or **✗ MISMATCH** above the compiled panel, with one line
  per difference (`missing from compiled RBS (expected …)` or `expected …, compiled …`).
- **show mismatches only** filters the list to the red ones, and the line under the list tallies matches and
  mismatches.

On the full collection (174 gems, 172 of which could be fetched, 3,440 pairs) 74% of pairs match exactly. The
other 26% are all members sentinel didn't emit, none are different signatures, and they come from the one-class-
per-file limitation below. A mismatch means either sentinel can't transpile something the annotations say (see
[Known limitations](#known-limitations)) or the pair needs editing; edit the output and it recompiles and
re-checks on its own.

### A GitHub repository

The repo is shallow-cloned and its own `.rbs` files are reverse compiled the same way. Only GitHub
repositories are accepted, and cloning never prompts for credentials, so private repos need working git/SSH
credentials already.

## Settings

A JSON file, `settings.json` in the working directory or the path given to `--settings`. Every field is
optional; see [`settings.example.json`](settings.example.json) for all of them with their defaults.

| Key | Controls |
|---|---|
| `sentinel.source`, `sentinel.path`, `sentinel.gem_name`, `sentinel.git_url`, `sentinel.git_ref`, `sentinel.cargo` | Where a scenario gets its sentinel: `rubygems` (default), `git`, `path` or `installed` (see [above](#where-the-sentinel-comes-from)) |
| `sentinel.command`, `sentinel.args` | The transpiler (default `sentinel init`), used as the fallback compile path and when `source = "installed"`. A scenario replaces it with the binary it fetched |
| `scenario.default`, `scenario.gems`, `scenario.synthetic_seed`, `scenario.synthetic_count` | The scenario selected at startup; `gem-rbs-collection` limited to these gems (empty means all); the seed range of `synthetic` |
| `lsp.enabled`, `lsp.command`, `lsp.args` | The language server (default `sentinel lsp`) used for in-memory compile and diagnostics |
| `highlight.enabled` | `false` gives a plain, selectable text box instead of the highlighted panel |
| `highlight.theme.*` | `#rrggbb` colours: `background`, `plain`, `comment`, `keyword`, `type`, `builtin`, `ivar`, `function`, `punct`, `string`, `error`, `warning` |
| `github.git`, `github.max_lines`, `github.exclude_dirs`, `github.max_file_bytes`, `github.check_syntax`, `github.ruby` | The git executable, how imports read a repo or gem, and whether (and with which `ruby`) split hunks are checked for valid syntax |
| `collection.url`, `collection.cache_dir`, `collection.gem`, `collection.auto_sync`, `collection.jobs`, `collection.exclude_dirs` | The signature collection: where it lives and is cached, the `gem` executable, whether to sync before each import, parallel downloads, and directories skipped inside an unpacked gem |

The `SENTINEL_BIN` environment variable overrides the sentinel and LSP commands, which is handy for trying a
different build: `SENTINEL_BIN=/path/to/sentinel-rb cargo run --release`.

## Adding a factory

Implement `PairFactory` (in `src/pair.rs`) and add it to `registry()` in `src/lib.rs`; it then shows up in the
GUI's factory picker.

```rust
pub trait PairFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// Pure function of the seed: same seed, same pair, on every platform.
    fn generate(&self, seed: u64) -> Pair;

    // Optional:
    fn compile(&self, source: &str) -> Result<Compiled, String>;     // run the downstream tool
    fn import_extensions(&self) -> &'static [&'static str];           // e.g. [".rb", ".rbs"]
    fn import_files(&self, repo: &str, files: &[SourceFile], opts: &ImportOptions) -> Vec<Pair>;
}
```

`generate` should use the provided `Rng` (`src/rng.rs`, a hand-rolled SplitMix64) rather than an external
crate, so a dependency upgrade can never change what a seed produces.

## Layout

| Path | What |
|---|---|
| `src/scenario.rs` | The workflow model: `Scenario`, `Step`, `Context`, the parallel runner, and `Report`/`Diff` between runs |
| `src/scenarios.rs` | The built-in scenarios and their steps (fetch sentinel, sync, build pairs, compile, check) |
| `src/sentinel_source.rs` | Resolving a sentinel from rubygems (extracting its binary from the gem), git, a path, or the installed one |
| `src/pair.rs` | `Pair`, `PairFactory`, `Compiled`, `Diagnostic` |
| `src/factories/ruby_rbs.rs` | The Ruby/RBS factory (generation, compile, import) |
| `src/factories/rbs_reverse.rs` | The reverse annotator (also yields each pair's `expected` RBS) |
| `src/factories/ruby_hunks.rs` | Splitting long Ruby files into standalone, validated hunks |
| `src/ruby_syntax.rs` | The warm Ruby syntax-check worker |
| `src/factories/sentinel.rs`, `src/lsp.rs` | Running `sentinel init` / talking to `sentinel lsp` |
| `src/collection.rs` | Syncing `gem_rbs_collection`, fetching gem sources, building pairs per gem |
| `src/rbs.rs` | A small RBS reader and the member-by-member comparison behind the match check |
| `src/github.rs` | Cloning and reading a repository |
| `src/highlight.rs`, `src/ui.rs`, `ui/app.slint` | Highlighting and the Slint UI |
| `src/store.rs`, `src/settings.rs`, `src/rng.rs` | JSONL store, settings, RNG |
| `examples/scenario.rs`, `examples/import.rs`, `examples/snapshot.rs` | Run a scenario headless; headless import; render the window to raw RGBA |

## Development

```sh
cargo test --lib                                   # unit tests
cargo test --lib -- --include-ignored --nocapture  # also prints sentinel's coverage over 100 generated pairs
SENTINEL_BIN=/path/to/sentinel-rb cargo test --lib # exercise the in-memory compile path
```

Tests that need sentinel skip themselves when it isn't installed.

## Known limitations

- Sentinel does not attach `#:` to `private def foo` / `protected def foo`. Imports annotate those methods
  anyway (it is valid inline RBS) and the warning lands in the pair's `diagnostics`, so you can filter them.
- Sentinel transpiles **one class per file**: the first `class` it finds, wrapped in its enclosing modules.
  Other classes, and the members of an enclosing `module`, are dropped, so those pairs are flagged as
  "missing from compiled RBS". Over the whole collection this accounts for every mismatch (see below).
  The pair is best dropped or split until sentinel supports multi-class files.
- Repos with inline annotations but no `.rbs` files import nothing.
- Hunking is indentation based, not a parser (hunks are validated with Ruby, see above). A single member longer than `max_lines` stays whole.
- Only the compiled panel is highlighted: the editable Input and Output boxes can't show colours.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional
terms or conditions.

See [CHANGELOG.md](CHANGELOG.md) for what has changed.
