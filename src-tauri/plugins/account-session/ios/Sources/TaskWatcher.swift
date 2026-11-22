// TaskWatcher.swift — 0.1.15 (detection), 0.1.16 (no system notifications).
//
// Watches every live Muse session — the active one via the identity
// probe cycle, parked ones via a 20s sweep — for a new assistant reply.
// The user's rule: a task is done when a new reply appears. On a
// working→idle transition for a PARKED account the watcher marks the
// account unread in the manager list (avatar dot); Rust also lights
// the aggregate dot on the session "← 账号" button. The ACTIVE
// account's completions are Muse web's own job (hamburger dot etc.) —
// the app never duplicates them, and while the app is in use there
// are no system notifications at all (user decision, 0.1.16). The
// 0.1.15 notification plumbing (posting, authorization, tap routing)
// was removed in 0.1.16 rather than kept as dead code; background
// notifications are a separate, paused phase.
//
// Detection is heuristic: Muse's DOM has no stable message API, so the
// script reports a message-count estimate (best of several common
// selectors), a hash of the conversation tail, the text length, and
// whether a Stop button is visible. The state machine arms only after
// sustained activity — a Stop button seen, or content growing on 2+
// separate checks — so a lone user message never fires; it fires
// after the content is stable for 2 consecutive checks.
//
// Limits (platform design, not choice):
// - Detection only runs while the app is in the foreground. iOS
//   suspends the app in the background, so parked pages stop updating
//   and the sweep timer stops firing. On foreground return the watcher
//   sweeps immediately and can report completions that happened while
//   away (the parked page's DOM shows the finished state).
//
// Main-thread only, like the probe loop and RecentAccountPages.

import Foundation
import UIKit

final class TaskWatcher: NSObject {
  static let shared = TaskWatcher()

  /// The task-state script. iOS-only (no Android counterpart), so it
  /// lives here rather than in Scripts.swift, whose scripts must stay
  /// byte-identical to the Kotlin originals. Returns a JSON string:
  /// { usable, msgCount, tailHash, tailLen, working }.
  static let taskStateScript = #"""
    (() => {
      try {
        const host = (location.hostname || '').toLowerCase();
        if (host !== 'muse.ai' && !host.endsWith('.muse.ai')) {
          return JSON.stringify({ usable: false });
        }
        if (document.readyState !== 'complete') {
          return JSON.stringify({ usable: false });
        }
        const composerEls = Array.from(
          document.querySelectorAll('textarea, [contenteditable="true"]'));
        const hasComposer = composerEls.some((el) => {
          if (el.disabled || el.readOnly) return false;
          const r = el.getBoundingClientRect();
          return r.width > 120 && r.height > 20;
        });
        if (!hasComposer) return JSON.stringify({ usable: false });
        const cands = [
          '[data-message-id]',
          '[data-testid="message"]',
          '[data-testid*="message"]',
          'article',
          '[role="listitem"]',
        ];
        let best = 0;
        for (const sel of cands) {
          try {
            const n = document.querySelectorAll(sel).length;
            if (n > best) best = n;
          } catch (_) {}
        }
        const main = document.querySelector('main') || document.body;
        let txt = ((main.innerText || '').replace(/\s+/g, ' ')).trim();
        // The user's own draft in the composer is part of innerText;
        // strip it so typing never looks like a new reply.
        for (const el of composerEls) {
          const draft = ((el.value !== undefined ? el.value : el.innerText) || '')
            .replace(/\s+/g, ' ').trim();
          if (draft.length > 0) txt = txt.replace(draft, '');
        }
        const tail = txt.slice(-160);
        let h = 0;
        for (let i = 0; i < tail.length; i++) {
          h = ((h * 31) + tail.charCodeAt(i)) | 0;
        }
        let working = false;
        try {
          const btns = document.querySelectorAll('button');
          for (const b of btns) {
            const t = ((b.getAttribute('aria-label') || '') + ' '
              + (b.textContent || '')).toLowerCase();
            if (/\bstop\b/.test(t) || t.indexOf('停止') !== -1) {
              working = true;
              break;
            }
          }
        } catch (_) {}
        return JSON.stringify({
          usable: true, msgCount: best, tailHash: h,
          tailLen: txt.length, working: working,
        });
      } catch (_) {
        return JSON.stringify({ usable: false });
      }
    })()
  """#

  private struct WatchState {
    var lastSignature: String?
    var lastTailLen: Int = 0
    var lastMsgCount: Int = 0
    var growthEvents: Int = 0
    var sawStopButton: Bool = false
    var stableCount: Int = 0
    var baselineSet: Bool = false
  }

  private struct TaskSample {
    let msgCount: Int
    let tailHash: Int
    let tailLen: Int
    let working: Bool
  }

  private static let sweepInterval: TimeInterval = 20
  private static let stableChecksToFire = 2

  private var started = false
  private var sweepTimer: Timer?
  private var foregroundObserver: NSObjectProtocol?
  private var states: [String: WatchState] = [:]

  private override init() {
    super.init()
  }

  /// Idempotent. Starts the parked-page sweep (and the foreground-
  /// return catch-up sweep). Called from plugin init.
  func start() {
    if Thread.isMainThread {
      startOnMain()
    } else {
      DispatchQueue.main.async { self.startOnMain() }
    }
  }

  private func startOnMain() {
    guard !started else { return }
    started = true
    let timer = Timer(
      timeInterval: Self.sweepInterval, repeats: true
    ) { [weak self] _ in self?.sweepParkedPages() }
    RunLoop.main.add(timer, forMode: .common)
    sweepTimer = timer
    // Catch-up sweep on foreground return: a task may have finished
    // while the app was suspended; the parked page's DOM shows the
    // finished state, so the transition is detected on return.
    foregroundObserver = NotificationCenter.default.addObserver(
      forName: UIApplication.willEnterForegroundNotification,
      object: nil,
      queue: .main
    ) { [weak self] _ in self?.sweepParkedPages() }
  }

  /// Drops all state for an account (profile deleted).
  func forget(uuid: String) {
    states = states.filter { !$0.key.hasSuffix("|\(uuid)") }
  }

  /// Feeds one task-state sample for an account. Returns true when the
  /// page's content changed (the probe uses it to stay at the 4.5s
  /// interval while a task is streaming). Main-thread only.
  @discardableResult
  func noteSample(
    providerId: String, uuid: String, result: Any?
  ) -> Bool {
    guard providerId == "muse" else { return false }
    guard let sample = parseSample(result) else { return false }
    let key = "\(providerId)|\(uuid)"
    var state = states[key] ?? WatchState()
    let signature = "\(sample.msgCount)|\(sample.tailHash)|\(sample.working)"

    // First sighting: record the baseline, never notify.
    guard state.baselineSet else {
      state.baselineSet = true
      state.lastSignature = signature
      state.lastTailLen = sample.tailLen
      state.lastMsgCount = sample.msgCount
      states[key] = state
      return false
    }

    guard signature != state.lastSignature else {
      state.stableCount += 1
      states[key] = state
      if !sample.working && state.stableCount >= Self.stableChecksToFire
        && (state.sawStopButton || state.growthEvents >= 2)
      {
        // Working → idle, with real growth behind it: task done.
        state.sawStopButton = false
        state.growthEvents = 0
        state.stableCount = 0
        states[key] = state
        fireCompletion(providerId: providerId, uuid: uuid)
      }
      return false
    }

    // Content changed. A big shrink means the user navigated to another
    // conversation (or the page reloaded mid-stream): re-baseline
    // silently instead of treating it as activity.
    if sample.tailLen < state.lastTailLen - 500
      || sample.msgCount < state.lastMsgCount
    {
      state.lastSignature = signature
      state.lastTailLen = sample.tailLen
      state.lastMsgCount = sample.msgCount
      state.growthEvents = 0
      state.sawStopButton = false
      state.stableCount = 0
      states[key] = state
      return true
    }

    if sample.tailLen > state.lastTailLen {
      state.growthEvents += 1
    }
    if sample.working {
      state.sawStopButton = true
    }
    state.stableCount = 0
    state.lastSignature = signature
    state.lastTailLen = sample.tailLen
    state.lastMsgCount = sample.msgCount
    states[key] = state
    return true
  }

  // MARK: - Parked sweep

  private func sweepParkedPages() {
    for entry in RecentAccountPages.parkedKeys() {
      guard entry.providerId == "muse" else { continue }
      RecentAccountPages.probeParked(
        providerId: entry.providerId, uuid: entry.uuid,
        javascript: Self.taskStateScript
      ) { [weak self] result in
        // probeParked returns nil when the page was evicted between
        // listing and probing; parseSample(nil) is nil, so this is
        // a harmless no-op.
        self?.noteSample(
          providerId: entry.providerId, uuid: entry.uuid, result: result)
      }
    }
  }

  // MARK: - Completion

  private func fireCompletion(providerId: String, uuid: String) {
    // 0.1.16: no system notification — the emit marks the account
    // unread (avatar dot + aggregate "← 账号" dot). The coordinator
    // skips the active account (Muse web handles its completions).
    AccountSessionCoordinator.shared.emitTaskReplyDetected(
      providerId: providerId, uuid: uuid)
  }

  private func parseSample(_ result: Any?) -> TaskSample? {
    guard let jsonString = result as? String,
      let data = jsonString.data(using: .utf8),
      let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
      obj["usable"] as? Bool == true
    else { return nil }
    return TaskSample(
      msgCount: obj["msgCount"] as? Int ?? 0,
      tailHash: obj["tailHash"] as? Int ?? 0,
      tailLen: obj["tailLen"] as? Int ?? 0,
      working: obj["working"] as? Bool ?? false
    )
  }
}
