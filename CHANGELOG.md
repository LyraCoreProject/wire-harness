# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0-alpha.1] — 2026-08-04

The first release as a standalone repository. Everything below happened while the harness lived in
LyraCore's `tools/wire-client/`; the git history extracted into this repository carries the detail.

### Added

- A headless build-5875 (WoW 1.12.1) wire client: SRP6 logon, encrypted world handshake, framing,
  and decoded-`SMSG` assertions, with no game client in the loop.
- `vanilla-wire smoke` — the generic acceptance test (logon → handshake → char enumerate/create →
  world entry), which asserts nothing server-specific.
- `vanilla-wire scenario NAME` — the named protocol scenarios, in five families under
  `src/scenarios/`: char-select probes, in-world probes, group, social, relay/AOI, and the
  orchestrated multi-step scenarios.
- `vanilla-wire-bench` — a synthetic-player capacity ramp reporting client-observed movement
  latency and throughput, with an optional (and switchable) SpacetimeDB metrics scrape.
- The harness's own `SMSG_UPDATE_OBJECT` VALUES decoder (`src/values_mask.rs`) and map/interest-grid
  math (`src/spatial.rs`) — independent implementations, not re-exports of a server's, so that an
  encoder bug cannot cancel itself out against the tool meant to catch it.
- `adapters/lyracore/` — the LyraCore/SpacetimeDB orchestrators and their seam script, quarantined
  from the generic client and documented as external-dependency-bearing live tests.

### Changed

- Configurable endpoints: `--host`, `--logon-port`, and `--world-port` (which overrides the
  realm-list answer, for servers advertising an address unroutable from the harness).
- Passwords are read from stdin only (`--password-stdin`), never from argv, and are never logged.
- No default account, password, character or target name remains anywhere in the client.

### Removed

- The path dependency on the server's shared crate.
