# Contributing to O Arreio

Thank you for considering a contribution. A few ground rules keep this project sustainable:

## 1. CLA (Contributor License Agreement)

Before your first PR can be merged you must sign the project CLA (based on the Apache ICLA). It grants the maintainer the right to relicense contributions — this is what keeps the dual-licensing model (AGPL-3.0-only + commercial) viable, and what would allow the project to relax to a more permissive license in the future. A CLA bot will prompt you automatically on your first PR; until the bot is live, maintainers will request it manually in the PR thread.

DCO sign-offs are appreciated but do **not** replace the CLA.

## 2. Architecture rules (non-negotiable)

- **Synchronous Rust only** — no tokio, no async/await.
- **State lives in the Blackboard** — actors and bridges never call each other directly.
- **No new dependencies with build scripts** without prior discussion (Windows AppControl constraint).
- **Security paths are sacred**: never remove authentication, validation, audit logging or sandbox checks to make something pass.

## 3. Honesty rules (PVC)

- No mock, stub or placeholder without declaring it in the PR description.
- No "done" without a test or a reproducible evidence command.
- If a test fails, say so — hidden failures are treated as process bugs.

## 4. Practical notes

- Code comments are written in **Brazilian Portuguese** (project convention); identifiers in English.
- Issues and PRs: English or Portuguese.
- Run `cargo test --workspace` before submitting; CI must be green.
- Licensing: core crates are `AGPL-3.0-only`; `examples/` are Apache-2.0. New core files inherit AGPL.
