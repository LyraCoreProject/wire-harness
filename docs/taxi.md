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
| `passenger_and_taxi_state` | Nearby entities differ with world data. Explicit subject paths exclude their packets. Every passenger field remains, including `PLAYER_FLAGS` at field 190; no passenger field is excused. VALUES updates compare per field with their original arrival times. |
| `spline_identity` | Spline ids are unsigned 32-bit server-local sequence numbers. A bijection preserves identity across samples. One sample establishes the mapping and checks its shape. |
| `teleport_counter` | Require unsigned 32-bit counters, including zero. Connection-local identity consistency is `unchecked: single sample`. The runner echoes the received counter. |
| `landing_clock_origin` | Require an unsigned 32-bit monotonic timestamp. Clock consistency is `unchecked: single sample`. Further samples compare elapsed differences with wraparound and this rule's own 100 ms tolerance, one map tick. |
| `landing_delivery` | Arrival must be within 150 ms of the recorded elapsed time, one configured 100 ms map tick plus 50 ms local delivery jitter. The bound remains 150 ms. |
| `passenger_update_delivery` | Passenger VALUES updates use the 150 ms delivery bound unless a named field timing rule applies. Taking the fare only at landing fails even if the final balance is correct. |
| `zone_world_states_delivery`, `zone_weather_delivery` | Opcodes 706 and 756 share the ordered `zones` broadcast stream. Count, order and each packet's arrival time compare, with a 1,150 ms delivery bound, one 1,000 ms zone poll plus a 100 ms map tick and 50 ms delivery jitter. |
| `passenger_zone_flags_delivery` | Only `169.objects.0.values.field_190` uses the 1,150 ms zone polling bound. Its value remains exact. Fare, mount and unit flags retain 150 ms. |
| `zone_world_state_catalogue` | Only `706.states` varies with the world's event catalogue. Map and zone remain exact. |
| `zone_weather_type` | Only `756.weather_type` varies with current weather. |
| `zone_weather_grade` | Only `756.grade` varies; both values must be finite floats in the inclusive range 0 through 1. |
| `zone_weather_sound` | Only `756.sound` varies with the current weather type and intensity. The instant-change flag remains exact. |

The original packets remain in the scrubbed trace. The comparator aligns each stream by opcode
and mapped subject GUID before comparing fields. Missing and extra packets each produce one
record, so an omitted reply cannot shift every later comparison. Unknown opcodes, malformed
packets, extra passenger fields and missing landing updates fail. No rule is specific to LyraCore.

The pinned [Player zone polling code](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Player.cpp#L1215-L1234)
checks zone and area changes every 1,000 ms. `UpdateZone` sends world states and weather and changes
the resting flag. Their polling phase differs between sessions, so the zone delivery rules include
that interval. Fare and landing do not use the zone poll.

## Recording check

The conformance.4 Golden Trace contains 245 scrubbed packets. Its independent repeat contains
259; comparison returned `{"differences": []}`. Arrival times below are milliseconds from activation.

| Event | Golden Trace | Repeat | Bound |
| --- | --- | --- | --- |
| Elwynn world states and weather | 11133 / 11133 | 11236 / 11236 | 1,150 ms |
| Westfall world states and weather | 47426 / 47426 | 48350 / 48351 | 1,150 ms |
| Landing correction | 73547 | 73527 | 150 ms |

An earlier check of the new zone rules used an incorrect 150 ms bound and reported five timing
differences, 185 through 285 ms. Reading the pinned zone poll established the 1,000 ms interval
above. The final recordings use that documented polling allowance. Fare and landing bounds
remain 150 ms. [Trace headers](traces.md#recorded-evidence-for-conformance4) record the exact pins
and scenario hashes.

## Protocol sources

The independent decoders were implemented from build-5875 layouts checked at vmangos source
`d9173eca3ea08dd985c52dabbcc826898d1d0b77`. No vmangos code is copied. Relevant sources:

- [Taxi handler](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Handlers/TaxiHandler.cpp)
  and [taxi packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Taxi.cpp).
- [Spline packet builder](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Movement/spline/packet_builder.cpp)
  and [movement packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Movement.cpp).
- [Combat packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Combat.cpp)
  and [miscellaneous packets](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Misc.cpp).
  `InitWorldStates` carries a 32-bit map, 32-bit zone, 16-bit count and pairs of unsigned state ids
  and signed 32-bit values. `WeatherUpdate` carries type, float grade, sound id and a boolean
  instant-change flag for build 5875.

The recording header also pins database `464179081673cfd240f7ffe7f0daf96bca0b5a70` and the binary
archive hash. Re-record through the Reference Server adapter when changing the scenario or pins.
The LyraCore known-node reducer and full taxi run remain pending T2d; see the
[LyraCore adapter contract](../adapters/lyracore/README.md#data-scenarios).
