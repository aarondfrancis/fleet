---
name: rust-practices
description: Apply Rust coding, testing, API design, async, and unsafe-code practices when writing, refactoring, debugging, or reviewing Rust crates, including Rust portions of Tauri apps. Follow repository conventions and the pinned toolchain.
---

# Rust Practices for Agents

Distilled from Microsoft's Pragmatic Rust Guidelines (M-* IDs), the openai/codex AGENTS.md, Josh McKinney's Rust agent instructions, and leonardomso/rust-skills. Target: Rust 2024 edition, current stable.

The spirit beats the letter. Follow local repo conventions (and any AGENTS.md / CLAUDE.md) over this file when they conflict. Don't churn existing code just to match these rules — apply them to what you touch.

## 1. Verify loop (non-negotiable)

A change is not done until these pass, scoped to the crates you touched first:

```sh
cargo fmt --all
cargo clippy -p <crate> --all-targets -- -D warnings
cargo nextest run -p <crate>     # or cargo test -p <crate> if nextest isn't installed
cargo test -p <crate> --doc      # nextest doesn't run doctests
```

- Run the full workspace suite only after touching shared/core crates. Avoid `--all-features` on routine runs unless features are what changed.
- If the repo has a `justfile` / `Makefile` / `xtask`, use its targets instead of raw cargo.
- Cargo builds can sit on the build-directory lock. Be patient; never kill cargo by PID.
- Don't re-run tests just because `fmt` or `clippy --fix` touched formatting.
- If `Cargo.toml` changed, confirm `Cargo.lock` is updated and committed with it.

## 2. Defaults the model gets wrong (highest-value rules)

These contradict patterns common in older training data. Apply them every time.

- **Async traits:** no `#[async_trait]`, no `#[allow(async_fn_in_trait)]`. Use native async fns in traits; when the trait is public or used across threads, spell the contract: `fn run(&self) -> impl Future<Output = T> + Send;`. Impls may still write `async fn`.
- **Closure bounds for async callbacks:** `F: AsyncFn(X) -> Y`, not `F: Fn(X) -> Fut, Fut: Future`.
- **Lazy statics:** `std::sync::LazyLock` / `OnceLock`, not `lazy_static!` or `once_cell`.
- **Lint suppression:** `#[expect(lint, reason = "...")]`, not `#[allow]`. Never silence a lint as "cleanup". (M-LINT-OVERRIDE-EXPECT)
- **Format args:** inline them — `format!("{name}: {count}")`, not `format!("{}: {}", name, count)`.
- **Control flow:** use `let ... else` for early returns and `if let` chains (2024) instead of nested `if let`. Collapse nested `if`s.
- **2024 unsafe syntax:** `unsafe extern "C" { ... }` with items marked `safe`/`unsafe`; `#[unsafe(no_mangle)]`, `#[unsafe(export_name = "...")]`.
- **No opaque positional args:** don't introduce `bool` or ambiguous `Option` params that produce `foo(false)` / `bar(None)` callsites. Use an enum, a named method, or a newtype. If you can't change the API, annotate: `foo(/*recursive*/ false)`.
- **Method refs over closures:** `.map(String::len)` not `.map(|s| s.len())`.
- **Exhaustive matches:** match owned enums exhaustively; avoid `_` arms that silently swallow new variants.

## 3. Errors and panics

- Libraries: concrete error types (`thiserror`), ideally a struct wrapping a kind, implementing `Debug + Display + std::error::Error`. (M-ERRORS-CANONICAL-STRUCTS) Applications: `anyhow` (or `eyre`) is fine. (M-APP-ERROR)
- Convert with `From` impls so `?` works; reach for `map_err` only for one-off context. (M-FROM-ERROR)
- Add context at boundaries with `.context("reading config")` / `.with_context(|| ...)`. Preserve source chains (`#[source]`, `#[from]`).
- Error messages: lowercase, no trailing period.
- Panic means "stop the program". Detected programming bugs panic; expected failures return `Result`. Never panic for control flow. (M-PANIC-IS-STOP, M-PANIC-ON-BUG)
- No `.unwrap()` in non-test code. `.expect("why this invariant holds")` only for real invariants.
- Never swallow errors (`let _ = fallible();`) without a comment explaining why it's safe.
- Don't log an error and also return it — handle it once.

## 4. API and type design

- Accept borrowed, flexible inputs: `&str` not `&String`, `&[T]` not `&Vec<T>`, `impl AsRef<Path>` where it helps. (M-IMPL-ASREF)
- Borrow before cloning. A `.clone()` to satisfy the borrow checker is a smell — restructure first.
- Parse, don't validate: convert raw input into validated newtypes at boundaries (`TryFrom`, `FromStr`). Newtypes must guard their invariants. (M-STRONG-TYPES-GUARD)
- Prefer concrete types > generics > `dyn Trait`. Don't box when `impl Trait` works. (M-DI-HIERARCHY)
- Don't leak dependency types or wrappers (`Arc<Mutex<..>>`) through public APIs. (M-DONT-LEAK-TYPES, M-AVOID-WRAPPERS)
- Builders for complex construction; validate in `.build() -> Result`. Mark builders `#[must_use]`. (M-INIT-BUILDER, M-BUILD-RESULT)
- Public types implement `Debug`; types meant to be read implement `Display`. (M-PUBLIC-DEBUG)
- Implement `From`, not `Into`. Use `#[non_exhaustive]` on public enums/structs that will grow.
- Naming: `as_` (free borrow), `to_` (costly), `into_` (consuming); no `get_` on simple getters; `is_`/`has_` for bools; acronyms as words (`HttpClient`). Avoid weasel words like `Manager`, `Service`, `Helper`, `Util`. (M-WEASEL-WORDS)
- Use `checked_`/`saturating_`/`wrapping_` arithmetic deliberately; `TryFrom` for narrowing casts, not `as`.
- Items are reachable through one canonical path; no glob re-exports. (M-SINGLE-ITEM-PATH, M-NO-GLOB-REEXPORTS)

## 5. Module and crate shape

- Target modules under ~500 lines excluding tests. Past ~800, put new functionality in a new module instead of growing the file. When extracting, move related tests and docs with the code.
- Organize modules by concept/feature, not by kind (`models/`, `utils/`). Main concept first; helpers near callers.
- Prefer `foo.rs` + `foo/` over `foo/mod.rs` unless the repo already uses `mod.rs`.
- Keep visibility narrow. Private by default; `pub` only for intentional API. Don't reflexively reach for `pub(crate)`.
- Don't write one-off helper functions that are called exactly once — inline them.
- Identify the "gravity well" crate/module everything gets dumped into and resist adding to it; add a new module or crate instead. If in doubt, split the crate. (M-SMALLER-CRATES)
- Keep `main.rs` thin; logic lives in `lib.rs` so it's testable.
- Workspace: shared settings, dependency versions, and `[workspace.lints]` live in the root `Cargo.toml`; crates are siblings in one folder. (M-CARGO-WORKSPACE, M-CRATES-FLAT-FOLDER)
- Cargo features are strictly additive. New crates use the latest edition.

## 6. Async and concurrency

- Never hold a `std::sync::Mutex`/`RwLock` guard across `.await`. Scope the guard or use `tokio::sync` locks only when you must hold across awaits.
- Blocking or CPU-heavy work goes in `spawn_blocking` (or rayon); use `tokio::fs`, not `std::fs`, inside async code.
- Bounded channels by default — backpressure is a feature. Pick the channel by shape: `mpsc` queue, `oneshot` reply, `broadcast` fan-out, `watch` latest-value.
- Concurrent independent work: `join!` / `try_join!`; dynamic task sets: `JoinSet`; shutdown: `CancellationToken`.
- Every future in a `select!` branch must be cancellation-safe.
- Long-running loops need yield points. (M-YIELD-POINTS)
- Instrument with `#[tracing::instrument]` on the fn definition, not `.instrument()` at call sites.
- For public async/concurrent APIs, document runtime assumptions, cancellation, backpressure, and ordering.

## 7. Testing

- Assert on whole values: `assert_eq!(actual, expected)` on the struct, not field-by-field. Use `pretty_assertions::assert_eq` for readable diffs.
- Don't write tautological tests — no tests that assert a constant equals itself, or that only re-state the implementation. (M-TAUTOLOGICAL-TESTS) Don't add negative tests for removed logic.
- Snapshot-test rendered/serialized output with `insta`. Review `.snap.new` files before `cargo insta accept`; never bulk-accept blindly.
- Integration tests in `tests/`; test-only utilities behind a feature, not leaking into the main implementation. (M-TEST-UTIL)
- Make I/O, time, randomness, env, and process access injectable so tests don't mutate global state. (M-MOCKABLE-SYSCALLS)
- `#[tokio::test]` for async; `proptest` for parsers/invariants; `loom` for lock-free code; Miri for anything with `unsafe`.

## 8. Documentation

- Every public item gets `///`. First sentence is one line, about 15 words. (M-FIRST-DOC-SENTENCE)
- Use canonical sections when relevant: `# Errors`, `# Panics`, `# Safety`, `# Examples`. (M-CANONICAL-DOCS)
- Examples use `?`, not `.unwrap()`, and prove real usage, not just that a constructor compiles.
- Module docs (`//!`) explain what the module owns. Use intra-doc links.
- Document magic values. Update nearby docs in the same change as the API.
- Don't write meta design docs or narration comments ("// Now we loop over items"). (M-NO-META-DESIGN-DOCUMENTATION)

## 9. Performance

- Measure before optimizing. A perf change needs a baseline, a result, and the tradeoff.
- Cheap defaults that are always fine: `Vec::with_capacity` when size is known, entry API for maps, iterators over indexing, don't collect intermediate iterators, `BufReader`/`BufWriter` for many small I/O ops, `write!` into a buffer over repeated `format!` in loops.
- Fast hasher (`FxHashMap`/`ahash`) only where DoS resistance doesn't matter. (M-FAST-HASHER)
- Apps: consider `mimalloc`; release profile with `lto`, `codegen-units = 1`. (M-MIMALLOC-APPS)
- Use `tracing` with structured fields, not `println!` or interpolated strings. Never log secrets. (M-LOG-STRUCTURED, M-LOG-NOT-PRINT)

## 10. Unsafe and FFI

- Avoid `unsafe` unless there's a concrete reason (FFI, proven perf need). All code must be sound. (M-UNSAFE, M-UNSOUND)
- Every `unsafe` block gets a `// SAFETY:` comment; every `unsafe fn` gets a `# Safety` doc section. Keep blocks minimal and wrap them in safe APIs.
- Business logic lives in core crates; FFI / bindings layers only translate. (M-FFI-TRANSLATES)
- Run `cargo miri test` on crates with `unsafe`.

## 11. Tauri apps

- `#[tauri::command]` functions are thin adapters: parse input, call into a core crate/module, map the result. No business logic in commands.
- Commands return `Result<T, E>` where `E: Serialize` — define one serializable app error type rather than stringifying errors ad hoc.
- Commands doing I/O or heavy work are `async` (or use `spawn_blocking`) so they don't block the main thread.
- Keep shared state in `tauri::State<T>` with explicit ownership; don't reach for globals.
- Keep command names and payload casing consistent with the frontend (`#[serde(rename_all = "camelCase")]` on IPC types).

## 12. Agent anti-patterns (stop and reconsider)

- Writing Rust that's really another language in Rust syntax: stringly-typed state, `Rc<RefCell<>>` everywhere, class-style inheritance via traits. Rust code solves Rust problems. (M-RUST-SHAPED)
- `.clone()` / `.to_string()` sprinkled to silence the borrow checker.
- Adding a dependency for something std does. Check before adding; prefer minimal, maintained crates.
- Generic or trait abstractions with a single implementation and no planned second.
- Silencing clippy instead of fixing the code.
- Large mixed diffs. Keep changes under ~500 changed lines for logic work; split into reviewable stages otherwise.
- Inventing crate APIs from memory. If unsure a method/feature exists in the pinned version, check `docs.rs` or the source in `~/.cargo/registry`.

## 13. Review checklist

When reviewing Rust (yours or others'):

1. Does it pass the verify loop with zero warnings?
2. Any `unwrap`, unjustified `clone`, lock across `.await`, `_` match arm on an owned enum, or bool/Option positional param?
3. Are error types concrete (lib) with context at boundaries?
4. Is new code in the right module/crate, and did any file blow past ~800 lines?
5. Do tests assert whole values and actually exercise behavior?
6. Are public items documented with the needed sections?
7. Any `unsafe` without `SAFETY:`? Any `#[allow]` without a reason?

## Where conflicts were resolved

Sources disagree on a few points; this skill picks:

- **Preludes:** don't define them (Microsoft M-NO-PRELUDE) over leonardomso's prelude rule.
- **`mod.rs`:** prefer `foo.rs` (joshka) unless the repo already uses `mod.rs`.
- **Async traits:** spell `-> impl Future + Send` in public/threaded traits (codex) rather than bare `async fn` everywhere.
- **Tests inline vs separate:** follow the repo; for new large test modules, a sibling `foo_tests.rs` via `#[cfg(test)] #[path = "foo_tests.rs"] mod tests;` is fine.

## Deeper references (load only when needed)

- Microsoft Pragmatic Rust Guidelines, full LLM-formatted text (~34k tokens): https://microsoft.github.io/rust-guidelines/agents/all.txt — checklist: https://microsoft.github.io/rust-guidelines/guidelines/checklist/
- openai/codex AGENTS.md (production example): https://github.com/openai/codex/blob/main/AGENTS.md
- joshka Rust agent instructions: https://www.joshka.net/practice/agents/rust/
- leonardomso/rust-skills (265 rules with examples, per-rule files): https://github.com/leonardomso/rust-skills
- Rust API Guidelines: https://rust-lang.github.io/api-guidelines/
