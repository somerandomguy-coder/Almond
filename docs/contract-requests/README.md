# Contract change requests

Workstreams don't edit `crates/almond-types` directly. To request a change, add `<ws>.md` here with:

1. **What** — the type/trait/field to add or change (paste the proposed Rust).
2. **Why** — what you can't do without it.
3. **Compat** — additive (preferred) or breaking? Who else is affected?

The integrator batches requests into small PRs on `main`; workstreams rebase.
