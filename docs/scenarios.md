# Scenarios

A scenario is TOML. It names one Behavior, Fixture Verbs, typed client packets, checkpoints and
Equivalence Rules. TOML keeps ordered steps readable in diffs and matches the Feature Ledger's
format. Unknown fields fail, so a typo cannot silently change a recording. This is the format
choice for LyraCore ADR 0005.

`schema_version = 2` requires one Character fixture and 1 through 256 steps. Identifiers use
lowercase letters, digits and underscores. A Character specifies its name, race, class, level and
map position in yards, with orientation in radians. Its name has 2 through 12 ASCII letters, with
only the first capitalized. The adapter requires that name to be absent, applies setup, then checks
the Character list and login position. Reset that disposable fixture before rerunning. Accounts
are provisioned separately. Credentials never belong in a scenario.

The initial vocabulary has only `character`. Add items, money, spells, flight nodes, spawned
entities or quests when a scenario needs them. Implement each new verb in both adapters before
adding a scenario that uses it.

Each step sends a typed packet, then receives for `window_ms`. `replies` lists numeric reply
opcodes in order, including repeated replies. A broadcast has an opcode and a named stream.
Every received opcode is recorded. Undeclared packets retain no body and become
`unexpected_packet` differences unless a named scenario rule excuses that opcode at that
checkpoint. Undecodable declared packets become `packet_decode` differences. Missing, extra and
reordered replies are reported after capture, so later checkpoints can still run. Transport
failures and the 4,096-frame checkpoint limit fail the run.

`runner::FixtureAdapter` owns setup. `runner::Transport` owns the wire boundary. An in-process
runner can reuse `Scenario`, `ClientPacket::encode` and `Observation::from_frame` without a socket
or either server adapter. `runner::run_on` takes an existing transport after setup; `runner::run`
also calls the fixture adapter. `compare::check_observations` checks a run against the checkpoint
contracts. Full field comparison uses the scenario-bound Golden Trace and the rules described in
[traces.md](traces.md).

```sh
WIRE_ADAPTER=vmangos vanilla-wire scenario-file scenarios/connection_identity.toml \
  --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
  --account ACCOUNT --password-stdin
```

For LyraCore, set `WIRE_ADAPTER=lyracore`, `WIRE_REALM_URL=http://127.0.0.1:PORT`, `WIRE_SHARD`,
and `WIRE_OPERATOR_TOKEN_FILE`. The Module must have `debug_reducers`. The adapter calls
`debug_set_level` and `debug_teleport`. It uses curl with the Operator token on stdin.

The vmangos adapter requires the local Reference Server. It checks the scenario Account's global
and realm security levels are both zero and checks the GM configuration. It opens a separate GM
World Session using `WIRE_GM_ACCOUNT`, `WIRE_GM_CHARACTER` and stdin password line 2. The GM issues
`.levelup NAME DELTA`, moves to the fixture position with `.go xyzo`, then summons the scenario
Character with `.namego NAME`. Both clients acknowledge setup teleports. After reconnecting the
scenario Character, the adapter drains remaining login frames before opening checkpoint windows.

These commands were checked in
[CharacterCommands.cpp](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/CharacterCommands.cpp)
and [TeleportCommands.cpp](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/TeleportCommands.cpp)
at the pinned source revision. Setup and recording instructions are in
[adapters/vmangos/](../adapters/vmangos/README.md).
