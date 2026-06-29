# Contributing to CKOS

Thanks for your interest! CKOS is a Cargo workspace; the layout and the spec it
implements are documented in [`docs/`](docs/) (start with
[`docs/architecture.md`](docs/architecture.md) and
[`docs/implementation-status.md`](docs/implementation-status.md)).

## Development loop

```sh
cargo build --workspace        # build
cargo test  --workspace        # unit + doc + integration tests
cargo fmt   --all              # format (CI runs --check)
cargo clippy --all-targets     # lint
```

CI (`docs/ci-workflow.yml`, copy to `.github/workflows/ci.yml`) runs all four
with `RUSTFLAGS="-D warnings"`, so keep clippy and the formatter clean. The
toolchain is pinned in `rust-toolchain.toml`.

## Design constraints

- **The kernel performs no inference (§891).** Keep orchestration in the kernel
  and inference behind the `runtime::Runtime` trait.
- **Dependency-light core.** The crates are currently `std`-only so the workspace
  builds and tests offline. If you add an external crate, gate it behind a
  feature or confine it to a new backend crate, and keep the default build
  std-only and green.
- **Program to the trait, not the backend.** New storage, runtimes, embedders,
  audit/telemetry sinks, identity providers, and event buses should implement the
  existing traits (`Storage`, `Runtime`, `Embedder`, `AuditSink`,
  `TelemetrySink`, `IdentityProvider`, `EventBus`) rather than change callers.
- **Validated state machines.** Task (§893) and agent (§909) transitions are
  checked centrally; prefer returning a `KernelError` over a silent no-op.
- **Test what you add.** Every module carries unit tests; cross-cutting behavior
  is covered by `sdk/tests/end_to_end.rs` and `cli/tests/cli.rs`.

## Where things live

| Area | Crate |
|------|-------|
| Task/event/capability/audit/telemetry primitives | `kernel` |
| Scheduling | `scheduler` |
| Runtime abstraction | `runtime` |
| Knowledge graph (+ versioning) | `graph` |
| Memory, storage, embeddings, GC | `memory` |
| Planner / verifier / policy / workflow / plugins | respective crates |
| Agents, engine, reflection, sessions, retrieval, KQL, messaging, security, knowledge-bus | `sdk` |
| CLI | `cli` |

## Commits & PRs

Keep commits focused with a clear message describing the change and the spec
section it relates to. Ensure `cargo test` and `cargo clippy --all-targets`
pass before pushing.
