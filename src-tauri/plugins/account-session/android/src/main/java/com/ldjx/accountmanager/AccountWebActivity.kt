package com.ldjx.accountmanager

import android.annotation.SuppressLint
import android.content.Intent
import android.content.MutableContextWrapper
import android.graphics.Bitmap
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.Message
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.webkit.RenderProcessGoneDetail
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.lifecycle.Lifecycle
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import org.json.JSONObject

class AccountWebActivity : AppCompatActivity() {

    private lateinit var headerTitleView: TextView
    private lateinit var mainWebContainer: FrameLayout
    private lateinit var loadingView: ProgressBar
    private lateinit var popupOverlayRoot: LinearLayout
    private lateinit var popupDomainLabel: TextView
    private lateinit var popupWebContainer: FrameLayout

    private var currentSpec: AndroidAccountSessionSpec? = null
    private var mainWebView: WebView? = null
    private var committedAuthority: String? = null
    private var popupWebView: WebView? = null
    private var mainInputEnabled: Boolean = false
    private var popupInputEnabled: Boolean = false
    private var closingFromManager: Boolean = false
    private var capturedIdentityHint: String? = null
    private var pendingFilePathCallback: ValueCallback<Array<Uri>>? = null
    private val uiHandler = Handler(Looper.getMainLooper())
    private var identityProbeRunnable: Runnable? = null

    private val fileChooserLauncher = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult()
    ) { result ->
        val callback = pendingFilePathCallback ?: return@registerForActivityResult
        pendingFilePathCallback = null
        if (result.resultCode != RESULT_OK) {
            callback.onReceiveValue(null)
            return@registerForActivityResult
        }
        val data = result.data
        val clip = data?.clipData
        val uris = if (clip != null && clip.itemCount > 0) {
            Array(clip.itemCount) { i -> clip.getItemAt(i).uri }
        } else {
            WebChromeClient.FileChooserParams.parseResult(result.resultCode, data)
        }
        callback.onReceiveValue(uris)
    }

    companion object {
        private const val QUOTA_HOOK_INIT_SCRIPT = """
            (() => {
              try {
                if (location.protocol !== 'https:') return;
                if (window !== window.top) return;
                if (window.__maamNetHooked) return;
                window.__maamNetHooked = true;
                const initHost = (location.hostname || '').toLowerCase();
                const isPrimaryHost = initHost === 'muse.ai' || initHost.endsWith('.muse.ai') || initHost === 'app.hark.com' || initHost.endsWith('.hark.com');
                const setQuota = (text, pct) => {
                  if (!text) return;
                  const cleanPct = typeof pct === 'number' && Number.isFinite(pct)
                    ? Math.max(0, Math.min(100, Math.round(pct)))
                    : null;
                  const obj = { text: String(text).slice(0, 48), pct: cleanPct };
                  window.__maamQuota = obj;
                  try { localStorage.setItem('__maam_cached_quota__', JSON.stringify(obj)); } catch (_) {}
                };
                const saveDetail = (detail) => {
                  if (!detail || !detail.planLabel) return;
                  window.__maamQuotaDetail = detail;
                  try { localStorage.setItem('__maam_cached_quota_detail_v2__', JSON.stringify(detail)); } catch (_) {}
                  const rem = typeof detail.planUsedPercent === 'number'
                    ? Math.max(0, Math.min(100, 100 - detail.planUsedPercent))
                    : null;
                  setQuota(detail.planValue || detail.planLabel, rem);
                };
                const formatHarkReset = (iso) => {
                  if (!iso || typeof iso !== 'string') return null;
                  const d = new Date(iso);
                  if (Number.isNaN(d.getTime())) return null;
                  const pad = (n) => String(n).padStart(2, '0');
                  const hhmm = pad(d.getHours()) + ':' + pad(d.getMinutes());
                  const now = new Date();
                  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
                  const targetStart = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
                  const diffDays = Math.round((targetStart - todayStart) / 86400000);
                  if (diffDays === 0) return '今天 ' + hhmm + ' 重置';
                  if (diffDays === 1) return '明天 ' + hhmm + ' 重置';
                  if (d.getFullYear() === now.getFullYear()) {
                    return (d.getMonth() + 1) + '月' + d.getDate() + '日 ' + hhmm + ' 重置';
                  }
                  return d.getFullYear() + '年' + (d.getMonth() + 1) + '月' + d.getDate() + '日 ' + hhmm + ' 重置';
                };
                const mapHarkSub = (node) => {
                  if (!(location.hostname || '').toLowerCase().endsWith('hark.com')) return null;
                  if (!node || typeof node !== 'object') return null;
                  const meters = (node.meters && typeof node.meters === 'object' ? (node.meters.harkTokens || node.meters) : null)
                    || (node.harkTokens && typeof node.harkTokens === 'object' ? node.harkTokens : null)
                    || (('dailyLimit' in node || 'poolLimit' in node) ? node : null);
                  if (meters && typeof meters === 'object' && (typeof meters.dailyLimit === 'number' || typeof meters.poolLimit === 'number')) {
                    const rawPlan = (node.plan && typeof node.plan === 'object' && typeof node.plan.name === 'string' && node.plan.name.trim())
                      || (typeof node.planName === 'string' && node.planName.trim())
                      || (typeof node.plan === 'string' && node.plan.trim())
                      || '';
                    const shortPlan = rawPlan.replace(/^hark\s+/i, '').trim();
                    const hasDaily = typeof meters.dailyLimit === 'number' && meters.dailyLimit > 0 && typeof meters.dailyUsed === 'number';
                    const hasMonthly = typeof meters.poolLimit === 'number' && meters.poolLimit > 0 && typeof meters.poolUsed === 'number';
                    if (hasDaily || hasMonthly) {
                      const dailyPct = hasDaily ? Math.max(0, Math.min(100, Math.round((meters.dailyUsed / meters.dailyLimit) * 100))) : null;
                      const monthlyPct = hasMonthly ? Math.max(0, Math.min(100, Math.round((meters.poolUsed / meters.poolLimit) * 100))) : null;
                      if (hasDaily) {
                        return {
                          planLabel: shortPlan ? (shortPlan + ' · 每日') : '每日上限',
                          planValue: '已使用 ' + dailyPct + '%',
                          planSubtext: formatHarkReset(meters.dailyResetsAt),
                          planUsedPercent: dailyPct,
                          extraLabel: hasMonthly ? '每月上限' : null,
                          extraValue: hasMonthly ? ('已使用 ' + monthlyPct + '%') : null,
                          extraSubtext: hasMonthly ? formatHarkReset(meters.poolResetsAt) : null,
                          extraUsedPercent: monthlyPct
                        };
                      }
                      return {
                        planLabel: shortPlan ? (shortPlan + ' · 每月') : '每月上限',
                        planValue: '已使用 ' + monthlyPct + '%',
                        planSubtext: formatHarkReset(meters.poolResetsAt),
                        planUsedPercent: monthlyPct,
                        extraLabel: null,
                        extraValue: null,
                        extraSubtext: null,
                        extraUsedPercent: null
                      };
                    }
                  }
                  const u = node.usage || node.billing || node.credits || node.subscription || node;
                  if (!u || typeof u !== 'object' || 'page' in u || 'items' in u || 'messages' in u) return null;
                  const rem = u.remaining ?? u.remaining_credits ?? u.remainingCredits ?? u.credits_remaining ?? u.creditsRemaining ?? u.balance ?? u.creditBalance ?? u.left;
                  const tot = u.total ?? u.limit ?? u.total_credits ?? u.totalCredits ?? u.max ?? u.allowance;
                  const used = u.used ?? u.used_credits ?? u.usedCredits ?? u.consumed;
                  const planName = u.plan_name || u.planName || u.plan || node.plan_name || node.planName || node.plan || (typeof node.tier === 'string' ? node.tier : null) || (typeof u.name === 'string' ? u.name : null) || 'Hark 额度';
                  if (typeof rem === 'number' || typeof used === 'number') {
                    let usedPct = null;
                    let planVal = '';
                    if (typeof tot === 'number' && tot > 0) {
                      const actualUsed = typeof used === 'number' ? used : (tot - (typeof rem === 'number' ? rem : 0));
                      usedPct = Math.max(0, Math.min(100, Math.round((actualUsed / tot) * 100)));
                      planVal = '已使用 ' + usedPct + '%';
                    } else if (typeof rem === 'number') {
                      planVal = '剩余 ' + rem;
                    }
                    const resetsAt = u.resets_at ?? u.resetsAt;
                    return {
                      planLabel: String(planName),
                      planValue: planVal,
                      planSubtext: resetsAt ? ('将在 ' + new Date(resetsAt).toLocaleDateString() + ' 重置') : (u.period ? ('周期: ' + u.period) : null),
                      planUsedPercent: usedPct,
                      extraLabel: null,
                      extraValue: null,
                      extraSubtext: null,
                      extraUsedPercent: null
                    };
                  }
                  return null;
                };
                const mapSub = (node) => {
                  if (!node || typeof node !== 'object') return null;
                  const harkRes = mapHarkSub(node);
                  if (harkRes) return harkRes;
                  const s = (node.subscription && typeof node.subscription === 'object' && node.subscription.usage)
                    ? node.subscription
                    : node;
                  const u = s.usage;
                  if (!u || typeof u !== 'object') return null;
                  if (!('state' in u) && !('percentUsed' in u) && !('percent_used' in u) && !('quotaStatus' in u) && !('quota_status' in u)) {
                    return null;
                  }
                  const subType = s.subscriptionType || s.subscription_type || '';
                  const tierName = (s.tier && typeof s.tier.name === 'string' && s.tier.name.trim()) || '';
                  const planLabel = (typeof s.usageRowLabel === 'string' && s.usageRowLabel.trim())
                    || (typeof s.usage_row_label === 'string' && s.usage_row_label.trim())
                    || tierName
                    || (subType === 'PAID' ? '订阅版' : subType === 'COMPLIMENTARY' ? '赠送版' : subType === 'FREE' ? '免费版' : '基础额度');
                  const rawPct = typeof u.percentUsed === 'number' ? u.percentUsed : u.percent_used;
                  const qStatus = u.quotaStatus || u.quota_status || '';
                  let planUsedPercent = null;
                  if (u.state !== 'UNLIMITED' && typeof rawPct === 'number' && Number.isFinite(rawPct)) {
                    planUsedPercent = Math.max(0, Math.min(100, Math.round(rawPct)));
                  } else if (qStatus === 'DEPLETED') {
                    planUsedPercent = 100;
                  }
                  const rawVal = (typeof s.usageRowValueLabel === 'string' && s.usageRowValueLabel.trim())
                    || (typeof s.usage_row_value_label === 'string' && s.usage_row_value_label.trim())
                    || '';
                  const planValue = rawVal
                    || (u.state === 'UNLIMITED' ? '无限额度'
                      : planUsedPercent !== null ? ('已使用 ' + planUsedPercent + '%')
                      : qStatus === 'DEPLETED' ? '已用尽'
                      : qStatus === 'LOW' ? '额度偏低'
                      : '额度充足');
                  let planSubtext = (typeof s.statusSubtitle === 'string' && s.statusSubtitle.trim())
                    || (typeof s.status_subtitle === 'string' && s.status_subtitle.trim())
                    || (typeof s.usageRowFooter === 'string' && s.usageRowFooter.trim())
                    || (typeof s.usage_row_footer === 'string' && s.usage_row_footer.trim())
                    || null;
                  if (!planSubtext) {
                    const rawReset = u.resetsAt ?? u.resets_at;
                    if (typeof rawReset === 'number' && rawReset > 1000000000) {
                      const ms = rawReset > 100000000000 ? rawReset : rawReset * 1000;
                      const d = new Date(ms);
                      if (!Number.isNaN(d.getTime())) {
                        planSubtext = '将在 ' + (d.getMonth() + 1) + '月' + d.getDate() + '日重置';
                      }
                    }
                  }
                  const topupVal = (typeof s.topupRowValueLabel === 'string' && s.topupRowValueLabel.trim())
                    || (typeof s.topup_row_value_label === 'string' && s.topup_row_value_label.trim())
                    || '';
                  const topupBal = typeof s.topupBalance === 'number' ? s.topupBalance : s.topup_balance;
                  const topupTot = typeof s.topupTotal === 'number' ? s.topupTotal : s.topup_total;
                  let extraLabel = null;
                  let extraValue = null;
                  let extraSubtext = null;
                  let extraUsedPercent = null;
                  if (topupVal || (typeof topupTot === 'number' && topupTot > 0)) {
                    extraLabel = (typeof s.topupRowLabel === 'string' && s.topupRowLabel.trim())
                      || (typeof s.topup_row_label === 'string' && s.topup_row_label.trim())
                      || '额外的使用额度';
                    if (typeof topupTot === 'number' && topupTot > 0 && typeof topupBal === 'number') {
                      extraUsedPercent = Math.max(0, Math.min(100, Math.round(((topupTot - topupBal) / topupTot) * 100)));
                    }
                    extraValue = topupVal || (extraUsedPercent !== null ? ('已使用 ' + extraUsedPercent + '%') : null);
                    extraSubtext = '从不过期';
                  }
                  return {
                    planLabel,
                    planValue,
                    planSubtext,
                    planUsedPercent,
                    extraLabel,
                    extraValue,
                    extraSubtext,
                    extraUsedPercent
                  };
                };
                window.__maamMapSub = mapSub;
                window.__maamSaveDetail = saveDetail;
                window.__maamRefreshHarkQuota = async (force) => {
                  if (window.__maamQuotaInflight) return;
                  const now = Date.now();
                  if (!force && window.__maamQuotaFetchedAt && now - window.__maamQuotaFetchedAt < 90000) return;
                  const curHost = (location.hostname || '').toLowerCase();
                  if (curHost !== 'app.hark.com' && !curHost.endsWith('.hark.com')) return;
                  window.__maamQuotaInflight = true;
                  try {
                    const fetchFn = window.__maamOrigFetch || window.fetch;
                    const res = await fetchFn.call(window, '/api/billing/summary', {
                      credentials: 'include',
                      headers: { Accept: 'application/json' }
                    });
                    if (res && res.ok) {
                      const data = await res.json();
                      const mapped = mapHarkSub(data);
                      if (mapped) {
                        window.__maamQuotaFetchedAt = Date.now();
                        saveDetail(mapped);
                      }
                    }
                    if (!window.__maamApiEmail) {
                      try {
                        const profRes = await fetchFn.call(window, '/api/profile', {
                          credentials: 'include',
                          headers: { Accept: 'application/json' }
                        });
                        if (profRes && profRes.ok) {
                          const prof = await profRes.json();
                          if (prof && typeof prof.email === 'string' && prof.email.includes('@')) {
                            window.__maamApiEmail = prof.email.trim().toLowerCase();
                          }
                        }
                      } catch (_) {}
                    }
                  } catch (_) {
                  } finally {
                    window.__maamQuotaInflight = false;
                  }
                };
                const inspectNode = (node, depth, skipSub) => {
                  if (!node || typeof node !== 'object' || depth > 6) return;
                  if (Array.isArray(node)) {
                    for (let i = 0; i < node.length && i < 15; i++) inspectNode(node[i], depth + 1, skipSub);
                    return;
                  }
                  if (typeof node.email === 'string' && /^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/.test(node.email.trim())) {
                    window.__maamApiEmail = node.email.trim().toLowerCase();
                  }
                  let nextSkipSub = Boolean(skipSub);
                  if (!nextSkipSub) {
                    const mapped = mapSub(node);
                    if (mapped) {
                      saveDetail(mapped);
                      nextSkipSub = true;
                    }
                  }
                  const remCredits = node.remaining_credits ?? node.credits_remaining ?? node.credit_balance ?? node.quota_remaining;
                  if (typeof remCredits === 'number' && Number.isFinite(remCredits)) {
                    const total = node.total_credits ?? node.monthly_limit ?? node.quota_limit;
                    if (typeof total === 'number' && total > 0) {
                      const pct = Math.max(0, Math.min(100, Math.round((remCredits / total) * 100)));
                      setQuota('剩余 ' + remCredits + '/' + total, pct);
                    } else {
                      setQuota('剩余额度 ' + remCredits, null);
                    }
                  }
                  if (node.code === 'out_of_credits' || node.error === 'rate_limit_exceeded') {
                    setQuota('额度已用尽', 0);
                  }
                  const keys = Object.keys(node);
                  for (let i = 0; i < keys.length && i < 24; i++) {
                    const v = node[keys[i]];
                    if (v && typeof v === 'object') inspectNode(v, depth + 1, nextSkipSub);
                  }
                };
                const inspectText = (txt) => {
                  if (typeof txt !== 'string' || txt.length < 2 || txt.length > 600000) return;
                  if (!/percent_used|percentUsed|quota_status|quotaStatus|usage_row_value_label|usageRowValueLabel|topup_balance|topupBalance|remaining_credits|credit_balance|out_of_credits|UNLIMITED|email|credits|allowance|balance|usage|tokens|billing|subscription|harkTokens|dailyLimit|dailyUsed|poolLimit|poolUsed|dailyResetsAt|poolResetsAt|meters/i.test(txt)) return;
                  try {
                    inspectNode(JSON.parse(txt), 0, false);
                    return;
                  } catch (_) {}
                  const lines = txt.split('\n');
                  for (let i = 0; i < lines.length && i < 80; i++) {
                    const idx = lines[i].indexOf(':');
                    if (idx > 0 && idx < 12) {
                      const rest = lines[i].slice(idx + 1).trim();
                      if (rest.startsWith('{') || rest.startsWith('[')) {
                        try { inspectNode(JSON.parse(rest), 0, false); } catch (_) {}
                      }
                    }
                  }
                };
                if (isPrimaryHost) {
                  const origFetch = window.fetch;
                  if (typeof origFetch === 'function') {
                    window.__maamOrigFetch = origFetch;
                    window.fetch = function(...args) {
                      const reqUrl = typeof args[0] === 'string' ? args[0] : (args[0] && args[0].url) || '';
                      const p = origFetch.apply(this, args);
                      p.then((res) => {
                        try {
                          const ct = (res.headers && res.headers.get('content-type')) || '';
                          if (ct.includes('json') || ct.includes('text')) {
                            res.clone().text().then(inspectText).catch(() => {});
                          }
                          if (reqUrl.includes('/api/messages') && initHost.endsWith('hark.com')) {
                            clearTimeout(window.__maamPostHarkChatTimer);
                            window.__maamPostHarkChatTimer = setTimeout(() => {
                              if (typeof window.__maamRefreshHarkQuota === 'function') {
                                window.__maamRefreshHarkQuota(true);
                              }
                            }, 6000);
                          }
                        } catch (_) {}
                      }).catch(() => {});
                      return p;
                    };
                  }
                  const OrigWS = window.WebSocket;
                  if (typeof OrigWS === 'function') {
                    const WrappedWS = function(...args) {
                      const ws = new OrigWS(...args);
                      ws.addEventListener('message', (ev) => {
                        if (typeof ev.data === 'string') inspectText(ev.data);
                      });
                      return ws;
                    };
                    WrappedWS.prototype = OrigWS.prototype;
                    Object.assign(WrappedWS, OrigWS);
                    window.WebSocket = WrappedWS;
                  }
                }
              } catch (_) {}
            })();
        """

        private const val IDENTITY_PROBE_SCRIPT = QUOTA_HOOK_INIT_SCRIPT + """
            (() => {
              try {
                if (!window.__maamHooked) {
                  window.__maamHooked = true;
                  const saveVal = (el) => {
                    if (!el || typeof el.value !== 'string') return;
                    const v = el.value.trim();
                    if (/^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/.test(v)) {
                      window.__maamTyped = v.toLowerCase();
                      try { sessionStorage.setItem('__maam_typed_identity__', window.__maamTyped); } catch (_) {}
                    }
                  };
                  document.addEventListener('input', (e) => saveVal(e.target), true);
                  document.addEventListener('change', (e) => saveVal(e.target), true);
                }
                document.querySelectorAll('input').forEach((el) => {
                  const v = (el.value || '').trim();
                  if (/^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$/.test(v)) {
                    window.__maamTyped = v.toLowerCase();
                    try { sessionStorage.setItem('__maam_typed_identity__', window.__maamTyped); } catch (_) {}
                  }
                });
                let typed = window.__maamTyped || '';
                try { typed = typed || sessionStorage.getItem('__maam_typed_identity__') || ''; } catch (_) {}
                const host = (location.hostname || '').toLowerCase();
                const path = (location.pathname || '').toLowerCase();
                const isPrimary = host === 'muse.ai' || host.endsWith('.muse.ai') || host === 'app.hark.com' || host.endsWith('.hark.com');
                const isExternalLoginHost = host === 'meta.com' || host.endsWith('.meta.com') || host === 'facebook.com' || host.endsWith('.facebook.com') || host === 'appleid.apple.com';
                const makeLoggedOut = () => {
                  window.__maamQuota = null;
                  window.__maamQuotaDetail = null;
                  try {
                    localStorage.removeItem('__maam_cached_quota__');
                    localStorage.removeItem('__maam_cached_quota_detail_v2__');
                  } catch (_) {}
                  return JSON.stringify({ auth: false, loggedOut: true, typed, email: '' });
                };
                const makeLoading = () => JSON.stringify({ auth: false, loggedOut: false, typed, email: '' });
                if (document.readyState !== 'complete') {
                  return makeLoading();
                }
                const loginInputs = document.querySelectorAll('input[type="email"], input[type="password"], input[autocomplete="one-time-code"], input[placeholder*="邮箱"], input[placeholder*="手机"], input[placeholder*="email" i], input[placeholder*="phone" i]');
                const hasVisibleLoginInput = () => {
                  for (const inp of loginInputs) {
                    if (inp.disabled || inp.readOnly) continue;
                    const r = inp.getBoundingClientRect();
                    if (r.width > 20 && r.height > 15) return true;
                  }
                  return false;
                };
                const hasChatComposer = Array.from(document.querySelectorAll('textarea, [contenteditable="true"]')).some((el) => {
                  if (el.disabled || el.readOnly) return false;
                  const r = el.getBoundingClientRect();
                  return r.width > 120 && r.height > 20;
                });
                if (isExternalLoginHost) {
                  if (/^\/(login|signin|signup|auth)\b/.test(path) || hasVisibleLoginInput()) {
                    return makeLoggedOut();
                  }
                  return makeLoading();
                }
                if (!isPrimary) {
                  return makeLoading();
                }
                if (/^\/(login|signin|signup|auth)\b/.test(path)) {
                  return makeLoggedOut();
                }
                if (!hasChatComposer && !/^\/(settings|profile|account)\b/.test(path) && hasVisibleLoginInput()) {
                  return makeLoggedOut();
                }
                const bodyText = (document.body && document.body.innerText) || '';
                if (!hasChatComposer && /登录或创建账户|Log in or sign up|Sign in to|登录以继续|通过 Google 继续|Continue with Google|通过 Apple 继续|Continue with Apple/i.test(bodyText)) {
                  return makeLoggedOut();
                }
                const bad = /^(support|privacy|help|legal|press|noreply|no-reply|info|security|contact|abuse|feedback|sales|terms)@/i;
                const re = /[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}/g;
                const candidates = [];
                if (window.__maamApiEmail) candidates.push(window.__maamApiEmail);
                document.querySelectorAll('[data-email],[aria-label*="@"],[title*="@"],a[href^="mailto:"]').forEach((el) => {
                  const raw = (el.getAttribute('data-email') || el.getAttribute('aria-label') || el.getAttribute('title') || el.getAttribute('href') || '').replace(/^mailto:/i, '');
                  const m = raw.match(re);
                  if (m) candidates.push(...m);
                });
                try {
                  for (let i = 0; i < localStorage.length; i++) {
                    const k = localStorage.key(i);
                    const val = k ? localStorage.getItem(k) : '';
                    if (val && val.includes('@') && val.length < 4000) {
                      const m = val.match(re);
                      if (m) candidates.push(...m);
                    }
                  }
                } catch (_) {}
                const textMatches = bodyText.match(re);
                if (textMatches) candidates.push(...textMatches);
                const extractAvatar = () => {
                  const list = document.querySelectorAll('[data-hatch-media-owner="avatar-layer"], [data-hatch-avatar-host] img, [data-hatch-avatar-host] video, [class*="hatch-avatar"] img, [class*="avatar" i] img');
                  for (const el of list) {
                    if (el.closest && el.closest('.landing-rebus-avatar-slot')) continue;
                    const r = el.getBoundingClientRect();
                    if (r.width < 16 || r.height < 16 || r.width > 520) continue;
                    try {
                      const sw = el.tagName === 'VIDEO' ? el.videoWidth : el.naturalWidth;
                      const sh = el.tagName === 'VIDEO' ? el.videoHeight : el.naturalHeight;
                      if (!sw || !sh) continue;
                      const c = document.createElement('canvas');
                      c.width = 72; c.height = 72;
                      const ctx = c.getContext('2d');
                      if (!ctx) continue;
                      const side = Math.min(sw, sh);
                      ctx.drawImage(el, (sw - side) / 2, (sh - side) / 2, side, side, 0, 0, 72, 72);
                      const px = ctx.getImageData(0, 0, 72, 72).data;
                      let vis = 0;
                      for (let i = 3; i < px.length; i += 64) { if (px[i] > 12 && ++vis >= 4) break; }
                      if (vis < 4) continue;
                      const d = c.toDataURL('image/webp', 0.85);
                      if (d && d.startsWith('data:image/')) return d;
                    } catch (_) {}
                  }
                  return '';
                };
                if ((host === 'muse.ai' || host.endsWith('.muse.ai')) && !window.__maamQuotaInflight && (!window.__maamQuotaFetchedAt || Date.now() - window.__maamQuotaFetchedAt > 90000)) {
                  const tpGlobal = globalThis.TURBOPACK;
                  if (tpGlobal && !Array.isArray(tpGlobal) && typeof tpGlobal.push === 'function') {
                    window.__maamQuotaInflight = true;
                    (async () => {
                      try {
                        if (!window.__maamTp) {
                          tpGlobal.push(['__maam_init__', 999999991, (ctx) => { window.__maamTp = ctx; }]);
                          tpGlobal.push(['__maam_run__', { otherChunks: [], runtimeModuleIds: [999999991] }]);
                        }
                        const tp = window.__maamTp;
                        if (!tp) return;
                        if (!window.__maamSubActionId && !window.__maamScriptsScanned) {
                          window.__maamScriptsScanned = true;
                          const fetchFn = window.__maamOrigFetch || window.fetch;
                          for (const sc of document.querySelectorAll('script[src*="/_next/static/"]')) {
                            if (!sc.src) continue;
                            try {
                              const txt = await fetchFn.call(window, sc.src, { cache: 'force-cache' }).then((r) => r.text());
                              const mPre = txt.match(/\.A\((\d+)\)\.then\(\(\{preloadHatchSettingsData\b/);
                              if (mPre) window.__maamPreloadModId = parseInt(mPre[1], 10);
                              const mAct = txt.match(/createServerReference\)\("([0-9a-f]{32,64})"[^)]*"fetchSubscriptionAction"\)/);
                              if (mAct) window.__maamSubActionId = mAct[1];
                            } catch (_) {}
                          }
                        }
                        if (window.__maamPreloadModId && !window.__maamSubActionId && typeof tp.A === 'function') {
                          const origPush = tpGlobal.push.bind(tpGlobal);
                          tpGlobal.push = (...items) => {
                            for (const ch of items) {
                              if (!Array.isArray(ch)) continue;
                              for (let i = 0; i < ch.length; i++) {
                                if (typeof ch[i] === 'function') {
                                  const src = String(ch[i]);
                                  const m = src.match(/createServerReference\)\("([0-9a-f]{32,64})"[^)]*"fetchSubscriptionAction"\)/);
                                  if (m) window.__maamSubActionId = m[1];
                                }
                              }
                            }
                            return origPush(...items);
                          };
                          await tp.A(window.__maamPreloadModId).catch(() => {});
                        }
                        let callServerFn = null;
                        if (tp.c && typeof tp.c.values === 'function') {
                          for (const m of tp.c.values()) {
                            if (m && m.exports && typeof m.exports.callServer === 'function') {
                              callServerFn = m.exports.callServer;
                              break;
                            }
                          }
                        }
                        const aid = window.__maamSubActionId || '40e0249eeae158685b6a8d0c3b203f53dd4f2c7d63';
                        if (callServerFn && aid && typeof window.__maamMapSub === 'function') {
                          const res = await callServerFn(aid, [{ includeAgreement: true }]);
                          const mapped = window.__maamMapSub(res);
                          if (mapped) {
                            window.__maamQuotaFetchedAt = Date.now();
                            if (typeof window.__maamSaveDetail === 'function') {
                              window.__maamSaveDetail(mapped);
                            } else {
                              window.__maamQuotaDetail = mapped;
                              try { localStorage.setItem('__maam_cached_quota_detail_v2__', JSON.stringify(mapped)); } catch (_) {}
                            }
                          }
                        }
                      } catch (_) {
                      } finally {
                        window.__maamQuotaInflight = false;
                      }
                    })();
                  }
                }
                if ((host === 'app.hark.com' || host.endsWith('.hark.com')) && typeof window.__maamRefreshHarkQuota === 'function') {
                  window.__maamRefreshHarkQuota(false);
                }
                let qText = '正在同步额度…';
                let qPct = -1;
                let qDetailStr = '';
                let dObj = window.__maamQuotaDetail || null;
                if (!dObj && !window.__maamRequireFreshQuota) {
                  try {
                    const cached = localStorage.getItem('__maam_cached_quota_detail_v2__');
                    if (cached) dObj = JSON.parse(cached);
                  } catch (_) {}
                }
                if ((!dObj || !dObj.planLabel) && host.endsWith('hark.com')) {
                  const mDaily = bodyText.match(/(?:已使用每日上限的\s*(\d+(?:\.\d+)?)\s*%|(\d+(?:\.\d+)?)\s*%\s*of\s*daily\s*limit\s*used)/i);
                  const mMonthly = bodyText.match(/(?:已使用每月上限的\s*(\d+(?:\.\d+)?)\s*%|(\d+(?:\.\d+)?)\s*%\s*of\s*monthly\s*limit\s*used)/i);
                  if (mDaily || mMonthly) {
                    const dPct = mDaily ? Math.max(0, Math.min(100, Math.round(parseFloat(mDaily[1] || mDaily[2])))) : null;
                    const mPct = mMonthly ? Math.max(0, Math.min(100, Math.round(parseFloat(mMonthly[1] || mMonthly[2])))) : null;
                    const mDailyReset = bodyText.match(/(?:每日重置|Daily reset)[\s:：·]*([^\n\r]{2,28})/i);
                    const mMonthlyReset = bodyText.match(/(?:每月重置|Monthly reset)[\s:：·]*([^\n\r]{2,28})/i);
                    const fmtDomReset = (m) => {
                      if (!m || !m[1]) return null;
                      const s = m[1].trim();
                      return s ? (s.endsWith('重置') ? s : (s + ' 重置')) : null;
                    };
                    dObj = dPct !== null
                      ? {
                          planLabel: '每日上限',
                          planValue: '已使用 ' + dPct + '%',
                          planSubtext: fmtDomReset(mDailyReset),
                          planUsedPercent: dPct,
                          extraLabel: mPct !== null ? '每月上限' : null,
                          extraValue: mPct !== null ? ('已使用 ' + mPct + '%') : null,
                          extraSubtext: mPct !== null ? fmtDomReset(mMonthlyReset) : null,
                          extraUsedPercent: mPct
                        }
                      : {
                          planLabel: '每月上限',
                          planValue: '已使用 ' + mPct + '%',
                          planSubtext: fmtDomReset(mMonthlyReset),
                          planUsedPercent: mPct,
                          extraLabel: null,
                          extraValue: null,
                          extraSubtext: null,
                          extraUsedPercent: null
                        };
                    if (typeof window.__maamSaveDetail === 'function') window.__maamSaveDetail(dObj);
                  }
                }
                if ((!dObj || !dObj.planLabel) && host.endsWith('hark.com')) {
                  const creditEls = document.querySelectorAll('[class*="credit" i], [class*="usage" i], [data-testid*="credit"], [data-testid*="usage"]');
                  for (const el of creditEls) {
                    const txt = (el.innerText || '').trim();
                    const m = txt.match(/(\d+(?:\.\d+)?)\s*\/\s*(\d+(?:\.\d+)?)/);
                    if (m) {
                      const cur = parseFloat(m[1]);
                      const max = parseFloat(m[2]);
                      if (max > 0) {
                        const pct = Math.max(0, Math.min(100, Math.round((cur / max) * 100)));
                        dObj = {
                          planLabel: 'Hark 用量',
                          planValue: '已使用 ' + pct + '%',
                          planSubtext: '当前 ' + cur + ' / ' + max,
                          planUsedPercent: pct,
                          extraLabel: null,
                          extraValue: null,
                          extraSubtext: null,
                          extraUsedPercent: null
                        };
                        if (typeof window.__maamSaveDetail === 'function') window.__maamSaveDetail(dObj);
                        break;
                      }
                    }
                  }
                }
                if (dObj && dObj.planLabel) {
                  window.__maamQuotaDetail = dObj;
                  qText = dObj.planValue || dObj.planLabel;
                  if (typeof dObj.planUsedPercent === 'number') qPct = Math.max(0, 100 - dObj.planUsedPercent);
                  qDetailStr = JSON.stringify(dObj);
                } else {
                  let qObj = window.__maamQuota || null;
                  if (!qObj && !window.__maamRequireFreshQuota) {
                    try {
                      const cachedQ = localStorage.getItem('__maam_cached_quota__');
                      if (cachedQ) qObj = JSON.parse(cachedQ);
                    } catch (_) {}
                  }
                  if (qObj && qObj.text) {
                    window.__maamQuota = qObj;
                    qText = qObj.text;
                    if (typeof qObj.pct === 'number') qPct = qObj.pct;
                  }
                }
                const av = extractAvatar();
                for (const c of candidates) {
                  const clean = c.toLowerCase();
                  if (!bad.test(clean) && !clean.endsWith('@example.com') && !clean.endsWith('@sentry.io')) {
                    return JSON.stringify({ auth: true, loggedOut: false, typed, email: clean, avatar: av, quotaText: qText, quotaPct: qPct, quotaDetail: qDetailStr });
                  }
                }
                return JSON.stringify({ auth: true, loggedOut: false, typed, email: '', avatar: av, quotaText: qText, quotaPct: qPct, quotaDetail: qDetailStr });
              } catch (_) {
                return JSON.stringify({ auth: false, loggedOut: false, typed: '', email: '' });
              }
            })()
        """
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        AccountSessionCoordinator.attachActivity(this)
        buildNativeHierarchy()
        WindowCompat.getInsetsController(window, window.decorView).apply {
            isAppearanceLightStatusBars = true
            isAppearanceLightNavigationBars = true
        }

        onBackPressedDispatcher.addCallback(
            this,
            object : OnBackPressedCallback(true) {
                override fun handleOnBackPressed() {
                    if (popupWebView != null) {
                        destroyPopupWebView()
                        return
                    }
                    returnToAccountList()
                }
            },
        )

        val resolvedSpec = if (savedInstanceState != null) {
            AccountSessionCoordinator.recoverSpecOnActivityRecreate()
        } else {
            AccountSessionCoordinator.currentActiveSpec()
        }

        if (resolvedSpec == null) {
            closingFromManager = true
            finish()
            return
        }

        bindSessionSpec(resolvedSpec)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(Bundle())
    }

    override fun onResume() {
        super.onResume()
        mainWebView?.onResume()
        popupWebView?.onResume()
        if (mainInputEnabled) currentSpec?.let(::startIdentityProbeLoop)
    }

    override fun onPause() {
        stopIdentityProbeLoop()
        mainWebView?.onPause()
        popupWebView?.onPause()
        super.onPause()
    }

    override fun onDestroy() {
        val spec = currentSpec
        releaseSessionViews()
        AccountSessionCoordinator.detachActivity(this)
        if (!closingFromManager && isFinishing && spec != null) {
            AccountSessionCoordinator.emitActivityClosed(spec.switchSeq, spec.viewInstanceId)
        }
        super.onDestroy()
    }

    private fun dp(value: Int): Int {
        return TypedValue.applyDimension(
            TypedValue.COMPLEX_UNIT_DIP,
            value.toFloat(),
            resources.displayMetrics,
        ).toInt()
    }

    private fun buildNativeHierarchy() {
        val root = FrameLayout(this).apply {
            setBackgroundColor(Color.parseColor("#F7F7F7"))
            layoutParams = ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            )
        }

        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            layoutParams = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            )
        }

        val topBar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(Color.parseColor("#EDEDED"))
            setPadding(dp(10), dp(6), dp(12), dp(6))
            minimumHeight = dp(52)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            )
        }

        val backBtn = Button(this).apply {
            text = "← 账号"
            textSize = 13f
            isAllCaps = false
            setOnClickListener {
                if (popupWebView != null) {
                    destroyPopupWebView()
                } else {
                    returnToAccountList()
                }
            }
        }

        headerTitleView = TextView(this).apply {
            text = "正在核验会话…"
            setTextColor(Color.parseColor("#191919"))
            textSize = 15f
            maxLines = 1
            ellipsize = TextUtils.TruncateAt.END
            setPadding(dp(10), 0, dp(10), 0)
            layoutParams = LinearLayout.LayoutParams(
                0,
                ViewGroup.LayoutParams.WRAP_CONTENT,
                1f,
            )
        }

        topBar.addView(backBtn)
        topBar.addView(headerTitleView)

        val contentFrame = FrameLayout(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                0,
                1f,
            )
        }

        loadingView = ProgressBar(this).apply {
            layoutParams = FrameLayout.LayoutParams(
                dp(36),
                dp(36),
                Gravity.CENTER,
            )
        }

        mainWebContainer = FrameLayout(this).apply {
            layoutParams = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            )
        }

        contentFrame.addView(loadingView)
        contentFrame.addView(mainWebContainer)
        column.addView(topBar)
        column.addView(contentFrame)

        popupOverlayRoot = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            visibility = View.GONE
            setBackgroundColor(Color.WHITE)
            layoutParams = FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            )
        }

        val popupHeader = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(Color.parseColor("#F2F5FA"))
            setPadding(dp(12), dp(6), dp(10), dp(6))
            minimumHeight = dp(48)
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.WRAP_CONTENT,
            )
        }

        popupDomainLabel = TextView(this).apply {
            text = ""
            setTextColor(Color.parseColor("#111827"))
            textSize = 13f
            maxLines = 1
            ellipsize = TextUtils.TruncateAt.END
            layoutParams = LinearLayout.LayoutParams(
                0,
                ViewGroup.LayoutParams.WRAP_CONTENT,
                1f,
            )
        }

        val closePopupBtn = Button(this).apply {
            text = "关闭认证"
            textSize = 12f
            isAllCaps = false
            setOnClickListener {
                destroyPopupWebView()
            }
        }

        popupHeader.addView(popupDomainLabel)
        popupHeader.addView(closePopupBtn)

        popupWebContainer = FrameLayout(this).apply {
            layoutParams = LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                0,
                1f,
            )
        }

        popupOverlayRoot.addView(popupHeader)
        popupOverlayRoot.addView(popupWebContainer)

        root.addView(column)
        root.addView(popupOverlayRoot)
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            val sysBars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            val bottomInset = maxOf(sysBars.bottom, ime.bottom)
            topBar.minimumHeight = dp(52) + sysBars.top
            topBar.setPadding(
                dp(10) + sysBars.left,
                dp(6) + sysBars.top,
                dp(12) + sysBars.right,
                dp(6),
            )
            popupHeader.minimumHeight = dp(48) + sysBars.top
            popupHeader.setPadding(
                dp(12) + sysBars.left,
                dp(6) + sysBars.top,
                dp(10) + sysBars.right,
                dp(6),
            )
            contentFrame.setPadding(sysBars.left, 0, sysBars.right, bottomInset)
            popupWebContainer.setPadding(sysBars.left, 0, sysBars.right, bottomInset)
            WindowInsetsCompat.CONSUMED
        }
        setContentView(root)
        ViewCompat.requestApplyInsets(root)
    }

    @SuppressLint("SetJavaScriptEnabled", "ClickableViewAccessibility")
    fun bindSessionSpec(spec: AndroidAccountSessionSpec) {
        releaseSessionViews(keepPage = true)

        currentSpec = spec
        mainInputEnabled = false
        headerTitleView.text = "正在核验会话…"
        loadingView.visibility = View.VISIBLE

        if (!WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)) {
            AccountSessionCoordinator.emitSessionFailed(
                spec.switchSeq,
                spec.viewInstanceId,
                "设备系统 WebView 不支持 MULTI_PROFILE",
            )
            closingFromManager = true
            finish()
            return
        }

        val cached = RecentAccountPages.take(this, spec)
        val wv = cached ?: WebView(MutableContextWrapper(applicationContext))
        try {
            if (cached == null) WebViewCompat.setProfile(wv, spec.uuid)
            val actualProfile = WebViewCompat.getProfile(wv).name
            if (actualProfile != spec.uuid) {
                wv.destroy()
                AccountSessionCoordinator.emitSessionFailed(
                    spec.switchSeq,
                    spec.viewInstanceId,
                    "Android WebView Profile 核对不匹配，已阻止加载",
                )
                closingFromManager = true
                finish()
                return
            }
        } catch (t: Throwable) {
            wv.destroy()
            AccountSessionCoordinator.emitSessionFailed(
                spec.switchSeq,
                spec.viewInstanceId,
                "绑定 Android WebView Profile 失败: ${t.message ?: "unknown"}",
            )
            closingFromManager = true
            finish()
            return
        }

        (wv.context as MutableContextWrapper).baseContext = this
        wv.visibility = View.INVISIBLE
        wv.isFocusable = false
        wv.isFocusableInTouchMode = false
        wv.setOnTouchListener { _, _ -> !mainInputEnabled }

        if (cached == null) {
            wv.settings.apply {
                javaScriptEnabled = true
                domStorageEnabled = true
                databaseEnabled = true
                setSupportMultipleWindows(true)
                javaScriptCanOpenWindowsAutomatically = false
                allowFileAccess = false
                allowContentAccess = false
            }
            if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
                runCatching {
                    WebViewCompat.addDocumentStartJavaScript(
                        wv,
                        QUOTA_HOOK_INIT_SCRIPT,
                        setOf(
                            "https://muse.ai",
                            "https://*.muse.ai",
                            "https://app.hark.com",
                            "https://*.hark.com",
                        ),
                    )
                }
            }
        }

        var mainLoadFailed = false
        wv.webViewClient = object : WebViewClient() {
            override fun onPageStarted(view: WebView, url: String?, favicon: Bitmap?) {
                if (view !== mainWebView || currentSpec !== spec) return
                mainLoadFailed = false
                committedAuthority = null
            }

            override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
                if (view === mainWebView && currentSpec === spec && request.isForMainFrame) {
                    mainLoadFailed = true
                    committedAuthority = null
                }
            }

            override fun onReceivedHttpError(view: WebView, request: WebResourceRequest, errorResponse: WebResourceResponse) {
                if (view === mainWebView && currentSpec === spec && request.isForMainFrame) {
                    mainLoadFailed = true
                    committedAuthority = null
                }
            }

            override fun shouldOverrideUrlLoading(
                view: WebView,
                request: WebResourceRequest,
            ): Boolean {
                if (!request.isForMainFrame) {
                    return false
                }
                if (!ownsMainView(view, spec)) return true
                val uri = request.url
                if (AccountSessionCoordinator.isGoogleOAuthHost(uri)) {
                    AccountSessionCoordinator.emitExternalAuthUnavailable(
                        spec.switchSeq,
                        spec.viewInstanceId,
                        "Google OAuth 按政策需使用系统浏览器，当前回跳暂无法绑定至发起账号的独立 Profile，请使用邮箱或手机号登录",
                    )
                    return true
                }
                return !AccountSessionCoordinator.hostMatchesAllowed(uri, spec.allowedHosts)
            }

            override fun onPageCommitVisible(view: WebView, url: String) {
                if (!ownsMainView(view, spec)) return
                val boundName = try {
                    WebViewCompat.getProfile(view).name
                } catch (ignored: Throwable) {
                    ""
                }
                val uri = Uri.parse(url)
                if (boundName != spec.uuid ||
                    !AccountSessionCoordinator.hostMatchesAllowed(uri, spec.allowedHosts)
                ) {
                    AccountSessionCoordinator.emitSessionFailed(
                        spec.switchSeq,
                        spec.viewInstanceId,
                        "顶层页面来源或资料归属核验未通过，已回退管理页",
                    )
                    closeAndFinishFromManager()
                    return
                }

                committedAuthority = if (mainLoadFailed) null else uri.encodedAuthority
                AccountSessionCoordinator.emitPageCommitVisible(spec, url)
            }

            override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                if (view === mainWebView && currentSpec === spec) {
                    AccountSessionCoordinator.emitSessionFailed(spec.switchSeq, spec.viewInstanceId, "账号页面已被系统回收，请重新打开")
                    closeAndFinishFromManager()
                }
                return true
            }
        }

        wv.webChromeClient = object : WebChromeClient() {
            override fun onCreateWindow(
                view: WebView,
                isDialog: Boolean,
                isUserGesture: Boolean,
                resultMsg: Message?,
            ): Boolean {
                if (!isUserGesture || resultMsg == null || popupWebView != null) {
                    return false
                }
                if (!ownsMainView(view, spec)) return false
                return createBoundAuthPopup(spec, resultMsg)
            }

            override fun onShowFileChooser(
                webView: WebView,
                filePathCallback: ValueCallback<Array<Uri>>,
                fileChooserParams: FileChooserParams?,
            ): Boolean {
                if (!ownsMainView(webView, spec)) {
                    filePathCallback.onReceiveValue(null)
                    return true
                }
                cancelPendingFileChooser()
                pendingFilePathCallback = filePathCallback
                val allowMultiple = fileChooserParams?.mode == FileChooserParams.MODE_OPEN_MULTIPLE
                val primaryIntent = runCatching {
                    fileChooserParams?.createIntent()?.apply {
                        if (allowMultiple) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                    }
                }.getOrNull()
                if (primaryIntent != null && runCatching { fileChooserLauncher.launch(primaryIntent) }.isSuccess) {
                    return true
                }
                val fallbackIntent = Intent(Intent.ACTION_GET_CONTENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = "*/*"
                    if (allowMultiple) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
                }
                if (runCatching { fileChooserLauncher.launch(fallbackIntent) }.isSuccess) {
                    return true
                }
                cancelPendingFileChooser()
                return false
            }
        }

        mainWebView = wv
        mainWebContainer.addView(
            wv,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            ),
        )
        val restoredUrl = cached?.url
        if (restoredUrl == null) {
            wv.loadUrl(spec.entryUrl)
        } else {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                wv.setRendererPriorityPolicy(WebView.RENDERER_PRIORITY_IMPORTANT, false)
            }
            wv.onResume()
            committedAuthority = Uri.parse(restoredUrl).encodedAuthority
            AccountSessionCoordinator.emitPageCommitVisible(spec, restoredUrl)
        }
    }

    private fun ownsMainView(view: WebView, spec: AndroidAccountSessionSpec) =
        view === mainWebView && currentSpec === spec &&
            AccountSessionCoordinator.isCurrent(spec.switchSeq, spec.viewInstanceId)

    @SuppressLint("SetJavaScriptEnabled", "ClickableViewAccessibility")
    private fun createBoundAuthPopup(
        spec: AndroidAccountSessionSpec,
        resultMsg: Message,
    ): Boolean {
        val popup = WebView(this)
        try {
            WebViewCompat.setProfile(popup, spec.uuid)
            if (WebViewCompat.getProfile(popup).name != spec.uuid) {
                popup.destroy()
                return false
            }
        } catch (ignored: Throwable) {
            popup.destroy()
            return false
        }

        popupInputEnabled = false
        popup.visibility = View.INVISIBLE
        popup.isFocusable = false
        popup.isFocusableInTouchMode = false
        popup.setOnTouchListener { _, _ -> !popupInputEnabled }

        popup.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            databaseEnabled = true
            setSupportMultipleWindows(false)
            javaScriptCanOpenWindowsAutomatically = false
            allowFileAccess = false
            allowContentAccess = false
        }

        popup.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(
                view: WebView,
                request: WebResourceRequest,
            ): Boolean {
                if (!request.isForMainFrame) {
                    return false
                }
                if (view !== popupWebView || !AccountSessionCoordinator.isCurrent(spec.switchSeq, spec.viewInstanceId)) {
                    return true
                }
                val uri = request.url
                if (uri.toString() == "about:blank") {
                    return false
                }
                if (AccountSessionCoordinator.isGoogleOAuthHost(uri)) {
                    destroyPopupWebView()
                    AccountSessionCoordinator.emitExternalAuthUnavailable(
                        spec.switchSeq,
                        spec.viewInstanceId,
                        "Google OAuth 按政策需使用系统浏览器，当前回跳暂无法绑定至发起账号的独立 Profile，请使用邮箱或手机号登录",
                    )
                    return true
                }
                if (!AccountSessionCoordinator.hostMatchesAllowed(uri, spec.allowedHosts)) {
                    destroyPopupWebView()
                    return true
                }
                return false
            }

            override fun onPageCommitVisible(view: WebView, url: String) {
                if (view !== popupWebView || !AccountSessionCoordinator.isCurrent(spec.switchSeq, spec.viewInstanceId)) {
                    return
                }
                if (url == "about:blank") {
                    return
                }
                val boundName = try {
                    WebViewCompat.getProfile(view).name
                } catch (ignored: Throwable) {
                    ""
                }
                val uri = Uri.parse(url)
                if (boundName != spec.uuid ||
                    !AccountSessionCoordinator.hostMatchesAllowed(uri, spec.allowedHosts)
                ) {
                    destroyPopupWebView()
                    return
                }
                val host = uri.host ?: ""
                popupDomainLabel.text = "认证域名 · $host"
                popupOverlayRoot.visibility = View.VISIBLE
                view.visibility = View.VISIBLE
                view.isFocusable = true
                view.isFocusableInTouchMode = true
                popupInputEnabled = true
            }

            override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                if (view === popupWebView) destroyPopupWebView()
                return true
            }
        }

        popup.webChromeClient = object : WebChromeClient() {
            override fun onCloseWindow(window: WebView?) {
                if (window === popupWebView) destroyPopupWebView()
            }
        }

        popupWebView = popup
        popupDomainLabel.text = ""
        popupOverlayRoot.visibility = View.GONE
        popupWebContainer.removeAllViews()
        popupWebContainer.addView(
            popup,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            ),
        )

        val transport = resultMsg.obj as? WebView.WebViewTransport ?: run {
            destroyPopupWebView()
            return false
        }
        transport.webView = popup
        resultMsg.sendToTarget()
        return true
    }

    fun revealCommittedSession(switchSeq: Long, viewInstanceId: Long, title: String) {
        val spec = currentSpec ?: return
        if (spec.switchSeq != switchSeq || spec.viewInstanceId != viewInstanceId) {
            return
        }
        val wv = mainWebView ?: return
        headerTitleView.text = title
        loadingView.visibility = View.GONE
        wv.visibility = View.VISIBLE
        wv.isFocusable = true
        wv.isFocusableInTouchMode = true
        mainInputEnabled = true
        if (lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) startIdentityProbeLoop(spec)
    }

    private fun stopIdentityProbeLoop() {
        identityProbeRunnable?.let { uiHandler.removeCallbacks(it) }
        identityProbeRunnable = null
    }

    private data class ProbeData(
        val auth: Boolean,
        val loggedOut: Boolean,
        val typed: String,
        val email: String,
        val avatar: String,
        val quotaText: String,
        val quotaPct: Int?,
        val quotaDetailJson: String,
    )

    private fun startIdentityProbeLoop(spec: AndroidAccountSessionSpec) {
        stopIdentityProbeLoop()
        var consecutiveLoggedOut = 0
        var loggedOutReported = false
        val task = object : Runnable {
            override fun run() {
                if (!AccountSessionCoordinator.isCurrent(spec.switchSeq, spec.viewInstanceId)) {
                    stopIdentityProbeLoop()
                    return
                }
                val popup = popupWebView
                popup?.evaluateJavascript(IDENTITY_PROBE_SCRIPT) { raw ->
                    if (identityProbeRunnable !== this || popup !== popupWebView ||
                        !AccountSessionCoordinator.isCurrent(spec.switchSeq, spec.viewInstanceId)) return@evaluateJavascript
                    parseProbeOutput(raw)?.let { parsed ->
                        if (parsed.typed.isNotEmpty()) {
                            capturedIdentityHint = parsed.typed
                        }
                    }
                }
                mainWebView?.evaluateJavascript(IDENTITY_PROBE_SCRIPT) { raw ->
                    if (identityProbeRunnable !== this ||
                        !AccountSessionCoordinator.isCurrent(spec.switchSeq, spec.viewInstanceId)) return@evaluateJavascript
                    val parsed = parseProbeOutput(raw)
                    if (parsed != null) {
                        if (parsed.typed.isNotEmpty()) {
                            capturedIdentityHint = parsed.typed
                        }
                        if (parsed.auth) {
                            consecutiveLoggedOut = 0
                            loggedOutReported = false
                            val resolved = parsed.email.ifEmpty { capturedIdentityHint.orEmpty() }
                            AccountSessionCoordinator.emitIdentityDetected(
                                spec,
                                resolved,
                                parsed.avatar.ifEmpty { null },
                                parsed.quotaText.ifEmpty { null },
                                parsed.quotaPct,
                                parsed.quotaDetailJson.ifEmpty { null },
                            )
                        } else if (parsed.loggedOut) {
                            consecutiveLoggedOut += 1
                            if (consecutiveLoggedOut == 1) {
                                uiHandler.removeCallbacks(this)
                                uiHandler.postDelayed(this, 1000L)
                            } else if (consecutiveLoggedOut >= 2 && !loggedOutReported) {
                                loggedOutReported = true
                                AccountSessionCoordinator.emitSessionLoggedOut(spec)
                            }
                        } else {
                            consecutiveLoggedOut = 0
                        }
                    }
                }
                uiHandler.postDelayed(this, 4500L)
            }
        }
        identityProbeRunnable = task
        uiHandler.postDelayed(task, 1400L)
    }

    private fun parseProbeOutput(raw: String?): ProbeData? {
        if (raw.isNullOrBlank() || raw == "null") return null
        return try {
            val decoded = org.json.JSONTokener(raw).nextValue() as? String ?: return null
            val obj = JSONObject(decoded)
            val pctRaw = obj.optInt("quotaPct", -1)
            ProbeData(
                auth = obj.optBoolean("auth", false),
                loggedOut = obj.optBoolean("loggedOut", false),
                typed = obj.optString("typed", ""),
                email = obj.optString("email", ""),
                avatar = obj.optString("avatar", ""),
                quotaText = obj.optString("quotaText", ""),
                quotaPct = if (pctRaw in 0..100) pctRaw else null,
                quotaDetailJson = obj.optString("quotaDetail", ""),
            )
        } catch (ignored: Throwable) {
            null
        }
    }

    fun closeAndFinishFromManager(keepPage: Boolean = false) {
        closingFromManager = true
        releaseSessionViews(keepPage)
        if (!isFinishing) {
            finish()
        }
    }

    private fun returnToAccountList() {
        val spec = currentSpec
        closingFromManager = true
        releaseSessionViews(keepPage = true)
        if (spec != null) {
            AccountSessionCoordinator.emitActivityClosed(spec.switchSeq, spec.viewInstanceId)
        }
        finish()
    }

    private fun cancelPendingFileChooser() {
        pendingFilePathCallback?.onReceiveValue(null)
        pendingFilePathCallback = null
    }

    private fun destroyPopupWebView() {
        val popup = popupWebView
        popupWebView = null
        popupInputEnabled = false
        popupOverlayRoot.visibility = View.GONE
        popupDomainLabel.text = ""
        popupWebContainer.removeAllViews()
        popup?.destroy()
    }

    private fun releaseSessionViews(keepPage: Boolean = false) {
        val spec = currentSpec
        val view = mainWebView
        val retain = keepPage && mainInputEnabled && popupWebView == null && committedAuthority != null &&
            view?.url?.let(Uri::parse)?.encodedAuthority == committedAuthority
        mainWebView = null
        currentSpec = null
        committedAuthority = null
        stopIdentityProbeLoop()
        cancelPendingFileChooser()
        capturedIdentityHint = null
        mainInputEnabled = false
        destroyPopupWebView()
        mainWebContainer.removeAllViews()
        if (view == null) return
        if (retain && spec != null) {
            view.visibility = View.INVISIBLE
            view.clearFocus()
            view.setOnTouchListener(null)
            view.webChromeClient = null
            RecentAccountPages.park(view, spec)
        } else {
            view.destroy()
        }
    }
}
