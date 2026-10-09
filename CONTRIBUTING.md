# Contributing to amail

Thanks for your interest. Bug reports, fixes, documentation and harness
adapters are welcome. For a larger change, please open an issue first so we
can agree on the approach.

## Building

You need Rust (stable, edition 2024) and, for the web interface, Node 20 or
later.

```sh
cd web && npm ci && npm run build && cd ..   # optional: embeds the web interface
cargo build                                  # target/debug/amail
```

Without `web/dist`, `cargo build` still works; the server then shows a page
that explains how to build the interface.

## Tests

Everything must pass on both storage backends:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                                   # SQLite; Postgres tests are skipped

docker run -d --name amail-test-pg -e POSTGRES_PASSWORD=amail \
  -p 127.0.0.1:55432:5432 postgres:17
AMAIL_TEST_PG_URL=postgres://postgres:amail@127.0.0.1:55432/postgres cargo test
```

Each Postgres test creates its own database. No test calls a model: the node
and team tests use a stand-in script that speaks the harness hook protocol
(`crates/amail-cli/tests/common/fake_harness.sh`).

The web interface:

```sh
cd web
npm ci
npm run check && npm run lint && npm test
npm run build && (cd .. && cargo build -p amail-cli)
PLAYWRIGHT_BROWSERS_PATH=.e2e/browsers npx playwright install chromium
PLAYWRIGHT_BROWSERS_PATH=.e2e/browsers npm run e2e
# the same tests with the servers under a path prefix (amail serve --base-path)
AMAIL_E2E_BASE_PATH=/amail PLAYWRIGHT_BROWSERS_PATH=.e2e/browsers npm run e2e
```

Continuous integration (`.github/workflows/ci.yml`) runs all of the above,
the tests on both backends, and a macOS build of the binary. Check workflow
changes locally with [actionlint](https://github.com/rhysd/actionlint).

## Dependencies and licenses

New dependencies must have licenses compatible with distributing amail
under Apache-2.0 (the accepted list is in `about.toml`). After changing
Rust dependencies or the npm packages bundled into the web interface,
regenerate the notices shipped with the binary:

```sh
cargo install --locked cargo-about --features cli
./scripts/third-party-licenses.sh            # writes THIRD_PARTY_LICENSES.md
```

## Releases

Bump the version in `Cargo.toml` (and `web/package.json`), move the
`Unreleased` entries in `CHANGELOG.md` under the new version, then push a
tag `v<version>`: `.github/workflows/release.yml` builds the Linux binaries
with the web interface and attaches them with `SHA256SUMS` to a draft
release.

## Conventions

- Code, identifiers, comments, commit messages and documentation are in
  English. User-visible strings of the web interface are in
  `web/src/lib/i18n.ts`, in English and Chinese.
- Keep both storage backends in step: a schema change is a new migration in
  `crates/amail-store/migrations/sqlite` and `.../postgres`, and the
  conformance tests cover both.
- Harness transcripts and hook payloads are not stable contracts: parsers
  must tolerate unknown fields.
- Commit messages: a short imperative summary line (what changes for the
  user or the code), then the why if it is not obvious.
- Keep pull requests focused; include tests for behaviour changes and update
  the documentation and `CHANGELOG.md` when users would notice.

## Security issues

Please do not open public issues for vulnerabilities; see
[SECURITY.md](SECURITY.md).

## License

amail is licensed under the [Apache License 2.0](LICENSE). Unless you say
otherwise, any contribution you intentionally submit for inclusion is
licensed under the same terms, without additional terms or conditions
(inbound = outbound, as in section 5 of the license). No CLA or sign-off is
required.
