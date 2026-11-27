// eg Native Rust Dashboard Client — Core State, Shared Helpers & Transport
//
// State-first controller: one `state` object owns UI state and the payload
// data; render functions read state and write DOM. The behavioral contract is
// pinned in src/dashboard.rs (client_contract_tests, tip_tests) and
// tests/cli/dashboard/assets.rs — keep those strings and signatures intact.

const GRAPH_TOOLS = ['callers', 'callees', 'path', 'context', 'impact', 'symbols', 'imports', 'build'];
const FEED_LIMIT = 10; // both live feeds show the newest ten rows only
const OVERVIEW_FEED_LIMIT = 5; // mission-control activity stream
const OVERVIEW_TOP_N = 5; // top repositories / agents on the overview tab
const UNATTRIBUTED = '(unattributed)';
const TAB_KEYS = ['overview', 'telemetry', 'reuse', 'memory'];

const state = {
  activeTab: 'overview',
  theme: 'light',
  connected: false,
  selectedRepo: 'ALL',
  summary: null,
  reuse: { candidates: [], summary: {}, prewrite: [], pools: {}, filterDet: 'ALL', selectedId: null },
  memories: { notes: [], errors: [], degraded: false, filterStore: 'ALL', filterKind: 'ALL', selectedId: null },
  recentQueries: [],
  health: { graphErrors: 0, anchorModified: 0, anchorBroken: 0, reuseDegraded: false, reuseError: '' },
};

// Collapsed list views. "Recent" means latest activity: created, verified, or
// recalled. Reuse has no per-candidate timestamp in the payload, so its
// ranking is call-site usage only; nothing here claims recency it cannot know.
const MEMORY_VIEWS = {
  recent10: { label: 'Top 10 recent', limit: 10, sort: 'activity' },
  used: { label: 'Most used', limit: 10, sort: 'used' },
  all: { label: 'All', limit: 0, sort: 'activity' },
};
const REUSE_VIEWS = {
  used10: { label: 'Top 10 by callers', limit: 10 },
  all: { label: 'All', limit: 0 },
};
let memView = 'recent10';
let reuseView = 'used10';
let recentQueryTotal = 0;
let isDocumentVisible = true;

function setText(id, val) {
  const el = document.getElementById(id);
  if (el) el.textContent = val;
}

function escapeHtml(str) {
  return String(str || '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function updateListCount(id, shown, total, label) {
  setText(id, 'Showing ' + shown + ' of ' + total + ' · ' + label);
}

// One builder for every agent tag so the copy cannot drift between tables.
// `user` is the fallback when no agent marker was detected, so its tag has to
// say what it does not know instead of claiming a human typed the command.
function agentTagHtml(aName) {
  const title = aName === 'user'
    ? ' title="No agent marker detected — a human terminal or an unidentified invoker (ENGRAM_AGENT=<name> labels it precisely)"'
    : '';
  return '<span class="agent-tag agent-tag-' + escapeHtml(aName) + '"' + title + '>' + escapeHtml(aName) + '</span>';
}

// Exit-code badge shared by both live feeds and the overview stream.
function statusBadgeHtml(exitCode) {
  let statusClass = 'status-ok';
  let statusText = 'ok';
  if (exitCode === 1) {
    statusClass = 'status-warn';
    statusText = 'empty';
  } else if (exitCode === 2) {
    statusClass = 'status-ambig';
    statusText = 'ambiguous';
  } else if (exitCode >= 3) {
    statusClass = 'status-err';
    statusText = 'err ' + exitCode;
  }
  return '<span class="badge ' + statusClass + '">' + statusText + '</span>';
}

// Shared reliability subtitle: zero calls is unmeasured, not a perfect score.
function reliabilityText(totalCalls, totalErrors, data) {
  if (totalCalls === 0) return '—';
  const rel = data.graph_reliability_pct !== undefined ? data.graph_reliability_pct : (data.reliability_pct || 100.0);
  return totalErrors > 0
    ? rel.toFixed(1) + '% clean (' + totalErrors + ' errs)'
    : 'No errors recorded';
}

function createRepoRow(onSelect) {
  const tr = document.createElement('tr');
  tr.className = 'repo-row-item';
  tr.tabIndex = 0;
  tr.setAttribute('role', 'row');
  tr.onclick = onSelect;
  tr.onkeydown = (ev) => {
    if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); onSelect(); }
  };
  return tr;
}

function repoTrackCellHtml(labelHtml, badgeHtml, pct, extraHtml) {
  return '<td>' +
    '  <div class="repo-name-wrap">' + labelHtml + ' ' + badgeHtml + (extraHtml || '') + '</div>' +
    '  <div class="row-track"><div class="row-track-fill" style="width: ' + pct.toFixed(1) + '%;"></div></div>' +
    '</td>';
}

function createQueryRow(q, detailed) {
  const tr = document.createElement('tr');
  tr.className = 'query-row';
  const rName = q.repo_name || UNATTRIBUTED;
  // A linked worktree keeps its own name in the feed, but scoping filters and
  // the row's provenance follow the main repository it belongs to.
  const mainName = q.repo_main_name || '';
  const aName = q.agent || 'user';
  const exitCode = parseInt(q.exit_code || 0, 10);
  if (detailed) {
    tr.setAttribute('data-repo', mainName || rName);
    tr.setAttribute('data-agent', aName);
    tr.setAttribute('data-code', exitCode);
  }
  const ts = q.ts ? new Date(q.ts * 1000).toLocaleTimeString() : '—';
  const fromMain = mainName
    ? ' <span class="text-muted wt-from">from ' + escapeHtml(mainName) + '</span>'
    : '';
  let tail = '<td>' + statusBadgeHtml(exitCode) + '</td>';
  if (detailed) {
    const latStr = q.latency_us ? (q.latency_us / 1000.0).toFixed(1) + 'ms' : '—';
    const argsRaw = JSON.stringify(q.args || {});
    const argsShort = argsRaw.length > 85 ? argsRaw.substring(0, 82) + '...' : argsRaw;
    const argsHuman = explainToolArgs(q.tool, q.args) || argsShort;
    tail =
      '<td class="text-right num-col">' + (q.shown || q.result_count || 0) + '</td>' +
      '<td class="text-right num-col">' + latStr + '</td>' +
      tail +
      '<td class="args-cell" title="' + escapeHtml(argsRaw) + '"><code>' + escapeHtml(argsHuman) + '</code></td>';
  }
  tr.innerHTML =
    '<td class="num-col text-muted">' + ts + '</td>' +
    '<td><span class="repo-tag">' + escapeHtml(rName) + '</span>' + fromMain + '</td>' +
    '<td>' + agentTagHtml(aName) + '</td>' +
    '<td><code>' + escapeHtml(q.tool || '—') + '</code></td>' +
    tail;
  return tr;
}

// ============================= Theme =============================
function initTheme() {
  const saved = localStorage.getItem('eg-theme')
    || (window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light');
  state.theme = saved;
  document.documentElement.setAttribute('data-theme', saved);
  updateThemeButtonText(saved);
}

function toggleTheme() {
  const current = document.documentElement.getAttribute('data-theme') || 'light';
  const next = current === 'light' ? 'dark' : 'light';
  state.theme = next;
  document.documentElement.setAttribute('data-theme', next);
  localStorage.setItem('eg-theme', next);
  updateThemeButtonText(next);
}

function updateThemeButtonText(theme) {
  setText('theme-toggle-btn', 'Theme: ' + (theme === 'dark' ? 'Dark' : 'Light'));
}

// ========================= SSE Transport Beacon =========================
function setConnState(mode) {
  state.connected = mode === 'live';
  const beacon = document.getElementById('conn-beacon');
  if (!beacon) return;
  beacon.classList.remove('is-live', 'is-syncing', 'is-offline');
  beacon.classList.add('is-' + mode);
  const label = mode === 'live' ? 'Live' : (mode === 'offline' ? 'Offline' : 'Syncing');
  const text = beacon.querySelector('.beacon-text');
  if (text) text.textContent = label;
}

// =========================== Tab Switching ===========================
// The allowlist is the guard: an unknown key would otherwise hide every panel.
function switchTab(tab) {
  if (!TAB_KEYS.includes(tab)) {
    console.warn('switchTab: unknown tab "' + tab + '" — falling back to overview');
    tab = 'overview';
  }
  state.activeTab = tab;
  TAB_KEYS.forEach(t => {
    const btn = document.getElementById('tab-' + t + '-btn');
    const content = document.getElementById('tab-' + t + '-content');
    const selected = t === tab;
    if (btn) {
      btn.classList.toggle('active', selected);
      btn.setAttribute('aria-selected', selected ? 'true' : 'false');
      btn.tabIndex = selected ? 0 : -1;
    }
    if (content) content.classList.toggle('hidden', !selected);
  });

  if (tab === 'reuse') {
    populateReuseRepoFilter();
    renderReuseList();
    renderPrewriteTable();
    if (state.summary) renderReuseReposTable(state.summary.repo_aggs || {});
    loadReuseData();
  } else if (tab === 'memory') {
    populateMemoryStoreFilter();
    renderMemoryList();
    if (state.summary) renderMemoryReposTable(state.summary.repo_aggs || {});
    loadMemoriesData();
  } else if (tab === 'overview') {
    renderOverviewActivity();
    if (state.summary) {
      renderOverviewRepos(state.summary.repo_aggs || {});
      renderOverviewAgents(state.summary.agent_aggs || {});
    }
  }
  loadSummaryData();
}

// ========================= Errors & Announcements =========================
// Screen-reader announcements: state transitions only, never ordinary rows.
let lastAnnounce = 0;
function announce(text) {
  const region = document.getElementById('live-region');
  if (!region) return;
  const now = Date.now();
  if (now - lastAnnounce < 1000) return;
  lastAnnounce = now;
  region.textContent = text;
}

function showFetchError(tab, message) {
  const el = document.getElementById('data-error-' + tab);
  if (!el) return;
  el.textContent = 'Data unavailable: ' + message + ' — retry with the button.';
  el.classList.remove('hidden');
  if (!el.querySelector('button')) {
    const btn = document.createElement('button');
    btn.className = 'btn-touch';
    btn.textContent = 'Retry';
    btn.addEventListener('click', () => {
      if (tab === 'telemetry') loadSummaryData();
      else if (tab === 'reuse') loadReuseData();
      else loadMemoriesData();
    });
    el.appendChild(btn);
  }
}

function clearFetchError(tab) {
  const el = document.getElementById('data-error-' + tab);
  if (el) {
    el.classList.add('hidden');
    el.textContent = '';
  }
}

function setDataAge(generatedAt) {
  setText('data-age', generatedAt
    ? 'Data as of ' + new Date(generatedAt * 1000).toLocaleTimeString()
    : '');
}

// ======================= Selective Invalidation =======================
// Debounce timers. No reset on later events: the timer fires once per window.
// A trailing debounce that resets on every event starves under a steady stream.
let summaryDebounceTimer = null;
let reuseDebounceTimer = null;
let memoriesDebounceTimer = null;

function triggerSummaryRefresh() {
  if (summaryDebounceTimer) return;
  summaryDebounceTimer = setTimeout(() => { summaryDebounceTimer = null; loadSummaryData(); }, 3000);
}

function triggerReuseRefresh() {
  if (state.activeTab !== 'reuse' || reuseDebounceTimer) return;
  reuseDebounceTimer = setTimeout(() => { reuseDebounceTimer = null; loadReuseData(); }, 1000);
}

function triggerMemoriesRefresh() {
  if (memoriesDebounceTimer) return;
  memoriesDebounceTimer = setTimeout(() => { memoriesDebounceTimer = null; loadMemoriesData(); }, 500);
}

function flashElement(el) {
  if (!el) return;
  el.classList.remove('metric-flash');
  void el.offsetWidth;
  el.classList.add('metric-flash');
}

function handleLiveGraphEvent(q) {
  const recentTbody = document.getElementById('recent-tbody');
  if (recentTbody) {
    if (recentTbody.querySelector('.empty-cell')) recentTbody.innerHTML = '';
    recentTbody.insertBefore(createQueryRow(q, true), recentTbody.firstChild);
    recentQueryTotal += 1;
    while (recentTbody.children.length > FEED_LIMIT) {
      recentTbody.removeChild(recentTbody.lastChild);
    }
    updateListCount('recent-list-count', recentTbody.children.length, recentQueryTotal, 'newest first');
    applyTelemetryFilters();
  }
  flashElement(document.getElementById('metric-total-calls'));
  state.recentQueries.unshift({
    ts: q.ts, repo_name: q.repo_name, repo_main_name: q.repo_main_name,
    agent: q.agent, tool: q.tool, exit_code: q.exit_code,
  });
  state.recentQueries = state.recentQueries.slice(0, OVERVIEW_FEED_LIMIT);
  if (state.activeTab === 'overview') renderOverviewActivity();
  flashElement(document.getElementById('ovkpi-queries'));
}

function handleLiveEvent(q) {
  const tool = q.tool || '';
  if (GRAPH_TOOLS.includes(tool)) {
    handleLiveGraphEvent(q);
  } else if (tool === 'search' || tool === 'reuse') {
    const args = q.args || {};
    state.reuse.prewrite.unshift({
      ts: q.ts, repo_name: q.repo_name, tool: tool,
      terms: args.terms || args.paths || '',
      shown: q.shown || q.result_count || 0, latency_us: q.latency_us || 0,
    });
    if (state.reuse.prewrite.length > 40) state.reuse.prewrite.pop();
    renderPrewriteTable();
    const feedCountEl = document.getElementById('reuse-feed-count');
    if (feedCountEl) {
      const cur = parseInt(feedCountEl.textContent.replace(/,/g, ''), 10) || 0;
      feedCountEl.textContent = (cur + 1).toLocaleString();
      flashElement(feedCountEl);
    }
  } else if (tool.startsWith('mem_')) {
    updateMemoryKpis();
    flashElement(document.getElementById('mem-kpi-currency'));
    flashElement(document.getElementById('ovkpi-currency'));
  }

  triggerSummaryRefresh();
  if (tool === 'build') triggerReuseRefresh();
  else if (tool === 'mem_write' || tool === 'mem_delete' || tool === 'mem_touch') triggerMemoriesRefresh();
}

// Hover help. One copy table, one shared node, delegated listeners.
const TIP = {
  'kpi.graph_queries': 'Code lookups agents ran against the index instead of reading files, including eg build index refreshes.',
  'kpi.p50': 'Half of all queries finished faster than this.',
  'kpi.tokens_saved': 'Estimated tokens an agent avoided spending on source reads, including eg build re-runs (coefficients, not a measurement).',
  'kpi.candidates': 'Functions that look like near-copies of another function.',
  'kpi.prewrite': 'Pre-write searches and reuse scans run by agents to discover existing helpers before authoring new code.',
  'kpi.avg_callers': 'Average number of call sites using each canonical helper, indicating strong codebase consolidation.',
  'kpi.judge': 'Gray-band candidates the local eg-judge binary rejected as semantically distinct after the static detectors flagged them; the LOC figure is the measured span of the rejected symbols.',
  'kpi.currency': 'Share of current notes whose code refs resolve to the current index. Freshness is bounded by the last eg build. Notes without refs are excluded; notes in stores without an index are unverified.',
  'kpi.active_notes': 'Total current notes (decisions, gotchas, facts, tasks, procedures) retained across agent sessions.',
  'kpi.recalls': 'Total times agents looked up exact memory notes across all active stores in the retained journal.',
  'col.last_active': 'Most recent run recorded for this row.',
  'col.p50': 'Half of runs were faster than this.',
  'col.items': 'Results shown to the agent by the query.',
  'col.reliability': 'Share of runs that finished without error.',
  'col.tokens_saved': 'Estimated tokens avoided instead of reading files (coefficients, not a measurement).',
  'col.reuse_tokens_saved': 'Estimated tokens avoided by search and reuse calls (coefficients, not a measurement).',
  'col.search_hit_rate': 'Share of pre-write searches that returned at least one result. Misses can be healthy — the twin genuinely does not exist yet. "—" when the row has no searches.',
  'col.candidates_sample': 'Duplicate twins per repository: shown is the top-overlap sample (at most 25); after the slash is the repository\u2019s full candidate pool. "\u2014" when the repository was not scanned.',
  'col.latest_scan': 'Newest successful eg reuse scan in the retained journal. "Truncated" is the journaled verdict that results were cut from a larger pool. The Δ badge compares against the previous scan in the window.',
  'col.results_shown': 'Results the search or scan showed to the agent.',
  'col.reuse_calls': 'Anti-reinvention surface calls by this agent: pre-write searches plus full eg reuse scans.',
  'col.reads_writes': 'Memory reads (query, get, export) versus write operations (write, touch, delete, gc, import).',
  'col.memory_hit_rate': 'Share of memory reads that returned at least one item (query results, exact-key gets, exported rows). Failed reads count against the rate. "—" when the row has no reads.',
  'card.overlap': 'Measured evidence overlap for D1/D2; D3/D4/D5/D1L/DT show their evidence class instead of a percentage.',
  'card.callers': 'Call sites using the canonical helper.',
  'card.confidence': 'Static confidence tier; stale or unknown freshness caps it at low.',
  'card.usage': 'How many eg reuse runs in the retained journal surfaced this candidate, and when the last one ran.',
  'card.recalled': 'Exact-key mem_get recalls in the retained journal.',
  'card.verified': 'Last Verify (anchor) timestamp; the creation date never changes.',
  'view.memory': 'Collapse the note list: Top 10 by latest activity (created, verified, or recalled), most used, or all notes.',
  'view.reuse': 'Collapse the candidate list by canonical call-site usage, or show all sampled candidates.',
  'card.loc_saving': 'Lines removed if the duplicate is merged away.',
  'det.all': 'Show every detection rule. Pill counts are sample floors from the per-repo scan (at most 25 candidates each), not corpus totals.',
  'header.data_age': 'Timestamp the payload was built. All counts cover the retained journal: the current log plus one rotated generation (about 10 MiB of history).',
  'header.conn': 'Live event stream state: Live (connected), Syncing (connecting), Offline (reconnecting). Events missed while offline are recovered by a full refetch.',
  'det.d1': 'Both functions call the same downstream functions.',
  'det.d2': 'Same parameter types and name tokens.',
  'det.d3': 'Calls a raw client the project already wraps.',
  'det.d4': 'One-caller wrapper that only forwards a call.',
  'det.d5': 'Both are method-shape twins of the same class.',
  'det.d1l': 'Both call the same not-yet-indexed functions.',
  'det.dt': 'Both are test helpers sharing token shapes.',
  'filter.status': 'Show only runs with these exit codes.',
  'filter.superseded': 'Show older revisions replaced by a newer note.'
};

let tipSeq = 0;

function attachTipDescriptions(root) {
  const scope = root || document;
  scope.querySelectorAll('[data-tip]').forEach((el) => {
    const text = TIP[el.getAttribute('data-tip')];
    if (!text || el.querySelector('.sr-only')) return;
    tipSeq += 1;
    const id = 'tip-' + tipSeq;
    const span = document.createElement('span');
    span.className = 'sr-only';
    span.id = id;
    span.textContent = text;
    el.appendChild(span);
    el.setAttribute('aria-describedby', id);
  });
}

function initTips() {
  const tip = document.createElement('div');
  tip.className = 'eg-tip';
  tip.id = 'eg-tip';
  tip.setAttribute('role', 'tooltip');
  document.body.appendChild(tip);
  attachTipDescriptions(document);

  let timer = null;
  let current = null;
  const place = (el) => {
    const r = el.getBoundingClientRect();
    const t = tip.getBoundingClientRect();
    let top = r.top - t.height - 8;
    if (top < 8) top = r.bottom + 8;
    const left = Math.max(8, Math.min(r.left + r.width / 2 - t.width / 2, window.innerWidth - t.width - 8));
    tip.style.top = Math.round(top) + 'px';
    tip.style.left = Math.round(left) + 'px';
  };
  const show = (el) => {
    const text = TIP[el.getAttribute('data-tip')];
    if (!text) return;
    current = el;
    tip.textContent = text;
    tip.classList.add('visible');
    place(el);
  };
  const hide = () => { clearTimeout(timer); current = null; tip.classList.remove('visible'); };

  document.addEventListener('mouseover', (e) => {
    if (window.matchMedia('(pointer: coarse)').matches) return;
    const el = e.target.closest('[data-tip]');
    if (!el || el === current) return;
    clearTimeout(timer);
    timer = setTimeout(() => show(el), 250);
  });
  document.addEventListener('mouseout', (e) => {
    const el = e.target.closest('[data-tip]');
    if (el) { if (el === current) hide(); else clearTimeout(timer); }
  });
  document.addEventListener('focusin', (e) => { const el = e.target.closest('[data-tip]'); if (el) show(el); });
  document.addEventListener('focusout', hide);
  document.addEventListener('keydown', (e) => { if (e.key === 'Escape') hide(); });
  window.addEventListener('scroll', hide, true);
}

window.addEventListener('DOMContentLoaded', () => {
  initTheme();
  initTips();
  setConnState('syncing');
  initDelegation();
  initTabKeyboard();
  initEventSource();
  loadSummaryData();
  loadReuseData();
  loadMemoriesData();
});

// One delegated listener replaces every inline handler (CSP: no unsafe-inline).
function initDelegation() {
  document.addEventListener('click', (e) => {
    const themeBtn = e.target.closest('#theme-toggle-btn');
    if (themeBtn) { toggleTheme(); return; }
    const tabBtn = e.target.closest('[data-tab]');
    if (tabBtn) { switchTab(tabBtn.dataset.tab); return; }
    const healthBtn = e.target.closest('[data-health-action]');
    if (healthBtn) { switchTab(healthBtn.dataset.healthAction); return; }
    const pill = e.target.closest('[data-det]');
    if (pill) { setReuseDetectorFilter(pill.dataset.det); return; }
    const storePill = e.target.closest('[data-store]');
    if (storePill) { setMemoryStoreFilter(storePill.dataset.store); return; }
    const touchBtn = e.target.closest('.btn-touch[data-key]');
    if (touchBtn) { touchMemory(touchBtn); return; }
    const verifyAll = e.target.closest('#verify-all-btn');
    if (verifyAll) { verifyAllModified(); return; }
  });
  document.addEventListener('input', (e) => {
    const id = e.target.id;
    if (id === 'query-search') applyTelemetryFilters();
    else if (id === 'reuse-search') { state.reuse.selectedId = null; renderReuseList(); }
    else if (id === 'mem-search') renderMemoryList();
  });
  document.addEventListener('change', (e) => {
    const id = e.target.id;
    if (id === 'scope-filter') filterByRepo(e.target.value);
    else if (id === 'repo-filter') { syncRepoScope(e.target.value); applyTelemetryFilters(); }
    else if (id === 'agent-filter' || id === 'status-filter') applyTelemetryFilters();
    else if (id === 'reuse-repo-filter') syncRepoScope(e.target.value);
    else if (id === 'mem-store-filter') { syncMemoryStorePills(e.target.value); renderMemoryList(); }
    else if (id === 'mem-kind-filter') { state.memories.filterKind = e.target.value; renderMemoryList(); }
    else if (id === 'mem-show-superseded') renderMemoryList();
    else if (id === 'mem-view' && MEMORY_VIEWS[e.target.value]) { memView = e.target.value; renderMemoryList(); }
    else if (id === 'reuse-view' && REUSE_VIEWS[e.target.value]) { reuseView = e.target.value; renderReuseList(); }
  });
  // `T` flips the theme from anywhere outside a text control.
  document.addEventListener('keydown', (e) => {
    if (e.key !== 't' && e.key !== 'T') return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT' || t.isContentEditable)) return;
    toggleTheme();
  });
}

function initTabKeyboard() {
  const nav = document.querySelector('.nav-tabs');
  if (!nav) return;
  nav.addEventListener('keydown', (e) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
    const idx = TAB_KEYS.indexOf(state.activeTab);
    let next = idx;
    if (e.key === 'ArrowLeft') next = (idx + TAB_KEYS.length - 1) % TAB_KEYS.length;
    else if (e.key === 'ArrowRight') next = (idx + 1) % TAB_KEYS.length;
    else if (e.key === 'Home') next = 0;
    else if (e.key === 'End') next = TAB_KEYS.length - 1;
    e.preventDefault();
    switchTab(TAB_KEYS[next]);
    const btn = document.getElementById('tab-' + state.activeTab + '-btn');
    if (btn) btn.focus();
  });
}

function refreshVisibleData() {
  loadSummaryData();
  if (state.activeTab === 'reuse') loadReuseData();
  if (state.activeTab === 'memory') loadMemoriesData();
}

function initEventSource() {
  isDocumentVisible = !document.hidden;
  const source = new EventSource('/events');
  source.onopen = function () { setConnState('live'); refreshVisibleData(); };
  source.onerror = function () { setConnState('offline'); };
  source.addEventListener('reset', function () {
    announce('Live stream reset; data reloaded.');
    refreshVisibleData();
  });
  source.onmessage = function (e) {
    try { handleLiveEvent(JSON.parse(e.data)); } catch (err) {}
  };
  document.addEventListener('visibilitychange', () => {
    isDocumentVisible = !document.hidden;
    if (isDocumentVisible) refreshVisibleData();
  });
  // Fallback poll: keeps KPIs fresh when the stream is quiet or degraded.
  setInterval(() => { if (isDocumentVisible) refreshVisibleData(); }, 15000);
}
