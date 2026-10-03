# Changelog

## [Unreleased]

### Added
- **Abstract factory** (`PairFactory`): a seeded, pure `generate(seed) -> Pair` per language/dialect, plus optional `compile`, `import_extensions` and `import_files`. A `Pair` is `{id, factory, seed, instruction, input, output, compiled, compile_error, diagnostics}`. Same seed, same pair, on every platform (hand-rolled SplitMix64 rather than `rand`, with golden-value tests).
- **Ruby/RBS factory** (`ruby-rbs`): renders one random class model twice, bare (input) and annotated (output), so the pair differs only by comments. Covers `#:` method types and `# @rbs` tags, required/optional/keyword params, nilable and union types, `Array[...]`/`Hash[...]`.
- **JSONL store**: one pair per line, atomic saves (temp file + rename), unique by `factory:seed`. Files written before `compiled`/`diagnostics` existed still load.
- **Slint GUI**: pick a factory, generate a seed range, browse pairs, edit instruction/input/output, delete. Edits and deletes save after 5 s of inactivity (debounced), and closing the window flushes immediately. Usage: `synthentic-sample [pairs.jsonl] [--settings settings.json]`.
- **Third "Compiled" panel** showing sentinel's RBS for each pair's output, with syntax highlighting (`highlight.enabled` falls back to a plain selectable box). Highlighting is a small built-in RBS lexer, since the LSP provides diagnostics but not tokens.
- **Compile after render**: pairs render first and compile in the background. Selecting a pair shows it immediately and starts a fresh compile ("compiling…" indicator); `Generate` and imports queue every new pair; editing the output recompiles 0.8 s after you stop typing; unfinished pairs resume at startup. Results for since-edited output are discarded.
- **In-memory compile via `sentinel lsp`**: uses sentinel's `sentinel/transpile` request (RBS plus diagnostics in one round trip, no files). Falls back to `sentinel init` in a temp dir plus the LSP's disk-based diagnostics against older servers. 100 compiles: 18.4 s through the asdf shim, about 0.1 s in memory.
- **LSP diagnostics** shown under the compiled panel and stored on each pair, so rows with warnings can be filtered out of training data.
- **Import from GitHub** (`owner/repo[@ref]`, or `cargo run --example import -- owner/repo [out.jsonl]`): shallow-clones the repo, indexes its `.rbs` files, and reverse-compiles them into inline RBS. Each Ruby file with matching signatures becomes a pair: plain Ruby in, annotated Ruby out (`#: SIG` above defs, trailing `#: T` on single-symbol `attr_*`, `# @rbs @x: T` for ivars no attr covers). Files longer than `github.max_lines` are split into standalone hunks (re-wrapped in their `class`/`module`), with ids suffixed `#n`. Ids are stable per revision. Only GitHub specs are accepted, with no credential prompts.
- **Settings file** (`settings.json`, see `settings.example.json`): sentinel and LSP commands/args and the LSP toggle, highlight toggle and every theme colour, and the `github` options. Every field is optional. `SENTINEL_BIN` overrides the sentinel and LSP commands.
- `examples/snapshot.rs` renders the window to raw RGBA for visual checks.

### Licensing
- Dual licensed under MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, `license` in `Cargo.toml`).

### Known limitations
- Needs a sentinel that transpiles trailing `attr_*` types, `# @rbs` tags and ivars. Older builds silently drop them.
- sentinel does not attach `#:` to `private def foo` / `protected def foo`; imports annotate those methods anyway (valid inline RBS) and the warnings land in the pair's `diagnostics`.
- Import skips Ruby files with no matching `.rbs`, overloaded methods and multi-symbol `attr_*` lines. Repos with inline annotations but no `.rbs` yield nothing.
- Hunking is indentation-based, not a parser; mis-indented source may split poorly. A single member longer than `max_lines` stays whole.
- Only the compiled panel is highlighted; the editable Input/Output boxes cannot show colours.
