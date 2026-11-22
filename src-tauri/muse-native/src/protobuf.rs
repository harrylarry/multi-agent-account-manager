// 最小 proto3 wire 编解码，移植自 BotArk v0.12.0
// src/main/muse-native/protobuf.ts（MIT）。
// 只覆盖 Hatch Noise 消息用到的部分：varint、length-delimited、bool；
// wire 1/5 解码时跳过，不支持的 wire type 报错。

use std::fmt;

/// 一个已解码的 proto 字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PbField {
  pub field: u32,
  pub wire: u32,
  /// wire 0 的 varint 值。
  pub int: Option<u64>,
  /// wire 2 的 length-delimited 值。
  pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtoError {
  TruncatedVarint,
  VarintTooLong,
  FieldZero,
  TruncatedBytes,
  UnsupportedWire(u32),
}

impl fmt::Display for ProtoError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ProtoError::TruncatedVarint => write!(f, "pb: truncated varint"),
      ProtoError::VarintTooLong => write!(f, "pb: varint too long"),
      ProtoError::FieldZero => write!(f, "pb: field 0"),
      ProtoError::TruncatedBytes => write!(f, "pb: truncated bytes"),
      ProtoError::UnsupportedWire(w) => write!(f, "pb: unsupported wire type {w}"),
    }
  }
}

impl std::error::Error for ProtoError {}

/// 与 BotArk PbWriter 对应的写入器。
#[derive(Debug, Default, Clone)]
pub struct PbWriter {
  parts: Vec<u8>,
}

impl PbWriter {
  pub fn new() -> Self {
    Self::default()
  }

  fn varint(&mut self, v: u64) {
    let mut x = v;
    while x > 0x7f {
      self.parts.push(((x & 0x7f) | 0x80) as u8);
      x >>= 7;
    }
    self.parts.push(x as u8);
  }

  fn tag(&mut self, field: u32, wire: u32) {
    self.varint(((field << 3) | wire) as u64);
  }

  pub fn uint(&mut self, field: u32, v: u64) -> &mut Self {
    self.tag(field, 0);
    self.varint(v);
    self
  }

  pub fn bool(&mut self, field: u32, v: bool) -> &mut Self {
    self.uint(field, u64::from(v))
  }

  pub fn bytes(&mut self, field: u32, v: &[u8]) -> &mut Self {
    self.tag(field, 2);
    self.varint(v.len() as u64);
    self.parts.extend_from_slice(v);
    self
  }

  pub fn string(&mut self, field: u32, v: &str) -> &mut Self {
    self.bytes(field, v.as_bytes())
  }

  pub fn finish(&self) -> Vec<u8> {
    self.parts.clone()
  }
}

/// 解码一段扁平字段列表；输入畸形时报错。
pub fn pb_decode(buf: &[u8]) -> Result<Vec<PbField>, ProtoError> {
  let mut out = Vec::new();
  let mut pos = 0usize;
  let read_varint = |pos: &mut usize| -> Result<u64, ProtoError> {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    loop {
      if *pos >= buf.len() {
        return Err(ProtoError::TruncatedVarint);
      }
      let b = buf[*pos];
      *pos += 1;
      result |= ((b & 0x7f) as u64) << shift;
      if b & 0x80 == 0 {
        return Ok(result);
      }
      shift += 7;
      if shift > 70 {
        return Err(ProtoError::VarintTooLong);
      }
    }
  };
  while pos < buf.len() {
    let key = read_varint(&mut pos)?;
    let field = (key >> 3) as u32;
    let wire = (key & 7) as u32;
    if field == 0 {
      return Err(ProtoError::FieldZero);
    }
    match wire {
      0 => out.push(PbField {
        field,
        wire,
        int: Some(read_varint(&mut pos)?),
        bytes: None,
      }),
      2 => {
        let len = read_varint(&mut pos)? as usize;
        if pos + len > buf.len() {
          return Err(ProtoError::TruncatedBytes);
        }
        out.push(PbField {
          field,
          wire,
          int: None,
          bytes: Some(buf[pos..pos + len].to_vec()),
        });
        pos += len;
      }
      1 => pos += 8,
      5 => pos += 4,
      w => return Err(ProtoError::UnsupportedWire(w)),
    }
    // wire 1/5 只跳过，不产生字段（与 BotArk pbDecode 一致）。
    if pos > buf.len() {
      return Err(ProtoError::TruncatedBytes);
    }
  }
  Ok(out)
}

/// 取第一个指定 field 的字段。
pub fn pb_get(fields: &[PbField], n: u32) -> Option<&PbField> {
  fields.iter().find(|f| f.field == n)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn round_trip_and_known_bytes() {
    // BotArk 风格：string(1, "POST").string(2, "/api/ping").bool(5, true)
    let mut w = PbWriter::new();
    w.string(1, "POST").string(2, "/api/ping").bool(5, true);
    let bytes = w.finish();
    // 0x0a = field 1 wire 2, 0x12 = field 2 wire 2, 0x28 = field 5 wire 0.
    assert_eq!(bytes[0], 0x0a);
    assert_eq!(bytes[1], 4);
    assert_eq!(&bytes[2..6], b"POST");
    assert_eq!(bytes[6], 0x12);
    assert_eq!(bytes[7], 9);
    assert_eq!(&bytes[8..17], b"/api/ping");
    assert_eq!(&bytes[17..], &[0x28, 0x01]);

    let fields = pb_decode(&bytes).expect("decode");
    assert_eq!(
      pb_get(&fields, 1).unwrap().bytes.as_deref(),
      Some(b"POST".as_slice())
    );
    assert_eq!(
      pb_get(&fields, 2).unwrap().bytes.as_deref(),
      Some(b"/api/ping".as_slice())
    );
    assert_eq!(pb_get(&fields, 5).unwrap().int, Some(1));
    assert!(pb_get(&fields, 9).is_none());
  }

  #[test]
  fn varint_boundaries() {
    for v in [0u64, 1, 127, 128, 300, u64::MAX] {
      let mut w = PbWriter::new();
      w.uint(1, v);
      let fields = pb_decode(&w.finish()).expect("decode");
      assert_eq!(pb_get(&fields, 1).unwrap().int, Some(v), "varint {v}");
    }
  }

  #[test]
  fn malformed_inputs_error() {
    assert!(matches!(
      pb_decode(&[0x08]),
      Err(ProtoError::TruncatedVarint)
    ));
    assert!(matches!(pb_decode(&[0x00]), Err(ProtoError::FieldZero)));
    assert!(matches!(
      pb_decode(&[0x0a, 0x05, 0x01]),
      Err(ProtoError::TruncatedBytes)
    ));
    assert!(matches!(
      pb_decode(&[0x0b, 0x00]),
      Err(ProtoError::UnsupportedWire(3))
    ));
  }
}
