# zenithal-memory

Local-first memory for Zenithal: a **typed graph plus vector retrieval behind one
trait**, with a bundled TypeDB daemon when you want a real graph database and an
in-process store when you do not.

```bash
cargo test                                   # 90 tests, no server, no network
cargo test --features typedb                 # 92 + the driver backend compiling
ZENITHAL_TYPEDB_ADDR=127.0.0.1:1729 \
  cargo test --features typedb --test typedb_live    # 7 live tests against a local daemon
```

| Piece | File | What it does |
|---|---|---|
| Domain model | `src/domain.rs` | `MemoryNode`, `MemoryEdge`, `Hit`, `Signals`; TypeQL type mapping |
| Backend seam | `src/backend.rs` | `MemoryBackend` trait + `Capabilities` |
| In-process backend | `src/inmemory.rs` | no deps, instant start; nothing persists |
| File backend | `src/jsonfile.rs` | `~/.zenithal/memory.json`; atomic writes, survives restarts, no daemon needed |
| Ranking | `src/rank.rs` | one scoring path shared by every backend |
| Scoring | `src/scoring.rs` | cosine, BM25, graph proximity, recency, importance, inference |
| Facade | `src/store.rs` | `MemoryStore` — budgeted `context()`, `explain()`, and `open()` which picks a backend |
| The `zenithal` command | `../cli` | drives all of the above from the shell |
| Local daemon | `src/daemon.rs` | find / spawn / attach / stop a local TypeDB, loopback only |
| TypeQL layer | `src/typedb.rs` + `schema/memory.tql` | schema, queries, escaping; no driver dependency |
| Driver backend | `src/typedb_driver.rs` | `--features typedb`; the real thing, tested live |

Read **`TYPEDB.md`** for the evaluation, the eleven things the live server taught me, and
the local-only architecture.

## Local only, three ways

```rust
use zenithal_memory::{BackendChoice, MemoryStore, OpenOptions};

// 1. No database at all (default in this crate)
let mem = MemoryStore::local();

// 2. Let the library choose: a local daemon if one is present or can be started, then the
//    JSON file, then in-process. Never a hosted service.
let opened = MemoryStore::open(OpenOptions { backend: "auto".into(), ..Default::default() })?;
println!("{}", opened.label());          // "typedb (local daemon at 127.0.0.1:1729, Spawned)"
assert!(matches!(opened.choice, BackendChoice::TypeDb | BackendChoice::File));

// 3. Say exactly what you want. `typedb` errors if no daemon — it never silently switches
//    your data store.
let opened = MemoryStore::open(OpenOptions { backend: "file".into(), ..Default::default() })?;
```

`auto` falls back only when no daemon can be *found*. A daemon that exists but fails to
attach is reported: silently swapping the store under a user who thinks their graph
database is live is worse than an error.

`DaemonConfig` defaults to `127.0.0.1:1729`, data in `~/.zenithal/typedb`, log in
`~/.zenithal/typedb.log`. A non-loopback address is refused before any socket is opened.

## Design rules

1. **No server in the default path.** `MemoryStore::local()` starts instantly, uses no
   files, needs no config. `open_local` upgrades to a daemon when one is available.
2. **One scoring formula** (`rank.rs` + `scoring.rs`), so results are comparable and
   explainable whichever store produced the candidates.
3. **Fail loudly.** A missing capability returns `Unsupported`, never an empty result that
   looks like "no memories".
4. **Budgeted prompts.** `context()` packs whole entries under a character budget and
   reports what it dropped; it never truncates an entry mid-way.
5. **Only stop what you started.** An attached daemon is never killed on drop.

## Example

```bash
cargo run --example recall_demo        # in-process, prints the packed prompt + score breakdown
ZENITHAL_TYPEDB_BIN=/path/to/typedb \
  cargo run --features typedb --example local_stack   # spawns a daemon, uses it, stops it
```
