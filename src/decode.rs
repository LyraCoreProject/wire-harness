use std::collections::BTreeMap;

use anyhow::{bail, ensure, Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Guid(u64),
    TimeMs(u64),
    WallClockMs(u64),
    Text(String),
    List(Vec<Field>),
    Object(BTreeMap<String, Field>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub opcode: u16,
    pub fields: BTreeMap<String, Field>,
}

struct Body<'a>(&'a [u8]);

impl Body<'_> {
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let value = self
            .0
            .get(..N)
            .context("truncated packet field")?
            .try_into()?;
        self.0 = &self.0[N..];
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }

    fn packed_guid(&mut self) -> Result<Field> {
        let mask = self.u8()?;
        let mut value = 0;
        for i in 0..8 {
            if mask & (1 << i) != 0 {
                value |= u64::from(self.u8()?) << (i * 8);
            }
        }
        Ok(Field::Guid(value))
    }

    fn vector(&mut self) -> Result<Field> {
        Ok(Field::Object(BTreeMap::from([
            ("x".into(), self.float()?),
            ("y".into(), self.float()?),
            ("z".into(), self.float()?),
        ])))
    }

    fn vectors(&mut self) -> Result<Field> {
        let count = self.u32()?;
        ensure!(
            count <= 4096 && count as usize <= self.0.len() / 12,
            "invalid spline point count"
        );
        Ok(Field::List(
            (0..count).map(|_| self.vector()).collect::<Result<_>>()?,
        ))
    }

    fn values(&mut self) -> Result<Field> {
        let count = self.u8()?;
        let masks: Vec<_> = (0..count).map(|_| self.u32()).collect::<Result<_>>()?;
        let mut values = BTreeMap::new();
        for (block, mask) in masks.into_iter().enumerate() {
            for bit in 0..32 {
                if mask & (1 << bit) != 0 {
                    values.insert(
                        format!("field_{}", block * 32 + bit),
                        Field::Unsigned(self.u32()?.into()),
                    );
                }
            }
        }
        Ok(Field::Object(values))
    }

    fn movement_info(&mut self) -> Result<(u32, BTreeMap<String, Field>)> {
        let mut fields = BTreeMap::new();
        let movement = self.u32()?;
        fields.insert("flags".into(), Field::Unsigned(movement.into()));
        fields.insert("timestamp".into(), Field::Unsigned(self.u32()?.into()));
        fields.insert("position".into(), self.vector()?);
        fields.insert("orientation".into(), self.float()?);
        if movement & 0x0200_0000 != 0 {
            fields.insert(
                "transport_guid".into(),
                Field::Guid(u64::from_le_bytes(self.bytes()?)),
            );
            fields.insert("transport_position".into(), self.vector()?);
            fields.insert("transport_orientation".into(), self.float()?);
        }
        if movement & 0x0020_0000 != 0 {
            fields.insert("pitch".into(), self.float()?);
        }
        fields.insert("fall_time".into(), Field::TimeMs(self.u32()?.into()));
        if movement & 0x2000 != 0 {
            for name in ["jump_velocity", "jump_sin", "jump_cos", "jump_speed"] {
                fields.insert(name.into(), self.float()?);
            }
        }
        if movement & 0x0400_0000 != 0 {
            fields.insert("spline_elevation".into(), self.float()?);
        }
        Ok((movement, fields))
    }

    fn targets(&mut self) -> Result<Field> {
        let flags = u16::from_le_bytes(self.bytes()?);
        let mut fields = BTreeMap::from([("flags".into(), Field::Unsigned(flags.into()))]);
        for (bits, name) in [(0x0002, "unit"), (0x0880, "object"), (0x1010, "item")] {
            if flags & bits != 0 {
                fields.insert(name.into(), self.packed_guid()?);
            }
        }
        for (bits, name) in [(0x20, "source"), (0x40, "destination")] {
            if flags & bits != 0 {
                fields.insert(name.into(), self.vector()?);
            }
        }
        if flags & 0x2000 != 0 {
            fields.insert("text".into(), Field::Text(self.text()?));
        }
        if flags & 0x8200 != 0 {
            fields.insert("corpse".into(), self.packed_guid()?);
        }
        Ok(Field::Object(fields))
    }

    fn movement(&mut self) -> Result<Field> {
        let flags = self.u8()?;
        let mut fields = BTreeMap::from([("update_flags".into(), Field::Unsigned(flags.into()))]);
        if flags & 0x20 != 0 {
            let (movement, info) = self.movement_info()?;
            fields.extend(info);
            for name in [
                "walk_speed",
                "run_speed",
                "back_speed",
                "swim_speed",
                "swim_back_speed",
                "turn_rate",
            ] {
                fields.insert(name.into(), self.float()?);
            }
            if movement & 0x0040_0000 != 0 {
                let spline = self.u32()?;
                fields.insert("spline_flags".into(), Field::Unsigned(spline.into()));
                if spline & 0x0004_0000 != 0 {
                    fields.insert("facing_angle".into(), self.float()?);
                } else if spline & 0x0002_0000 != 0 {
                    fields.insert(
                        "facing_target".into(),
                        Field::Guid(u64::from_le_bytes(self.bytes()?)),
                    );
                } else if spline & 0x0001_0000 != 0 {
                    fields.insert("facing_point".into(), self.vector()?);
                }
                fields.insert("elapsed".into(), Field::TimeMs(self.u32()?.into()));
                fields.insert("duration".into(), Field::TimeMs(self.u32()?.into()));
                fields.insert("spline_id".into(), Field::Unsigned(self.u32()?.into()));
                fields.insert("points".into(), self.vectors()?);
                fields.insert("destination".into(), self.vector()?);
            }
        } else if flags & 0x40 != 0 {
            fields.insert("position".into(), self.vector()?);
            fields.insert("orientation".into(), self.float()?);
        }
        for (mask, name) in [(0x08, "high_guid"), (0x10, "all")] {
            if flags & mask != 0 {
                fields.insert(name.into(), Field::Unsigned(self.u32()?.into()));
            }
        }
        if flags & 0x04 != 0 {
            fields.insert("target".into(), self.packed_guid()?);
        }
        if flags & 0x02 != 0 {
            fields.insert("transport_time".into(), Field::Unsigned(self.u32()?.into()));
        }
        Ok(Field::Object(fields))
    }

    fn text(&mut self) -> Result<String> {
        let end = self
            .0
            .iter()
            .position(|&byte| byte == 0)
            .context("unterminated text field")?;
        let value = std::str::from_utf8(&self.0[..end])
            .context("non-UTF-8 text field")?
            .to_owned();
        self.0 = &self.0[end + 1..];
        Ok(value)
    }

    fn float(&mut self) -> Result<Field> {
        let value = f32::from_le_bytes(self.bytes()?);
        if !value.is_finite() {
            bail!("non-finite packet field");
        }
        Ok(Field::Float(f64::from(value)))
    }
}

/// Decodes a plaintext body. Unknown opcodes and trailing bytes fail instead of becoming opaque data.
pub fn decode(opcode: u16, body: &[u8]) -> Result<Packet> {
    if opcode == 0x01f6 {
        let declared = body.get(..4).context("truncated compressed size")?;
        let declared = u32::from_le_bytes(declared.try_into()?) as usize;
        ensure!(
            declared <= 8 * 1024 * 1024,
            "compressed update exceeds 8 MiB"
        );
        let mut output = Vec::with_capacity(declared + 1);
        let mut decoder = flate2::Decompress::new(true);
        let status =
            decoder.decompress_vec(&body[4..], &mut output, flate2::FlushDecompress::Finish)?;
        ensure!(
            status == flate2::Status::StreamEnd
                && output.len() == declared
                && decoder.total_in() as usize == body.len() - 4,
            "invalid compressed update"
        );
        return decode(0x00a9, &output);
    }
    let mut body = Body(body);
    let mut fields = BTreeMap::new();
    match opcode {
        0x02c2 => {
            fields.insert("map".into(), Field::Unsigned(body.u32()?.into()));
            fields.insert("zone".into(), Field::Unsigned(body.u32()?.into()));
            let count = u16::from_le_bytes(body.bytes()?);
            ensure!(
                usize::from(count) <= body.0.len() / 8,
                "invalid world-state count"
            );
            let states = (0..count)
                .map(|_| {
                    Ok(Field::Object(BTreeMap::from([
                        ("id".into(), Field::Unsigned(body.u32()?.into())),
                        (
                            "value".into(),
                            Field::Signed(i32::from_le_bytes(body.bytes()?).into()),
                        ),
                    ])))
                })
                .collect::<Result<Vec<_>>>()?;
            fields.insert("states".into(), Field::List(states));
        }
        0x02f4 => {
            fields.insert("weather_type".into(), Field::Unsigned(body.u32()?.into()));
            fields.insert("grade".into(), body.float()?);
            fields.insert("sound".into(), Field::Unsigned(body.u32()?.into()));
            ensure!(
                fields["weather_type"] != Field::Unsigned(0)
                    || (fields["grade"] == Field::Float(0.0)
                        && fields["sound"] == Field::Unsigned(0)),
                "weather type 0 requires grade and sound 0"
            );
            let instant = body.u8()?;
            ensure!(instant <= 1, "invalid weather instant-change flag");
            fields.insert("instant".into(), Field::Unsigned(instant.into()));
        }
        0x00c7 => {
            fields.insert("guid".into(), body.packed_guid()?);
            fields.insert("counter".into(), Field::Unsigned(body.u32()?.into()));
            fields.extend(body.movement_info()?.1);
        }
        0x00c5 => {
            fields.insert("guid".into(), body.packed_guid()?);
            fields.extend(body.movement_info()?.1);
        }
        0x013c => {
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
            fields.insert("reaction".into(), Field::Unsigned(body.u32()?.into()));
        }
        0x0143 => {
            fields.insert(
                "attacker".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
            fields.insert(
                "victim".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
        }
        0x0144 => {
            fields.insert("attacker".into(), body.packed_guid()?);
            fields.insert("victim".into(), body.packed_guid()?);
            fields.insert("dead".into(), Field::Unsigned(body.u32()?.into()));
        }
        0x0301 => {
            fields.insert("guid".into(), body.packed_guid()?);
            fields.insert("speed".into(), body.float()?);
        }
        0x030d | 0x030e => {
            fields.insert("guid".into(), body.packed_guid()?);
        }
        0x014e => (),
        0x016e | 0x016f => {
            fields.insert("result".into(), Field::Unsigned(body.u32()?.into()));
        }
        0x0103 => {
            fields.insert("emote".into(), Field::Unsigned(body.u32()?.into()));
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
        }
        0x0131 | 0x0132 => {
            fields.insert("cast_item".into(), body.packed_guid()?);
            fields.insert("caster".into(), body.packed_guid()?);
            fields.insert("spell".into(), Field::Unsigned(body.u32()?.into()));
            let flags = u16::from_le_bytes(body.bytes()?);
            fields.insert("flags".into(), Field::Unsigned(flags.into()));
            if opcode == 0x0131 {
                fields.insert("duration".into(), Field::TimeMs(body.u32()?.into()));
            } else {
                let hits = body.u8()?;
                fields.insert(
                    "hits".into(),
                    Field::List(
                        (0..hits)
                            .map(|_| Ok(Field::Guid(u64::from_le_bytes(body.bytes()?))))
                            .collect::<Result<_>>()?,
                    ),
                );
                let misses = body.u8()?;
                let mut values = Vec::new();
                for _ in 0..misses {
                    let guid = Field::Guid(u64::from_le_bytes(body.bytes()?));
                    let reason = body.u8()?;
                    let mut miss = BTreeMap::from([
                        ("guid".into(), guid),
                        ("reason".into(), Field::Unsigned(reason.into())),
                    ]);
                    if reason == 11 {
                        miss.insert("reflect".into(), Field::Unsigned(body.u8()?.into()));
                    }
                    values.push(Field::Object(miss));
                }
                fields.insert("misses".into(), Field::List(values));
            }
            fields.insert("targets".into(), body.targets()?);
            if flags & 0x20 != 0 {
                fields.insert("ammo_display".into(), Field::Unsigned(body.u32()?.into()));
                fields.insert("ammo_type".into(), Field::Unsigned(body.u32()?.into()));
            }
        }

        0x01ab => {
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
            let known = body.u8()?;
            ensure!(known <= 1, "invalid taxi node status");
            fields.insert("known".into(), Field::Unsigned(known.into()));
        }
        0x01a9 => {
            let show = body.u32()?;
            ensure!(show <= 1, "invalid taxi menu state");
            fields.insert("show".into(), Field::Unsigned(show.into()));
            if show != 0 {
                fields.insert(
                    "guid".into(),
                    Field::Guid(u64::from_le_bytes(body.bytes()?)),
                );
                fields.insert("node".into(), Field::Unsigned(body.u32()?.into()));
                fields.insert(
                    "known_nodes".into(),
                    Field::List(
                        (0..8)
                            .map(|_| body.u32().map(|v| Field::Unsigned(v.into())))
                            .collect::<Result<_>>()?,
                    ),
                );
            }
        }
        0x01ae => {
            fields.insert("result".into(), Field::Unsigned(body.u32()?.into()));
        }
        0x01af => (),
        0x00dd => {
            fields.insert("guid".into(), body.packed_guid()?);
            fields.insert("position".into(), body.vector()?);
            fields.insert("spline_id".into(), Field::Unsigned(body.u32()?.into()));
            let kind = body.u8()?;
            fields.insert("kind".into(), Field::Unsigned(kind.into()));
            match kind {
                0 | 1 => (),
                2 => {
                    fields.insert("facing_point".into(), body.vector()?);
                }
                3 => {
                    fields.insert(
                        "facing_target".into(),
                        Field::Guid(u64::from_le_bytes(body.bytes()?)),
                    );
                }
                4 => {
                    fields.insert("facing_angle".into(), body.float()?);
                }
                _ => bail!("invalid monster move kind"),
            }
            if kind != 1 {
                let flags = body.u32()?;
                fields.insert("flags".into(), Field::Unsigned(flags.into()));
                fields.insert("duration".into(), Field::TimeMs(body.u32()?.into()));
                if flags & 0x200 != 0 {
                    fields.insert("points".into(), body.vectors()?);
                } else {
                    let count = body.u32()?;
                    ensure!((1..=4096).contains(&count), "invalid linear spline count");
                    fields.insert("destination".into(), body.vector()?);
                    fields.insert(
                        "offsets".into(),
                        Field::List(
                            (1..count)
                                .map(|_| body.u32().map(|v| Field::Unsigned(v.into())))
                                .collect::<Result<_>>()?,
                        ),
                    );
                }
            }
        }
        0x00a9 => {
            let count = body.u32()?;
            ensure!(
                count <= 4096 && count as usize <= body.0.len(),
                "invalid object count"
            );
            fields.insert("has_transport".into(), Field::Unsigned(body.u8()?.into()));
            let mut objects = Vec::new();
            for _ in 0..count {
                let kind = body.u8()?;
                let mut object = BTreeMap::from([("kind".into(), Field::Unsigned(kind.into()))]);
                match kind {
                    0..=3 => {
                        object.insert("guid".into(), body.packed_guid()?);
                        if kind >= 2 {
                            object.insert("object_type".into(), Field::Unsigned(body.u8()?.into()));
                        }
                        if kind != 0 {
                            object.insert("movement".into(), body.movement()?);
                        }
                        if kind != 1 {
                            object.insert("values".into(), body.values()?);
                        }
                    }
                    4 | 5 => {
                        let count = body.u32()?;
                        ensure!(
                            count <= 4096 && count as usize <= body.0.len(),
                            "invalid object GUID count"
                        );
                        object.insert(
                            "guids".into(),
                            Field::List(
                                (0..count)
                                    .map(|_| body.packed_guid())
                                    .collect::<Result<_>>()?,
                            ),
                        );
                    }
                    _ => bail!("unknown object update kind"),
                }
                objects.push(Field::Object(object));
            }
            fields.insert("objects".into(), Field::List(objects));
        }
        0x0156 => {
            fields.insert("guid".into(), body.packed_guid()?);
            fields.insert("enabled".into(), Field::Unsigned(body.u8()?.into()));
        }
        0x00aa | 0x0215 => {
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
        }
        0x01b8 => {
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
        }
        0x01ba => {
            fields.insert("result".into(), Field::Unsigned(body.u32()?.into()));
        }

        0x01dd => {
            fields.insert("sequence".into(), Field::Unsigned(body.u32()?.into()));
        }
        0x01cf => {
            fields.insert(
                "time".into(),
                Field::WallClockMs(u64::from(body.u32()?) * 1000),
            );
        }
        0x0236 => {
            fields.insert("map".into(), Field::Unsigned(body.u32()?.into()));
            for name in ["x", "y", "z", "orientation"] {
                fields.insert(name.into(), body.float()?);
            }
        }
        0x01fb => {
            for name in ["minimum", "maximum", "result"] {
                fields.insert(name.into(), Field::Unsigned(body.u32()?.into()));
            }
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
        }
        0x0051 => {
            fields.insert(
                "guid".into(),
                Field::Guid(u64::from_le_bytes(body.bytes()?)),
            );
            fields.insert("name".into(), Field::Text(body.text()?));
            fields.insert("realm".into(), Field::Text(body.text()?));
            for name in ["race", "gender", "class"] {
                fields.insert(name.into(), Field::Unsigned(body.u32()?.into()));
            }
        }
        _ => bail!("unsupported conformance opcode {opcode:#06x}"),
    }
    if !body.0.is_empty() {
        bail!("trailing bytes in opcode {opcode:#06x}");
    }
    Ok(Packet { opcode, fields })
}
