//! Apple bridge for the existing Rust Noise handshake and frame codec.
//! The Swift caller owns the network connection and serializes access to each state.
use crate::collect::subscription_request;
use crate::chat_send::{start_session_request, validate_ack, AckReject};
use crate::gateway::is_owner_rv_challenge;
use crate::noise::NoiseXXInitiator;
use crate::protobuf::PbWriter;
use crate::transport::{HttpRequest, InboundFrame, NoiseTransport, Service};

enum Wire {
  Handshake(NoiseXXInitiator),
  Transport(NoiseTransport),
  Closed,
}

fn boxed(bytes: Vec<u8>) -> *mut Vec<u8> {
  Box::into_raw(Box::new(bytes))
}

fn encode(transport: &mut NoiseTransport, request: HttpRequest) -> *mut Vec<u8> {
  let Ok((stream, frames)) = transport.encrypt_request(&request) else { return std::ptr::null_mut() };
  let mut data = stream.to_le_bytes().to_vec();
  for frame in frames {
    data.extend((frame.len() as u32).to_le_bytes());
    data.extend(frame);
  }
  boxed(data)
}

fn approval_route(path: &str) -> bool {
  path.strip_prefix("/approvals/").is_some_and(|id| {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
  })
}

#[no_mangle]
extern "C" fn diehao_wire_new() -> *mut Wire {
  Box::into_raw(Box::new(Wire::Handshake(NoiseXXInitiator::new())))
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_free(wire: *mut Wire) {
  if !wire.is_null() {
    drop(Box::from_raw(wire));
  }
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_message_one(wire: *mut Wire) -> *mut Vec<u8> {
  let Some(Wire::Handshake(state)) = wire.as_mut() else { return std::ptr::null_mut() };
  let mut nonce = [0u8; 32];
  if getrandom::fill(&mut nonce).is_err() {
    return std::ptr::null_mut();
  }
  let mut payload = PbWriter::new();
  payload.bytes(1, &nonce);
  state.write_message1(&payload.finish()).map(boxed).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_message_three(wire: *mut Wire, bytes: *const u8, length: usize) -> *mut Vec<u8> {
  let Some(wire) = wire.as_mut() else { return std::ptr::null_mut() };
  let Wire::Handshake(mut state) = std::mem::replace(wire, Wire::Closed) else { return std::ptr::null_mut() };
  if bytes.is_null() || length == 0 || length > 65_535 {
    return std::ptr::null_mut();
  }
  let Ok(challenge) = state.read_message2(std::slice::from_raw_parts(bytes, length)) else { return std::ptr::null_mut() };
  if is_owner_rv_challenge(&challenge) {
    return std::ptr::null_mut();
  }
  let Ok(msg) = state.write_message3(&[]) else { return std::ptr::null_mut() };
  let Ok((send, receive)) = state.split() else { return std::ptr::null_mut() };
  *wire = Wire::Transport(NoiseTransport::new(send, receive));
  boxed(msg)
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_request(wire: *mut Wire, route: *const u8, length: usize) -> *mut Vec<u8> {
  let Some(Wire::Transport(transport)) = wire.as_mut() else { return std::ptr::null_mut() };
  if route.is_null() || length == 0 || length > 2_048 {
    return std::ptr::null_mut();
  }
  let Ok(path) = std::str::from_utf8(std::slice::from_raw_parts(route, length)) else { return std::ptr::null_mut() };
  let request = match path {
    "/chat/subscribe" => subscription_request(),
    "/session/start" => start_session_request(),
    path if path == "/api/session/list" || path.starts_with("/chat/history?")
        || path == "/approvals" || approval_route(path) => HttpRequest {
      service: if path.starts_with("/approvals") { Service::Sentinel } else { Service::Daemon },
      verb: "GET".to_owned(),
      path: path.to_owned(),
      headers: vec![
        ("x-app-id".to_owned(), "hatch-web".to_owned()),
        ("Accept-Language".to_owned(), "en-US".to_owned()),
        ("x-request-id".to_owned(), uuid::Uuid::new_v4().to_string()),
      ],
      body: Vec::new(),
    },
    _ => return std::ptr::null_mut(),
  };
  encode(transport, request)
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_post(
  wire: *mut Wire, route: *const u8, route_length: usize, body: *const u8, body_length: usize,
) -> *mut Vec<u8> {
  let Some(Wire::Transport(transport)) = wire.as_mut() else { return std::ptr::null_mut() };
  if route.is_null() || route_length == 0 || route_length > 160
      || body.is_null() || body_length == 0 || body_length > 524_288 {
    return std::ptr::null_mut();
  }
  let Ok(path) = std::str::from_utf8(std::slice::from_raw_parts(route, route_length)) else { return std::ptr::null_mut() };
  let service = if path == "/chat/stream" {
    Service::Daemon
  } else if body_length <= 4096 && path.strip_suffix("/decide").is_some_and(approval_route) {
    Service::Sentinel
  } else {
    return std::ptr::null_mut();
  };
  encode(transport, HttpRequest {
    service,
    verb: "POST".to_owned(),
    path: path.to_owned(),
    headers: vec![("Content-Type".to_owned(), "application/json".to_owned()), ("x-app-id".to_owned(), "hatch-web".to_owned())],
    body: std::slice::from_raw_parts(body, body_length).to_vec(),
  })
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_chat_ack(bytes: *const u8, length: usize) -> *mut Vec<u8> {
  if bytes.is_null() || length == 0 || length > 1_048_576 {
    return std::ptr::null_mut();
  }
  let Ok(value) = serde_json::from_slice::<serde_json::Value>(std::slice::from_raw_parts(bytes, length)) else {
    return boxed(Vec::new());
  };
  if value["type"] == "event" {
    return boxed(Vec::new());
  }
  let value = match validate_ack(&value, None) {
    Ok(ack) => serde_json::json!({"message": ack.message_id, "thread": ack.session_id}),
    Err(AckReject::NotAck) => return boxed(Vec::new()),
    Err(AckReject::Error(_)) => serde_json::json!({"rejected":true}),
  };
  serde_json::to_vec(&value).map(boxed).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_receive(wire: *mut Wire, bytes: *const u8, length: usize) -> *mut Vec<u8> {
  let Some(Wire::Transport(transport)) = wire.as_mut() else { return std::ptr::null_mut() };
  if bytes.is_null() || length == 0 || length > 65_535 {
    return std::ptr::null_mut();
  }
  let Ok(result) = transport.decrypt(std::slice::from_raw_parts(bytes, length)) else { return std::ptr::null_mut() };
  let Some(frame) = result else { return boxed(Vec::new()) };
  let (kind, stream, status, end, body) = match frame {
    InboundFrame::Response { stream_id, status, end_body, body, .. } => (1, stream_id, status, end_body, body),
    InboundFrame::BodyChunk { stream_id, data, end_body } => (2, stream_id, 0, end_body, data),
    InboundFrame::Reset { stream_id, .. } => (3, stream_id, 0, true, Vec::new()),
  };
  let mut data = Vec::with_capacity(body.len() + 12);
  data.push(kind);
  data.extend(stream.to_le_bytes());
  data.extend(status.to_le_bytes());
  data.push(u8::from(end));
  data.extend(body);
  boxed(data)
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_blob_read(blob: *const Vec<u8>, length: *mut usize) -> *const u8 {
  match (blob.as_ref(), length.as_mut()) {
    (Some(blob), Some(length)) => {
      *length = blob.len();
      blob.as_ptr()
    }
    _ => std::ptr::null(),
  }
}

#[no_mangle]
unsafe extern "C" fn diehao_wire_blob_free(blob: *mut Vec<u8>) {
  if !blob.is_null() {
    drop(Box::from_raw(blob));
  }
}
