# Contributing

## The one rule

**`src/` stays server-agnostic.** It is build-5875 protocol and nothing else. A change to `src/`
that names a database, a reducer, a shard, a fixture character or a specific server's constants is
the wrong change — that belongs in an adapter under `adapters/`. The README's "The boundary"
section is the full statement of this; it is the reason the repository exists separately at all.

Concretely, a patch to `src/` must not:

* add a path dependency, or any dependency that is not a protocol crate;
* introduce a default account, password, character or target name;
* accept a password anywhere but stdin, or log one;
* re-export a decoder from a server so the two share an implementation. (An independent decoder is
  the point: a test tool that decodes with the encoder's own logic cannot catch an encoder bug.)

## Before you open a PR

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

All three are offline and hermetic — no server needed, no server checkout needed. CI runs exactly
these on Linux and macOS, plus a coverage measurement.

The toolchain is pinned in `rust-toolchain.toml`; rustup will fetch it. The crate's MSRV is the
older `rust-version` in `Cargo.toml` — don't raise it casually.

## Adding a scenario

A scenario is one function in `src/scenarios/`, reached by name through
`vanilla-wire scenario NAME`. Document its arguments in a `// Usage: vanilla-wire scenario …`
comment directly above it, register it in its family's dispatcher, and keep every server-specific
value (guids, entries, coordinates, account names) in the caller's hands as an argument.

## Adding an adapter for another server

Create `adapters/<server>/` alongside `adapters/lyracore/`. It needs its own seam script — the
equivalent of `wire.sh`, translating that server's fixture identities into the client's CLI — and a
README stating the external requirements its scripts assume. Nothing in `src/` should have to
change; if it does, that is a sign the client is missing a flag rather than a sign the boundary
should move.

## Live tests

The scripts under `adapters/` open real sessions against a real server, and can collide with
someone's play session. They are operator-gated: ask before running one against a stack you do not
own. CI never runs them.

## Reporting a protocol bug

Include the opcode, the raw bytes if you have them, and what a stock 1.12.1 client does with the
same packet. "The client crashes" is a symptom of a byte layout; the bytes are the report.

## Licensing

By contributing you agree that your contribution is dual-licensed under MIT OR Apache-2.0, matching
the rest of the repository.
