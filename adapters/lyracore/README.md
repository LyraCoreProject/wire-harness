# LyraCore adapter

The adapter connects the Headless Client to a local Native Host. The Account must already exist.
It takes its password from stdin, or from `WIRE_PASSWORD_<UPPERCASE_ACCOUNT>` or
`WIRE_FIXTURE_PASSWORD` for a disposable local fixture. It has no default password.

```sh
printf '%s' "$PASSWORD" | adapters/lyracore/wire.sh ACCOUNT Character
```

`wire.sh` keeps the positional Account and Character interface. Further arguments select a legacy
Rust scenario. `WIRE_BIN` selects an existing `vanilla-wire`; otherwise the script builds it from
this repository. `WIRE_HOST`, `WIRE_LOGON_PORT` and `WIRE_WORLD_PORT` select the local listeners.
The host defaults to `127.0.0.1`, logon to `3724`, and world to the realm-list address.
`WIRE_CLASS` and `WIRE_RACE` default to `warrior` and `human` for Character creation.

`adapter-env.sh` provides `resolve_lyracore` for scripts that need the Module. It checks
`LYRACORE_DIR`, or the working directory, for `native-host/`, `module/` and `Cargo.toml`.
Connecting through `wire.sh` needs no source checkout.

To use this checkout from LyraCore's pinned CLI:

```sh
LYRACORE_WIRE_HARNESS_DIR=/path/to/wire-harness ./lyracore dev smoke ACCOUNT Character
```

The CLI builds and runs `vanilla-wire smoke` directly, with the password on stdin. Keep the binary
name, `adapters/lyracore/` directory and `wire.sh` available for CLI consumers. No adapter starts,
restarts or provisions a Realm. Use a local Realm you own.

## Data scenarios

Schema 4 scenarios use one pre-existing Account per named Character. Set `WIRE_PEER_ACCOUNTS` to
the additional Account names in Character order and pass their passwords on later stdin lines.
The adapter prepares each Character's Fixture Verbs and reconnects that Account when a step asks.

The fixture adapter calls these existing debug reducers as the Operator: `debug_set_level(character_guid, level)`,
`debug_teleport(character_guid, map_id, x, y, z, orientation)`,
`debug_set_money(character_guid: u64, copper: u32)` and
`debug_spawn_at_feet(character_guid: u64, creature_entry: u32, offset: f32)`.
Creature offsets follow the Character's orientation. Captured CREATE updates bind each scenario
creature id to the spawned GUID and verify its entry and position.

The known-node verb calls the agreed T2d reducer
`debug_learn_taxi_node(character_guid: u64, node_id: u32)`. Core must restrict it to the Operator and
`debug_reducers`, validate the Character and `game_taxi_node`, and use the discovery path to persist
the known node without starting a flight or sending a packet. The adapter's exact call is unit-tested;
live verification of this verb and the complete LyraCore taxi scenario is pending T2d. The other
required reducers already exist. Fixture creatures in LyraCore live for the disposable Realm's
lifetime; use a fresh Realm when resetting them.
