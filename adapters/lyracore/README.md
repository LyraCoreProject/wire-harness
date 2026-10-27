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
