# Contributing (humans and agents)

## Ownership — the one rule that makes parallel work possible

Each workstream (see `docs/PLAN.md`) owns directories. **Only edit what you own.**

| WS | Branch | Owns |
|---|---|---|
| 0 (contracts) | `main` | `crates/almond-types`, root `Cargo.toml`, `.cargo/`, `.github/`, `docs/ARCHITECTURE.md`, `docs/API.md` |
| A Audio | `ws/a-audio` | `crates/almond-audio` |
| B STT | `ws/b-stt` | `crates/almond-stt`, `eval/stt` |
| C LLM/pipeline | `ws/c-llm` | `crates/almond-llm`, `crates/almond-pipeline`, `eval/summary` |
| D Store/sync | `ws/d-sync` | `crates/almond-store`, `crates/almond-sync` |
| E Calendar/mail | `ws/e-calendar` | `crates/almond-calendar`, `crates/almond-mail`, `docs/google-setup.md` |
| F UI | `ws/f-ui` | `apps/desktop/src` (except `src/gen`), `apps/desktop/package.json`, `apps/desktop/index.html`, vite/ts config |
| G Android | `ws/g-android` | `plugins/tauri-plugin-almond-android`, `apps/desktop/src-tauri/gen/android` |
| H Verify | `ws/h-verify` | `crates/almond-verify`, `docs/adr/neuro-symbolic.md` |
| Integration | `main` (Phase 2) | `crates/almond-core`, `apps/desktop/src-tauri/src` |

- Each crate may add **its own** dependencies in its own `Cargo.toml`. Don't touch the root
  `Cargo.toml` (workspace deps) — ask instead.
- `Cargo.lock` will conflict; on rebase just run `cargo update -w` / `cargo build` and commit the result.
- Need a contract change in `almond-types`? Write `docs/contract-requests/<ws>.md` (what, why,
  proposed diff), keep working against a local shim, and tell the integrator. Don't edit the crate.
- Use the mocks: `almond-types = { path = "../almond-types", features = ["mock"] }` under
  `[dev-dependencies]` (or normal deps for examples).

## Quality bar

- `cargo fmt --all`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  `cargo test --workspace --all-features` must pass.
- UI: `pnpm -C apps/desktop build` must pass.
- Tests that need real hardware/models/network are `#[ignore]` with a comment on how to run them.
- Model files, recordings and secrets never go in git (see `.gitignore`).

## Commits & milestones

- Conventional-ish messages: `stt: whisper-rs engine with chunked streaming`.
- Commit + push at every milestone in `docs/PLAN.md`; open a PR to `main` when your WS is done.
- End commit messages with the co-author trailer if an AI agent wrote the change.
