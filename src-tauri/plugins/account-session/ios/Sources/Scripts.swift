// Scripts.swift — JavaScript injected into account web views.
//
// The two scripts below are byte-identical (modulo leading/trailing blank
// lines, which are insignificant in JS) to the Kotlin constants
// QUOTA_HOOK_INIT_SCRIPT and the probe part of IDENTITY_PROBE_SCRIPT in
// AccountWebActivity.kt. They were extracted mechanically from the Kotlin
// source; do not hand-edit them here — change the Kotlin originals and
// re-extract, so Android and iOS never drift apart.

enum Scripts {
  /// fetch/XHR/WebSocket hooks that capture quota-related responses and
  /// stash them on window.__maamQuota. Injected once per web view at
  /// document start via WKUserScript.
  static let quotaHookInitScript = #"""
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
                  // --- Server Action 采集（广收集、窄使用）---
                  // 目的：把所有出现过的 Next-Action id 记下来，供诊断与后续复用；
                  // 只有当请求体与本项目已知的额度探测体完全一致时，才允许自动重放，
                  // 避免误触发有副作用的 action（删除/提交类）。
                  const HARVEST_KEY = '__maam_actions_v1';
                  const QUOTA_BODY = '[{"includeAgreement":false}]';
                  const harvest = (() => {
                    let list = [];
                    try { list = JSON.parse(localStorage.getItem(HARVEST_KEY) || '[]') || []; } catch (_) { list = []; }
                    const seen = new Set(list.map((x) => x.a + '|' + x.u + '|' + (x.b || '')));
                    const save = () => { try { localStorage.setItem(HARVEST_KEY, JSON.stringify(list)); } catch (_) {} };
                    return {
                      record(url, action, method, body) {
                        if (!action) return;
                        const b = (body || '').slice(0, 160);
                        const k = action + '|' + url + '|' + b;
                        if (seen.has(k)) return;
                        seen.add(k);
                        list.push({ a: action, u: String(url || '').slice(0, 200), m: method || 'GET', b, t: Date.now() });
                        if (list.length > 400) list = list.slice(-400);
                        save();
                      },
                      all() { return list; },
                      quotaBodies() {
                        return list.filter((x) => x.b === QUOTA_BODY).map((x) => x.a);
                      }
                    };
                  })();
                  window.__maamHarvest = harvest;
                  const pickHeader = (headers, name) => {
                    try {
                      if (!headers) return '';
                      if (typeof headers.get === 'function') return headers.get(name) || '';
                      if (Array.isArray(headers)) {
                        for (const kv of headers) {
                          if (kv && String(kv[0]).toLowerCase() === name) return String(kv[1]);
                        }
                        return '';
                      }
                      for (const k of Object.keys(headers)) {
                        if (k.toLowerCase() === name) return String(headers[k]);
                      }
                    } catch (_) {}
                    return '';
                  };
                  // 用采集到的 id 重放一次额度请求；只在响应确实含 subscription 时采纳。
                  window.__maamTryQuotaAction = async () => {
                    const ids = harvest.quotaBodies().slice(-4).reverse();
                    if (!ids.length) return false;
                    const fetchFn = window.__maamOrigFetch || window.fetch;
                    for (const id of ids) {
                      try {
                        const res = await fetchFn.call(window, '/', {
                          method: 'POST',
                          credentials: 'include',
                          headers: {
                            Accept: 'text/x-component',
                            'Content-Type': 'text/plain;charset=UTF-8',
                            'Next-Action': id
                          },
                          body: QUOTA_BODY
                        });
                        if (!res || !res.ok) continue;
                        const text = await res.text();
                        if (!/"subscription"\s*:/.test(text)) continue;
                        const lines = text.split(/\r?\n/);
                        for (const line of lines) {
                          const t = line.trim();
                          if (!t) continue;
                          const seg = /^\d+:/.test(t) ? t.slice(t.indexOf(':') + 1) : t;
                          try {
                            const obj = JSON.parse(seg);
                            const m = mapSub(obj);
                            if (m) { saveDetail(m); return true; }
                          } catch (_) {}
                        }
                      } catch (_) {}
                    }
                    return false;
                  };

                    window.fetch = function(...args) {
                      const reqUrl = typeof args[0] === 'string' ? args[0] : (args[0] && args[0].url) || '';
                      try {
                        const init = args[1] || {};
                        const act = pickHeader(init.headers, 'next-action');
                        if (act) {
                          harvest.record(reqUrl, act, init.method || 'GET', typeof init.body === 'string' ? init.body : '');
                        }
                      } catch (_) {}
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
                  // XHR 通道也采集（有些调用不走 fetch）。
                  try {
                    const XHR = window.XMLHttpRequest;
                    if (XHR && XHR.prototype && !XHR.prototype.__maamHooked) {
                      const oOpen = XHR.prototype.open;
                      const oSet = XHR.prototype.setRequestHeader;
                      const oSend = XHR.prototype.send;
                      XHR.prototype.open = function(method, url) {
                        this.__maamM = method; this.__maamU = url; return oOpen.apply(this, arguments);
                      };
                      XHR.prototype.setRequestHeader = function(k, v) {
                        try { if (String(k).toLowerCase() === 'next-action') this.__maamA = String(v); } catch (_) {}
                        return oSet.apply(this, arguments);
                      };
                      XHR.prototype.send = function(body) {
                        try {
                          if (this.__maamA) {
                            harvest.record(this.__maamU, this.__maamA, this.__maamM, typeof body === 'string' ? body : '');
                          }
                        } catch (_) {}
                        return oSend.apply(this, arguments);
                      };
                      XHR.prototype.__maamHooked = true;
                    }
                  } catch (_) {}
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
"""#

  /// Identity probe: runs the quota hook first, then inspects the DOM /
  /// cookies to detect the signed-in identity and logout state.
  /// Evaluated on demand (probe timer) via WKWebView.evaluateJavaScript.
  /// Returns a JSON string: { status, identity, avatarUrl, quotaText,
  /// quotaPercent, quotaDetail, providerId? } — same shape as Android.
  static let identityProbeScript = quotaHookInitScript + #"""
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
                if (!dObj) {
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
                  if (!qObj) {
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
"""#
}
