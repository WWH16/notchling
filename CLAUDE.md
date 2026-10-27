# Notchling

Windows "Dynamic Island" notch in Rust and Slint. Spec: `docs/SPEC.md`. Plan and
progress: `docs/PLAN.md`. Data-source findings: `docs/FINDINGS.md`.

## Rules

- Always load the `/beautiful-rust:rust-best-practices` skill before writing or changing
  Rust code, and follow it.
- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass
  before every commit.
- One commit per plan step. Conventional Commits messages.
- Commits are authored by the repository owner only. Never add `Co-Authored-By`
  trailers or "Generated with" lines.
- Branch `main`, pushed to GitHub (`origin`). Push only when asked.
- After each step, mark it `[x]` in `docs/PLAN.md` in the same commit.
- Never commit real logs, tokens, prompts or hook payloads. Test fixtures are synthetic.
