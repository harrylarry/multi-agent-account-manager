// TaskWatcher.swift — 0.1.15: Muse task-completion notifications.
//
// Watches every live Muse session — the active one via the identity
// probe cycle, parked ones via a 20s sweep — for a new assistant reply.
// The user's rule: a task is done when a new reply appears. On a
// working→idle transition the watcher posts a local notification
// (tap = switch to that account) and marks the account unread in the
// manager list. Each account is watched independently, so several
// accounts can notify at once, including non-active spaces.
//
// Detection is heuristic: Muse's DOM has no stable message API, so the
// script reports a message-count estimate (best of several common
// selectors), a hash of the conversation tail, the text length, and
// whether a Stop button is visible. The state machine arms only after
// sustained activity — a Stop button seen, or content growing on 2+
// separate checks — so a lone user message never notifies; it fires
// after the content is stable for 2 consecutive checks.
//
// Limits (platform design, not choice):
// - Local notifications only. The app is signed with a free personal
//   certificate, which Apple does not grant the push entitlement, so
//   there is no APNs and no server — detection runs on-device.
// - Detection only runs while the app is in the foreground. iOS
//   suspends the app in the background, so parked pages stop updating
//   and the sweep timer stops firing. On foreground return the watcher
//   sweeps immediately and can report completions that happened while
//   away (the parked page's DOM shows the finished state).
//
// Main-thread only, like the probe loop and RecentAccountPages.

import Foundation
import UIKit
import UserNotifications

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
  private static let authRequestedKey = "maam.notificationAuthRequested"

  private var started = false
  private var sweepTimer: Timer?
  private var foregroundObserver: NSObjectProtocol?
  private var states: [String: WatchState] = [:]

  private override init() {
    super.init()
  }

  /// Idempotent. Sets the notification delegate (so banners show while
  /// the app is in the foreground and taps route to the account) and
  /// starts the parked-page sweep. Called from plugin init — the
  /// earliest point in this codebase — so a notification tap that
  /// cold-launches the app still reaches the delegate.
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
    UNUserNotificationCenter.current().delegate = self
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

  /// Requests notification authorization once, the first time a Muse
  /// session opens (called from openAccountSession). The system prompt
  /// appears exactly once; a denial is final until the user changes it
  /// in Settings, and the watcher keeps working (the manager-list
  /// unread dot still marks the account).
  func ensureAuthorization() {
    guard !UserDefaults.standard.bool(forKey: Self.authRequestedKey) else { return }
    UserDefaults.standard.set(true, forKey: Self.authRequestedKey)
    UNUserNotificationCenter.current().requestAuthorization(
      options: [.alert, .sound, .badge]
    ) { _, _ in }
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
    providerId: String, uuid: String, alias: String, result: Any?
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
        fireCompletion(providerId: providerId, uuid: uuid, alias: alias)
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
          providerId: entry.providerId, uuid: entry.uuid,
          alias: entry.alias, result: result)
      }
    }
  }

  // MARK: - Completion

  private func fireCompletion(providerId: String, uuid: String, alias: String) {
    postNotification(providerId: providerId, uuid: uuid, alias: alias)
    AccountSessionCoordinator.shared.emitTaskReplyDetected(
      providerId: providerId, uuid: uuid)
  }

  private func postNotification(providerId: String, uuid: String, alias: String) {
    let content = UNMutableNotificationContent()
    content.title = alias.isEmpty ? "Muse" : alias
    content.body = "任务完成了，有新回复"
    content.sound = .default
    content.userInfo = ["providerId": providerId, "uuid": uuid]
    // One notification per account: a new one replaces the old, so a
    // busy account never stacks banners.
    let request = UNNotificationRequest(
      identifier: "maam.task.\(uuid)", content: content, trigger: nil)
    UNUserNotificationCenter.current().add(request) { _ in }
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

// MARK: - UNUserNotificationCenterDelegate

extension TaskWatcher: UNUserNotificationCenterDelegate {
  /// Show the banner even while the app is in the foreground — the user
  /// asked for foreground-space completions to notify too.
  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    willPresent notification: UNNotification
  ) async -> UNNotificationPresentationOptions {
    [.banner, .sound]
  }

  /// Tap on a task notification: switch to that account. The tap is
  /// stored first so a cold launch (channel not yet subscribed) still
  /// delivers it when the channel is set; emitNotificationTapped clears
  /// the stored tap once the event is sent.
  func userNotificationCenter(
    _ center: UNUserNotificationCenter,
    didReceive response: UNNotificationResponse
  ) async {
    guard response.actionIdentifier == UNNotificationDefaultActionIdentifier
    else { return }
    let info = response.notification.request.content.userInfo
    guard let providerId = info["providerId"] as? String,
      let uuid = info["uuid"] as? String
    else { return }
    let coordinator = AccountSessionCoordinator.shared
    coordinator.storePendingNotificationTap(providerId: providerId, uuid: uuid)
    coordinator.emitNotificationTapped(providerId: providerId, uuid: uuid)
  }
}
