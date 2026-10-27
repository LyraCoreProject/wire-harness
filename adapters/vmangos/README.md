# Reference Server

This adapter runs vmangos's prebuilt Linux amd64 release and MariaDB in Docker. It publishes only
loopback logon port 13724 and world port 18085. The database has no host port. Runtime files live
under `$HOME/.cache/lyracore-scratch/reference-server/`, including downloaded archives, SQL,
credentials, extracts and logs. Keep that directory outside Git.

You need Docker with Compose, Python 3.12 or later, `gh`, the harness binary and a build-5875 client.
The setup script runs `lyracore config` in the specified checkout and reads its `client-data` value.
The verified command is `config`, without `get`. Use an installed CLI at the checkout's pinned
revision. The script reads the checkout and client files without editing them.

```sh
python3 adapters/vmangos/setup.py \
  --lyracore-checkout /path/to/LyraCore \
  --lyracore-cli /path/to/installed/lyracore
```

An existing shell may not have a newly granted Docker group. In that case run the command in
`newgrp docker`:

```sh
newgrp docker <<'DOCKER'
python3 adapters/vmangos/setup.py \
  --lyracore-checkout /path/to/LyraCore \
  --lyracore-cli /path/to/installed/lyracore
DOCKER
```

No sudo or source checkout of vmangos is needed. `pins.json` records full source and database
commits, release asset names and archive SHA-256 values. Both Docker images are pinned by digest.
The setup script verifies cached archives before extracting them. It can reuse the supplied
`$HOME/.cache/lyracore-scratch/vmangos-db/db-4641790.zip`; otherwise it downloads the release asset.
A missing pinned asset or changed checksum fails. It never selects a different release or builds
vmangos from source. Changing the pins requires a fresh runtime directory and a new recording.

The script loads all four MySQL dumps into the Reference Server's own MariaDB and provisions the
local GM Account `CONFORMANCE`, with realm 1 permission in `account_access`. Its generated password stays in `credentials/gm-password`, mode
0600, and never enters command arguments. Fixture setup uses GM commands in Common or Orcish after normal SRP login.
The verifier calculation follows build-5875 SRP6, checked against
[`SRP6.cpp`](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/shared/Crypto/Authentication/SRP6.cpp)
and [`AccountMgr.cpp`](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/AccountMgr.cpp)
at the pinned source SHA. No upstream implementation is included here.

## Client extracts and scope

The release's MapExtractor, VMapExtractor and VMapAssembler produce maps, DBCs and vmaps from the
client's MPQs. MoveMapGenerator produces map 0 tile 32,48, which contains the initial Character
fixture. Map, collision and navigation settings remain enabled. The initial setup does not claim
navigation coverage outside that tile. Generate the additional mmaps before adding scenarios
elsewhere. The extractor command and its configuration inputs are in `setup.py`; the full tools
and logs remain under the runtime directory. A failed extraction can be rerun. A completed run
reuses its extracts for the same binary pin.

The server needs DBCs at `data/5875/dbc`; setup links that path to the extractor's `data/dbc`.
`StrictVersionCheck = 0` disables the original executable checksum check for the headless client.
SRP authentication remains enabled. This setup does not test client executable integrity.
Warden is disabled by the upstream release's default configuration.

## Login and recording

Build with `cargo build --locked --bin vanilla-wire`. If the target directory is outside the
checkout, set `WIRE_BIN` to its `debug/vanilla-wire` file. Then record from a fresh Character:

```sh
adapters/vmangos/reference.sh record Conformance traces/connection_identity.json
```

The output directory must exist. The recorder refuses an existing output file or Character.
The adapter creates the Character, applies `.levelup` and `.go xyzo` as needed, then reconnects to
check race, class, level and position. It sends the same scenario requests used by the LyraCore
adapter. Packet text becomes SHA-256 before writing. Review the scrubbed trace and its header
before committing it. The committed trace uses Character `Conformant`; a candidate run must use
that same name to match the name digest. [Trace rules](../../docs/traces.md) describes comparison and provenance.

```sh
adapters/vmangos/reference.sh ps
adapters/vmangos/reference.sh logs --tail 30 world
adapters/vmangos/reference.sh down
```

Run these Docker commands through `newgrp docker` too when needed. `down` removes this Compose
project's containers and network; runtime files remain for the next run. Never use the fixture
adapter against a production or shared Realm. A failed fixture can leave its Character behind;
choose a fresh name for the next attempt.
