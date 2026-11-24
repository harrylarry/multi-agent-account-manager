//! Opt-in metadata captured before subscription routing. Never exports a body or identifier from JSON.
use crate::transport::InboundFrame;
use std::{
  collections::HashMap,
  time::{SystemTime, UNIX_EPOCH},
};

const MAX_ROWS: usize = 512;
const MAX_LINE: usize = 64 * 1024;
const MAX_STREAMS: usize = 8;

#[derive(Default)]
struct LineBuffer {
  bytes: Vec<u8>,
  overflow: bool,
}

#[derive(Default)]
pub(crate) struct WireTrace {
  methods: HashMap<u64, &'static str>,
  buffers: HashMap<u64, LineBuffer>,
  rows: Vec<String>,
  seq: u64,
  dropped: usize,
}

impl WireTrace {
  pub fn request(&mut self, id: u64, method: &'static str) {
    self.methods.insert(id, method);
    self.record(id, method, "request", "-", 0, false, "-", "-");
  }

  pub fn frame(&mut self, id: u64, frame: &InboundFrame, route: &str) {
    let method = self.methods.get(&id).copied().unwrap_or("unknown");
    let (kind, body, end, status) = match frame {
      InboundFrame::Response {
        body,
        end_body,
        status,
        ..
      } => ("response", body.as_slice(), *end_body, status.to_string()),
      InboundFrame::BodyChunk { data, end_body, .. } => {
        ("bodyChunk", data.as_slice(), *end_body, "-".into())
      }
      InboundFrame::Reset { code, .. } => ("reset", &[][..], true, code.to_string()),
    };
    self.record(id, method, kind, route, body.len(), end, &status, "-");
    if kind != "reset" && (!body.is_empty() || end) {
      if let Some(mut line) = self
        .buffers
        .remove(&id)
        .or_else(|| (self.buffers.len() < MAX_STREAMS).then(LineBuffer::default))
      {
        let mut chunks = body.split(|b| *b == b'\n').peekable();
        while let Some(chunk) = chunks.next() {
          if chunk.len() > MAX_LINE - line.bytes.len() {
            line.overflow = true;
          } else if !line.overflow {
            line.bytes.extend_from_slice(chunk);
          }
          if chunks.peek().is_some() || end {
            if line.overflow || !line.bytes.iter().all(u8::is_ascii_whitespace) {
              let event = if line.overflow {
                "oversize"
              } else {
                event_name(&line.bytes)
              };
              self.record(id, method, kind, route, 0, end, &status, event);
            }
            line.bytes.clear();
            line.overflow = false;
          }
        }
        if !end && (line.overflow || !line.bytes.is_empty()) {
          self.buffers.insert(id, line);
        }
      } else {
        self.record(id, method, kind, route, 0, end, &status, "buffer_limit");
      }
    }
    if end {
      self.buffers.remove(&id);
    }
  }

  #[allow(clippy::too_many_arguments)]
  fn record(
    &mut self,
    id: u64,
    method: &str,
    frame: &str,
    route: &str,
    bytes: usize,
    end: bool,
    status: &str,
    event: &str,
  ) {
    self.seq += 1;
    if self.rows.len() >= MAX_ROWS {
      self.dropped += 1;
      return;
    }
    let ms = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap_or_default()
      .as_millis();
    self.rows.push(format!(
      "MUSE_WIRE_TRACE {ms} 1 {} {method} {id} {frame} {route} {bytes} {end} {status} {event}",
      self.seq
    ));
  }

  pub fn take(&mut self) -> Vec<String> {
    let mut rows = std::mem::take(&mut self.rows);
    if self.dropped > 0 {
      rows.push(format!(
        "MUSE_WIRE_TRACE_DROPPED {}",
        std::mem::take(&mut self.dropped)
      ));
    }
    rows
  }
}

fn event_name(bytes: &[u8]) -> &'static str {
  let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
    return "invalid_json";
  };
  let Some(object) = value.as_object() else {
    return "non_object";
  };
  let name = object
    .get("event_name")
    .and_then(|v| v.as_str())
    .filter(|s| !s.is_empty())
    .or_else(|| {
      object
        .get("event")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    });
  match name.or_else(|| object.get("type").and_then(|v| v.as_str())) {
    Some("delta.message_start") => "delta.message_start",
    Some("delta.text_append") => "delta.text_append",
    Some("delta.message_done") => "delta.message_done",
    Some("delta.placeholder") => "delta.placeholder",
    Some("delta.presentation") => "delta.presentation",
    Some("message.user") => "message.user",
    Some("message.assistant") => "message.assistant",
    Some("activity") => "activity",
    Some("agent.activity") => "agent.activity",
    Some("agent.status") => "agent.status",
    Some("event") if name.is_none() => "event_without_name",
    Some(_) => "other",
    None => "object_without_name",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn traces_split_events_on_unrouted_stream_without_body_or_arbitrary_names() {
    let mut trace = WireTrace::default();
    for (data, end_body) in [
      (&b"{\"event_name\":\"delta.text_"[..], false),
      (
        &b"append\",\"payload\":\"secret\"}\n{\"event\":\"untrusted-secret\"}"[..],
        true,
      ),
    ] {
      trace.frame(
        99,
        &InboundFrame::BodyChunk {
          stream_id: 99,
          data: data.to_vec(),
          end_body,
        },
        "unknown",
      );
    }
    let rows = trace.take().join("\n");
    assert!(rows.contains("unknown 99 bodyChunk unknown"));
    assert!(rows.contains("delta.text_append"));
    assert!(rows.ends_with(" other"));
    assert!(!rows.contains("secret"));
  }
}
