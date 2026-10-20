//! Reader for Photoshop "ActionDescriptor" structures, as found in the `desc`
//! section of v6+ `.abr` files. Follows the descriptor layout in Adobe's
//! published Photoshop File Formats Specification.

use std::collections::BTreeMap;

use crate::types::BrushIoError;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Descriptor(Descriptor),
    List(Vec<Value>),
    Double(f64),
    /// Unit (`#Pxl`, `#Prc`, `#Ang`, ...) and value.
    Unit(String, f64),
    Text(String),
    Enum(String, String),
    Integer(i64),
    Bool(bool),
    Class(String),
    Raw(Vec<u8>),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Descriptor {
    pub class: String,
    pub items: BTreeMap<String, Value>,
}

impl Descriptor {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.get(key)
    }

    pub fn descriptor(&self, key: &str) -> Option<&Descriptor> {
        match self.get(key)? {
            Value::Descriptor(d) => Some(d),
            _ => None,
        }
    }

    pub fn list(&self, key: &str) -> Option<&[Value]> {
        match self.get(key)? {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    /// A numeric value regardless of its encoding (unit, double or integer).
    pub fn number(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            Value::Double(v) | Value::Unit(_, v) => Some(*v),
            Value::Integer(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn text(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn flag(&self, key: &str) -> Option<bool> {
        match self.get(key)? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

pub(crate) struct Cursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8], BrushIoError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| BrushIoError::Corrupt(format!("unexpected end of data at byte {}", self.pos)))?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, BrushIoError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, BrushIoError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn i16(&mut self) -> Result<i16, BrushIoError> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, BrushIoError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn i32(&mut self) -> Result<i32, BrushIoError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f64(&mut self) -> Result<f64, BrushIoError> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// `u32` count of UTF-16BE code units, then the units (trailing NULs dropped).
    pub fn unicode(&mut self) -> Result<String, BrushIoError> {
        let n = self.u32()? as usize;
        if n > self.remaining() / 2 {
            return Err(BrushIoError::Corrupt("unicode string longer than its section".into()));
        }
        let units: Vec<u16> = self.take(n * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
        Ok(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
    }

    /// A key / class ID: `u32` length, or 0 for a 4-byte code.
    pub fn id(&mut self) -> Result<String, BrushIoError> {
        let n = self.u32()? as usize;
        let n = if n == 0 { 4 } else { n };
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
}

/// Reads a descriptor body (class name, class ID, items).
pub(crate) fn read_descriptor(c: &mut Cursor<'_>) -> Result<Descriptor, BrushIoError> {
    let _name = c.unicode()?;
    let class = c.id()?;
    let count = c.u32()? as usize;
    let mut items = BTreeMap::new();
    for _ in 0..count {
        let key = c.id()?;
        let value = read_value(c)?;
        items.insert(key, value);
    }
    Ok(Descriptor { class, items })
}

fn read_value(c: &mut Cursor<'_>) -> Result<Value, BrushIoError> {
    let tag: [u8; 4] = c.take(4)?.try_into().unwrap();
    Ok(match &tag {
        b"Objc" | b"GlbO" => Value::Descriptor(read_descriptor(c)?),
        b"VlLs" => {
            let n = c.u32()? as usize;
            let mut list = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                list.push(read_value(c)?);
            }
            Value::List(list)
        }
        b"doub" => Value::Double(c.f64()?),
        b"UntF" => {
            let unit = String::from_utf8_lossy(c.take(4)?).into_owned();
            Value::Unit(unit, c.f64()?)
        }
        b"UnFl" => {
            let unit = String::from_utf8_lossy(c.take(4)?).into_owned();
            let n = c.u32()? as usize;
            let mut first = 0.0;
            for i in 0..n {
                let v = c.f64()?;
                if i == 0 {
                    first = v;
                }
            }
            Value::Unit(unit, first)
        }
        b"TEXT" => Value::Text(c.unicode()?),
        b"enum" => Value::Enum(c.id()?, c.id()?),
        b"long" => Value::Integer(c.i32()? as i64),
        b"comp" => Value::Integer(i64::from_be_bytes(c.take(8)?.try_into().unwrap())),
        b"bool" => Value::Bool(c.u8()? != 0),
        b"type" | b"GlbC" => {
            let _name = c.unicode()?;
            Value::Class(c.id()?)
        }
        b"alis" | b"tdta" | b"Pth " => {
            let n = c.u32()? as usize;
            Value::Raw(c.take(n)?.to_vec())
        }
        other => {
            return Err(BrushIoError::Unsupported(format!(
                "descriptor value type {:?}",
                String::from_utf8_lossy(other)
            )))
        }
    })
}

#[cfg(test)]
pub(crate) mod build {
    //! Writers used by tests to build descriptor bytes.

    pub fn unicode(out: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        out.extend((units.len() as u32).to_be_bytes());
        for u in units {
            out.extend(u.to_be_bytes());
        }
    }

    pub fn id(out: &mut Vec<u8>, s: &str) {
        if s.len() == 4 {
            out.extend(0u32.to_be_bytes());
        } else {
            out.extend((s.len() as u32).to_be_bytes());
        }
        out.extend(s.as_bytes());
    }

    pub enum V<'a> {
        Obj(&'a str, Vec<(&'a str, V<'a>)>),
        List(Vec<V<'a>>),
        Unit(&'a str, f64),
        Text(&'a str),
        Long(i32),
        Bool(bool),
    }

    pub fn descriptor(out: &mut Vec<u8>, class: &str, items: &[(&str, V<'_>)]) {
        unicode(out, "");
        id(out, class);
        out.extend((items.len() as u32).to_be_bytes());
        for (key, value) in items {
            id(out, key);
            write(out, value);
        }
    }

    pub fn write(out: &mut Vec<u8>, value: &V<'_>) {
        match value {
            V::Obj(class, items) => {
                out.extend(b"Objc");
                descriptor(out, class, items);
            }
            V::List(items) => {
                out.extend(b"VlLs");
                out.extend((items.len() as u32).to_be_bytes());
                for item in items {
                    write(out, item);
                }
            }
            V::Unit(unit, v) => {
                out.extend(b"UntF");
                out.extend(unit.as_bytes());
                out.extend(v.to_be_bytes());
            }
            V::Text(t) => {
                out.extend(b"TEXT");
                unicode(out, t);
            }
            V::Long(v) => {
                out.extend(b"long");
                out.extend(v.to_be_bytes());
            }
            V::Bool(b) => {
                out.extend(b"bool");
                out.push(u8::from(*b));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    #[test]
    fn reads_nested_values() {
        let mut bytes = Vec::new();
        descriptor(
            &mut bytes,
            "null",
            &[
                ("Nm  ", V::Text("Chalk ✓")),
                ("Dmtr", V::Unit("#Pxl", 25.0)),
                ("Brsh", V::List(vec![V::Obj("sampledBrush", vec![("Spcn", V::Unit("#Prc", 15.0)), ("Intr", V::Bool(true))])])),
                ("bVTy", V::Long(2)),
                ("longKeyName", V::Bool(false)),
            ],
        );
        let d = read_descriptor(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(d.text("Nm  "), Some("Chalk ✓"));
        assert_eq!(d.number("Dmtr"), Some(25.0));
        assert_eq!(d.number("bVTy"), Some(2.0));
        assert_eq!(d.flag("longKeyName"), Some(false));
        let Value::Descriptor(inner) = &d.list("Brsh").unwrap()[0] else { panic!() };
        assert_eq!(inner.class, "sampledBrush");
        assert_eq!(inner.number("Spcn"), Some(15.0));
    }
}
