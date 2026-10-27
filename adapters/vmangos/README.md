# Reference Server

This adapter runs vmangos's prebuilt Linux amd64 release and MariaDB in Docker. It publishes only
loopback logon port 13724 and world port 18085. The database has no host port. Runtime files live
under `$HOME/.cache/lyracore-scratch/reference-server/`, including archives, SQL, credentials,
extracts and logs. Keep that directory outside Git.

You need Docker with Compose, Python 3.12 or later, `gh`, the harness binary and a build-5875 client.
The setup script runs `lyracore config` in the specified checkout and reads its `client-data` value.
The command is `config`, without `get`. Use an installed CLI at the checkout's pinned revision.
The script reads the checkout and client files without editing them.

```sh
newgrp docker <<'DOCKER'
python3 adapters/vmangos/setup.py \
  --lyracore-checkout /path/to/LyraCore \
  --lyracore-cli /path/to/installed/lyracore
DOCKER
```

No sudo or vmangos source checkout is needed. Both Docker images are pinned by digest. The script
loads all four MySQL dumps into the Reference Server's MariaDB and creates two Accounts:

| Account | gmlevel | Purpose | Password file under `credentials/` |
| --- | --- | --- | --- |
| `CONFORMANCE` | 0 | Scenario gameplay | `player-password` |
| `CONFORMANCEGM` | 3 | Fixture setup in a separate World Session | `gm-password` |

Passwords are generated locally, stored with mode 0600 and passed through stdin. Setup writes both
`account.gmlevel` and realm 1's `account_access` entry, removes older grants for these Accounts,
and checks them. An older single-Account runtime retains its gameplay password when migrating.

Setup explicitly sets `GM.LoginState`, `GM.AcceptTickets`, `GM.Chat`, `GM.WhisperingTo` and
`GM.CheatGod` to zero. `GM.Visible = 1` keeps the GM visible rather than applying hidden mode.
It reads these values back, then recreates the auth and world containers so the new configuration
is loaded. Before each fixture, the adapter checks the configuration and both gameplay security
levels again. You can run the same read-only check yourself:

```sh
newgrp docker <<'DOCKER'
python3 adapters/vmangos/setup.py --verify-account CONFORMANCE
DOCKER
```

The GM World Session applies `.levelup NAME DELTA`, `.go xyzo` and `.namego NAME`. Gameplay runs
on the other Account throughout. The adapter reconnects the scenario Character and checks its
name, race, class, level, position and orientation before capture.

## Archive cache and re-pinning

`pins.json` records source and database commits, release tags, asset names, release asset ids and
archive SHA-256 values. Both archives live in the local content cache:

```text
$HOME/.cache/lyracore-scratch/reference-server/cache/sha256/<archive SHA-256>
```

Setup uses this cache first and checks the checksum on every use. It can migrate a matching archive
from the older `downloads/` directory or `$HOME/.cache/lyracore-scratch/vmangos-db/`. On a cache miss
it downloads the exact release asset id through `gh api`. A changed checksum fails; no newer release
is substituted. Keep both cached archives when cleaning extracted files. They are what permits
rebuilding the pinned runtime after upstream deletes a moving release's assets. Never commit them.

To re-pin:

1. Read `gh api repos/vmangos/core/releases/tags/latest` and the `db_latest` release. Select the
   binary and database assets. Record their ids and names, then resolve both full commit SHAs.
2. Download each selected asset by id, compute its SHA-256 and verify the release's digest when
   present. Review the source/configuration changes at those SHAs. Change all affected fields in
   `pins.json` together.
3. Stop this disposable Reference Server with `reference.sh down`. Retain its archives in a local
   backup, and move its runtime directory aside. Create a fresh runtime with the new archives under
   `cache/sha256/`. Do not reuse a database or client extracts from a different pin.
4. Run setup, check gmlevel 0, record each affected scenario again and review the scrubbed traces.
   Include the pin change and recordings in the harness PR. Never push data or post upstream.

A missing cached archive whose release asset was deleted requires a maintainer's retained copy or
a reviewed re-pin. The script does not build vmangos from source.

## Client extracts and scope

The release's MapExtractor, VMapExtractor and VMapAssembler produce maps, DBCs and vmaps from the
client's MPQs. MoveMapGenerator produces map 0 tile 32,48, which contains the initial Character
fixture. Map, collision and navigation settings remain enabled. This setup does not claim
navigation coverage outside that tile. Generate additional mmaps before adding scenarios elsewhere.
The commands are in `setup.py`; tools and logs stay in the runtime directory. A completed run reuses
its extracts for the same binary pin.

The server needs DBCs at `data/5875/dbc`; setup links that path to `data/dbc`.
`StrictVersionCheck = 0` disables the original executable checksum check for the Headless Client.
SRP authentication remains enabled. This setup does not test client executable integrity.
Warden is disabled by the pinned release's default configuration.

## Login and recording

Build with `cargo build --locked --bin vanilla-wire`. If the target directory is outside the
checkout, set `WIRE_BIN` to its `debug/vanilla-wire`. The scenario fixes the name `Conformant`.
The wrapper uses `Conformsetup` for the separate GM Character and supplies both passwords:

```sh
newgrp docker <<'DOCKER'
adapters/vmangos/reference.sh record traces/connection_identity.json
DOCKER
```

The output directory must exist, and the output file and scenario Character must be absent.
Before a rerun, delete the previous disposable Character through the protocol and use a fresh
output path. For the local Reference Server only:

```sh
WIRE_CRASH_DIR="$HOME/.cache/lyracore-scratch/reference-server/diagnostics" \
  target/debug/vanilla-wire scenario char-delete \
  --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
  --account CONFORMANCE --character Conformant --password-stdin \
  < "$HOME/.cache/lyracore-scratch/reference-server/credentials/player-password"
```

The recorder hashes packet text before writing. It retains every checkpoint opcode, including
ambient movement covered by named scenario rules. Review the scrubbed trace and its header before
committing. [Trace rules](../../docs/traces.md) describes candidate recording and comparison.

The vmangos adapter retries logon once after a typed client-proof rejection or server-proof
mismatch and logs that retry without credentials or proof bytes. Other failures do not retry.
At source revision `d9173eca3ea08dd985c52dabbcc826898d1d0b77`,
[BigNumber](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/shared/Crypto/BigNumber.cpp)
serializes at minimal length when no minimum is specified.
[SHA1](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/shared/Crypto/Hash/SHA1.cpp)
uses that form for BigNumber input, and
[SRP6](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/shared/Crypto/Authentication/SRP6.cpp)
hashes A, B and K through it. A zero most-significant byte therefore changes the proof input relative
to this client's fixed-width hashing. The server proof also hashes M this way. The retry generates
fresh ephemeral values; a second failure still aborts. The failure rate has not been measured.
Setup generates a full-width salt to avoid a persistent short-salt mismatch.

```sh
newgrp docker <<'DOCKER'
adapters/vmangos/reference.sh ps
adapters/vmangos/reference.sh logs --tail 30 world
adapters/vmangos/reference.sh down
DOCKER
```

`down` removes this Compose project's containers and network; runtime files and the archive cache
remain. Never use this fixture adapter against a production or shared Realm.
