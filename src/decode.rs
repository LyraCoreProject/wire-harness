use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};

#[derive(Clone, Debug, PartialEq)]
pub enum Field {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Guid(u64),
    TimeMs(u64),
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
    let mut body = Body(body);
    let mut fields = BTreeMap::new();
    match opcode {
        0x01dd => {
            fields.insert("sequence".into(), Field::Unsigned(body.u32()?.into()));
        }
        0x01cf => {
            fields.insert("time".into(), Field::TimeMs(u64::from(body.u32()?) * 1000));
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
