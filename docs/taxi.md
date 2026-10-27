# Single-hop taxi evidence

`taxi_single_hop` flies from Stormwind (node 2) to Sentinel Hill (node 4) on map 0. Both are Alliance
nodes with mount creature 541. These ids, maps and mount assignments agree between vmangos's
`taxi_nodes` and the client's `TaxiNodes.dbc`, which supplies LyraCore's imported nodes.

The check used the read-only SQLite copy at
`$HOME/.cache/lyracore-scratch/vmangos-db/sqlite/sqlite-dump/mangos.sqlite`, taking each node's
highest build at or below 5875 (4222 for both). The client check read the extractor's
`reference-server/data/dbc/TaxiNodes.dbc` under the same scratch root. No database or client files
are committed.

| Node | vmangos x, y, z | Client DBC x, y, z |
| --- | --- | --- |
| 2, Stormwind | -8840.56, 489.7, 109.61 | -8840.5595703125, 489.70001220703125, 109.61000061035156 |
| 4, Sentinel Hill | -10628.9, 1036.68, 34.06 | -10628.8896484375, 1036.6800537109375, 34.060001373291016 |

The rounded database coordinates differ slightly from the DBC. Node ids and the map agree; this
does not establish identical coordinates. The scenario does not add a position tolerance to hide
that difference. TaxiPath.dbc defines route 6, with a fare of 110 copper and 28 route vertices.

## Fixtures and observations

The scenario creates a level-1 human warrior, gives exactly 1,000 copper, learns nodes 2 and 4,
and spawns flight-master entry 352 two yards forward of the Character at node 2. It asks for status,
then the node menu, then activates the flight. The menu must report node 2 and known-node bits 2
and 4. The flight checkpoint lasts 80 seconds so it includes the Reference Server's 73,484 ms
flight and landing. The recorded balance becomes 890 copper.

The Reference Server sends cancel-combat, activation success, mount result and a flying monster
spline, then dismount result, cancel-combat and a teleport correction at landing. It does not send
a monster-move stop for this landing. The spline includes 27 points after its start position.
The spline's last point is (-10629.0654296875, 1039.445556640625, 37.9543342590332).
The landing correction uses the database node position
(-10628.900390625, 1036.6800537109375, 34.060001373291016), with zero movement flags. The runner replies with the Character's full GUID, correction counter and timestamp.
Assertions prevent a truncated flight recording from becoming a Golden Trace.

## Equivalence Rules

All parameters and reasons are in the scenario and covered by its hash.

| Rule | Constraint and reason |
| --- | --- |
| GUID bijection | Passenger and flight-master allocations differ by server; references must remain consistent and one-to-one. Zero still maps only to zero. |
| Reply order and broadcast streams | Answers to activation retain their order and multiplicity. Unrelated broadcast streams can interleave. |
| Duration tolerance | 1 ms covers final integer rounding of a floating-point spline length. Route points, flags, fare and landing position remain exact. |
| Wall clock | 2,000 ms is the schema's clock bound; this scenario's wall-clock samples occur only in its excluded opening. |
| `passenger_and_taxi_state` | Nearby world entities differ with data. Explicit subject paths retain passenger movement, spells, combat and emotes. Object updates retain every passenger unit-flags (46), mount-display (133) and money (1176) change in order, including duplicates, across packet batching. |
| `spline_identity` | Spline ids are server-local sequence numbers. A separate bijection preserves their identity, rather than treating them as elapsed time. |
| `teleport_counter` | Correction counters are connection-local and may start at zero. The runner must echo the actual value. |
| `landing_clock_origin` | Monotonic clocks have unrelated process origins; compare elapsed differences from their first sample, with 32-bit wraparound. Delivery timing checks this flight's single landing sample separately. |
| `landing_delivery` | Arrival must be within 150 ms of the recorded elapsed time: one configured 100 ms map tick plus 50 ms local delivery jitter. This does not change the 1 ms duration bound. |
| `zone_world_states` | Flight crosses zones; world-state catalogues depend on world data. Keep opcode 706 in the trace. |
| `zone_weather` | Weather is independent world state. Keep opcode 756 in the trace. |

The complete original packets remain in the scrubbed trace. Unknown opcodes, malformed packets,
extra passenger events and missing landing updates fail. There are no random-field rules or
LyraCore-specific exceptions.

## Protocol sources

The independent decoders were implemented from build-5875 layouts checked at vmangos source
`d9173eca3ea08dd985c52dabbcc826898d1d0b77`. No vmangos code is copied. Relevant sources:

- [Taxi handler](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Handlers/TaxiHandler.cpp)
  and [taxi packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Taxi.cpp).
- [Spline packet builder](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Movement/spline/packet_builder.cpp)
  and [movement packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Movement.cpp).
- [Combat packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Combat.cpp)
  and [miscellaneous packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Misc.cpp).

The recording header also pins database `464179081673cfd240f7ffe7f0daf96bca0b5a70` and the binary
archive hash. Re-record through the Reference Server adapter when changing the scenario or pins.
The LyraCore known-node reducer and full taxi run remain pending T2d; see the
[LyraCore adapter contract](../adapters/lyracore/README.md#data-scenarios).
