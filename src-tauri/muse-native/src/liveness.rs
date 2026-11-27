//! Passive WSS observations, NOT proof of continuous connectivity while iOS is suspended.
//! Wall-clock deadline is checked whenever execution resumes; no claim is made about
//! what happened in an interval in which this task was not polled.
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Liveness {
  pub running: bool,
  pub phase: String,
  pub started_ms: u64,
  pub connected_ms: Option<u64>,
  pub subscription_ready_ms: Option<u64>,
  pub deadline_ms: Option<u64>,
  pub observed_at_ms: u64,
  pub max_observation_gap_ms: u64,
  pub last_event_ms: Option<u64>,
  pub ended_ms: Option<u64>,
  pub lines: u64,
  pub events: u64,
  pub end_reason: Option<String>,
  pub error: Option<String>,
}

impl Liveness {
  pub fn started(now: u64) -> Self {
    Self {
      running: true,
      phase: "cookies".into(),
      started_ms: now,
      observed_at_ms: now,
      ..Self::default()
    }
  }

  pub fn observe(&mut self, now: u64) {
    self.max_observation_gap_ms = self
      .max_observation_gap_ms
      .max(now.saturating_sub(self.observed_at_ms));
    self.observed_at_ms = now;
  }

  pub fn connected(&mut self, now: u64) {
    self.observe(now);
    self.connected_ms = Some(now);
    self.phase = "subscribe".into();
  }

  pub fn reading(&mut self, now: u64, seconds: u64) {
    self.observe(now);
    self.subscription_ready_ms = Some(now);
    self.deadline_ms = Some(now.saturating_add(seconds.saturating_mul(1000)));
    self.phase = "reading".into();
  }

  /// A deadline reached after an observation gap is still a WINDOW end,
  /// not a server disconnect at the requested deadline.
  pub fn expired(&self, now: u64) -> bool {
    self.deadline_ms.is_some_and(|end| now >= end)
  }

  pub fn record_line(&mut self, now: u64, is_event: bool) {
    self.observe(now);
    self.lines += 1;
    if is_event {
      self.events += 1;
      self.last_event_ms = Some(now);
    }
  }

  pub fn finish(&mut self, now: u64, reason: &str, error: Option<String>) {
    self.observe(now);
    self.running = false;
    self.phase = "done".into();
    // Detection time, never a claimed timestamp of the remote disconnection.
    self.ended_ms = Some(now);
    self.end_reason = Some(reason.into());
    self.error = error;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn setup_time_is_not_connected_time() {
    let l = Liveness::started(1000);
    assert_eq!(l.connected_ms, None);
    assert_eq!(l.deadline_ms, None);
  }

  #[test]
  fn starts_window_after_subscription_setup() {
    let mut l = Liveness::started(1000);
    l.connected(3000);
    l.reading(5000, 300);
    assert_eq!(l.subscription_ready_ms, Some(5000));
    assert!(!l.expired(304999));
    assert!(l.expired(305000));
  }

  #[test]
  fn resume_after_long_gap_reports_observation_not_continuous_liveness() {
    let mut l = Liveness::started(1000);
    l.connected(1000);
    l.reading(1000, 300);
    l.record_line(4000, true);
    assert!(l.expired(1236000));
    l.finish(1236000, "window_elapsed", None);
    assert_eq!(l.max_observation_gap_ms, 1232000);
    assert_eq!(l.last_event_ms, Some(4000));
    assert_eq!(l.ended_ms, Some(1236000));
    assert_eq!(l.end_reason.as_deref(), Some("window_elapsed"));
  }

  #[test]
  fn ack_does_not_count_as_event() {
    let mut l = Liveness::started(1000);
    l.record_line(2000, false);
    assert_eq!(l.lines, 1);
    assert_eq!(l.events, 0);
    assert_eq!(l.last_event_ms, None);
    l.record_line(3000, true);
    assert_eq!(l.events, 1);
  }

  #[test]
  fn socket_error_is_recorded_at_detection_time() {
    let mut l = Liveness::started(1000);
    l.finish(2000, "read_error", Some("socket disconnected".into()));
    assert!(!l.running);
    assert!(l.error.is_some());
    assert_eq!(l.ended_ms, Some(2000));
  }

  #[test]
  fn clock_rollback_does_not_underflow() {
    let mut l = Liveness::started(1000);
    l.observe(900);
    assert_eq!(l.max_observation_gap_ms, 0);
  }
}
