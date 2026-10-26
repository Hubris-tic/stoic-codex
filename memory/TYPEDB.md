# TypeDB as the memory layer — decision and verification record

**Verdict: yes, and it is now built and tested against a real local TypeDB 3.14.1.**
It runs as a **local daemon on loopback**, started by Zenithal when needed. Nothing is
hosted, nothing is rented, no account exists, and if no daemon is available the memory
layer runs in-process instead.

## The constraint that shaped the design

> "make sure everything is local not server based … the user does allat via daemon or
> nothing if none is needed."

So the memory layer has exactly three states, and all of them are local:

| State | When | Backend |
|---|---|---|
| Attach | something already listens on `127.0.0.1:1729` | that daemon, untouched |
| Spawn | a `typedb` binary is found | we start it, we stop it |
| Nothing | no binary, or `policy = Never` | in-process store |

There is no cloud path in the code to fall back to. `is_loopback()` is enforced in both
the daemon manager and the driver backend, so a non-local address is refused **before a
socket is opened**.

## What the local daemon actually is

TypeDB Community Edition, MPL-2.0, a single Rust binary (the Java/JRE core disappeared in
3.0). We start it as:

```bash
typedb server \
  --server.listen-address 127.0.0.1:1729 \
  --storage.data-directory ~/.zenithal/typedb \
  --diagnostics.reporting.errors false \   # a local personal DB does not phone home
  --server.http.enabled false              # no Studio UI, one socket only
```

The handle kills only what it started. A daemon you already had running is attached to and
left alone — killing someone else's server is how a tool loses trust.

## Verified against a live server

TypeDB 3.14.1, started locally in this workspace. Everything below is a passing test, not
a design sketch.

```
$ ZENITHAL_TYPEDB_ADDR=127.0.0.1:1729 cargo test --features typedb --test typedb_live
test schema_bootstraps_and_is_idempotent ... ok
test round_trips_nodes_edges_and_embeddings ... ok
test an_edge_to_a_missing_node_is_refused ... ok
test traverses_the_graph_in_both_directions ... ok
test hybrid_search_ranks_semantic_then_lexical ... ok
test clear_empties_the_store ... ok
test forget_removes_a_node_and_the_edges_that_touch_it ... ok
test result: ok. 7 passed
```

The same store driven through the shipped binary (`zenithal memory …`), which starts the
daemon, writes, and stops it — each command a separate process, data persisted in RocksDB
between them:

```
$ ZENITHAL_TYPEDB_BIN=…/typedb zenithal memory remember file:a.rs a.rs --text "alpha token"
remembered file:a.rs (doc) in typedb — 1 entries
$ zenithal memory link file:a.rs file:b.rs imports
linked file:a.rs -> file:b.rs (imports)
$ zenithal memory forget file:b.rs
forgot file:b.rs — 1 entries left
$ zenithal memory stats
backend      typedb (local daemon at 127.0.0.1:1729, Spawned)
entries      1
```

And the whole stack start-to-stop:

```
$ ZENITHAL_TYPEDB_BIN=…/typedb cargo run --features typedb --example local_stack
backend=typedb daemon=127.0.0.1:1730 provenance=Spawned
--- packed context ---
Relevant memory:
- [file] auth.rs (file:auth.rs)  score 0.67
- [commit] fix expired-token bypass (commit:4f2c)  score 0.65
daemon stopped
```

## What the live server taught me (all were wrong in my first draft)

These are the reasons the queries are shaped the way they are. Each one was a failure
against the real parser or the real result set:

1. **`owns foo?` is not valid TypeQL 3.** Optional ownership is the default; `@card(0..1)`
   is available if you want it stated. The docs' `?` habit is TypeQL 2.
2. **`role` is a reserved keyword.** `attribute role` fails with `[DEX31]`. Renamed to
   `agent-role`.
3. **A subtype declaration needs its kind keyword.** `calls sub memory-edge;` is parsed as
   a *reference* to an existing type (`[SYR1] The type 'calls' was not found`); it must be
   `relation calls, sub memory-edge;`.
4. **Role players must be declared.** A traversal over an undirected edge set needs
   `plays memory-edge:source, plays memory-edge:target` on the node supertype, or type
   inference cannot resolve the role (`[INF11]`).
5. **A recursive `fun` needs the start node bound in every branch.** This works:
   ```typeql
   fun reachable($start: memory-node) -> { memory-node }:
     match
       { $x is $start; } or
       { (source: $start, target: $mid) isa memory-edge; let $x in reachable($mid); };
     return { $x };
   ```
6. **`insert` runs per matched row.** `match … delete $n; insert …` in one query silently
   inserts nothing when the match is empty — i.e. on every first write. Upsert is now two
   statements, with a comment saying why.
7. **`select $r` on `isa memory-edge` yields one row per candidate type**, including the
   supertype. Anything that parses the relation label must skip `memory-edge` itself or
   every edge becomes `derived_from`.
8. **`concept row` columns must be read from the row**, not from a hardcoded list — the
   hardcoded list silently dropped every optional attribute (`body`, `importance`, …).
9. **TypeDB also wants port 8000** (Studio) and exits if it is taken. Hence
   `--server.http.enabled false`.
10. **`isa` must come before the role players in a `delete`.** `match $r (source: $a,
    target: $b) isa memory-edge; delete $r;` fails with `[REP1] The variable 'r' cannot be
    declared as both a 'Thing' and as a 'ThingType'` — the same pattern is fine for a read
    with `select`. Deleting an edge needs `$r isa memory-edge (source: $n, target: $m)`.
11. **Deleting a node means deleting its relations first.** TypeDB refuses to remove an
    entity that still plays a role, so `forget` is three statements: the two role
    directions, then the node. The live test asserts that no dangling edge survives.

## What TypeDB actually buys us

1. **Typed memory.** An edge to a missing node cannot be written; a node without a `uid`
   cannot exist. The other backends have to *check* this — TypeDB *refuses* it.
2. **Multi-hop "why" as a query.** `fun reachable($start)` gives transitive impact
   ("which commits and decisions transitively affect `auth.rs`") without a client-side BFS
   and without materialising a closure table.
3. **Inference without staleness.** No denormalised derived edges to keep in sync.
4. **A real answer to the "baked-in graph DB" pillar** instead of a hand-rolled graph.

## What it costs — and how that is contained

| Cost | Containment |
|---|---|
| A process + a port | `LaunchPolicy`: `Auto` (default) or `Never`; falls back to in-process |
| `tonic`/`prost`/`tokio` (~22 MB) | optional dependency behind `--features typedb` |
| Async driver | `sync` feature; the `MemoryBackend` trait stays synchronous |
| No native vector index (alpha upstream) | embeddings stored as text, scored in-process; the `cosine_similarity_search` path is documented for when it stabilises |
| Schema migrations | `bootstrap()` is idempotent; it re-defines only when the probe fails |

## Honest limits

- The in-process backend stores **no** embeddings unless you give it some; nothing here
  calls an embedding model yet. Embeddings are a `Vec<f32>` field today.
- `search()` loads the graph and ranks in Rust rather than pushing scoring into TypeQL.
  For a personal memory (thousands of nodes) that is faster and keeps one scoring
  implementation; at millions of nodes it would need to move into the query.
- The live tests are opt-in (`ZENITHAL_TYPEDB_ADDR`), so CI without a daemon stays green —
  which means a regression in the driver path would only be caught by a machine that has
  the daemon. That is a deliberate trade, and the reason `local_stack` exists as a
  one-command check.
