## What and why

## How it was tested

- [ ] `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test` on SQLite and Postgres (`AMAIL_TEST_PG_URL`)
- [ ] web: `npm run check && npm run lint && npm test` (and `npm run e2e` for UI changes)

## Checklist

- [ ] Documentation and `CHANGELOG.md` updated where users would notice
- [ ] Both storage backends kept in step (migrations for SQLite and Postgres)
- [ ] No tokens, private paths or other secrets in code, tests or logs
