// ========================= Anti-Reinvention UI =========================

function viewReuseCandidates(items) {
  const spec = REUSE_VIEWS[reuseView] || REUSE_VIEWS.used10;
  const sorted = items.slice().sort((a, b) => {
    const callers = (b.canonical_callers || 0) - (a.canonical_callers || 0);
    if (callers !== 0) return callers;
    const ap = a.overlap_applicable ? (a.overlap_pct || 0) : -1;
    const bp = b.overlap_applicable ? (b.overlap_pct || 0) : -1;
    if (bp !== ap) return bp - ap;
    return String(a.id).localeCompare(String(b.id));
  });
  const visible = spec.limit > 0 ? sorted.slice(0, spec.limit) : sorted;
  return { items: visible, total: items.length, label: spec.label };
}

// Reuse "last used" comes from the retained journal: an `eg reuse` run that
// surfaced this candidate. Zero means no run in the retained window.
function reuseUsageText(c) {
  if (!c.reuse_count) return 'never surfaced in retained journal';
  const when = c.last_reused_ts
    ? new Date(c.last_reused_ts * 1000).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
    : '—';
  return 'surfaced ' + c.reuse_count + '\u00d7 \u00b7 last used ' + when;
}

const DETECTOR_EVIDENCE_LABELS = {
  D3: 'shared guard',
  D4: 'pass-through wrapper',
  D5: 'class shape',
  D1L: 'unresolved call',
  DT: 'test overlap',
};

function detectorEvidenceLabel(detector) {
  return DETECTOR_EVIDENCE_LABELS[detector] || 'evidence';
}

function renderReuseKpis(summary, totalCandidates) {
  setText('reuse-kpi-count', totalCandidates.toLocaleString());

  const cacheNote = summary.cache_hit ? ' · cached ' + Math.round((summary.cache_age_ms || 0) / 1000) + 's' : '';
  const closure = summary.resolved_twins || {};
  const resolvedNote = (closure.surfaced || 0) > 0
    ? ' · ' + (closure.resolved || 0) + ' of ' + closure.surfaced + ' surfaced twins resolved (sample-complete repos)'
    : '';
  const locNote = summary.preventable_loc ? ' · ~' + summary.preventable_loc.toLocaleString() + ' LOC preventable' : '';
  const countPrefix = totalCandidates > state.reuse.candidates.length
    ? 'Top ' + state.reuse.candidates.length + ' of ' + totalCandidates.toLocaleString() + ' duplicate twins'
    : 'Syntactic & structural duplicate twins';
  setText('reuse-kpi-count-sub', countPrefix + locNote + resolvedNote + cacheNote);

  const prewriteCount = summary.prewrite_count !== undefined ? summary.prewrite_count : state.reuse.prewrite.length;
  setText('reuse-feed-count', prewriteCount.toLocaleString());
  setText('reuse-kpi-prewrite', prewriteCount.toLocaleString());
  setText('reuse-kpi-prewrite-sub', prewriteCount > 0
    ? 'Proactive searches before code authoring'
    : 'No pre-write lookups recorded');

  const avg = summary.avg_callers;
  const hasAvg = avg !== null && avg !== undefined && avg > 0;
  setText('reuse-kpi-callers', hasAvg ? avg.toFixed(1) + 'x' : '—');
  setText('reuse-kpi-callers-sub', hasAvg ? 'Avg callers per authoritative helper' : 'No canonical callers recorded');

  const poolsMeasured = Object.keys(state.reuse.pools).length > 0;
  setText('ovkpi-candidates', poolsMeasured ? totalCandidates.toLocaleString() : '—');
  setText('ovkpi-candidates-sub', !poolsMeasured
    ? 'No repositories scanned'
    : (totalCandidates > state.reuse.candidates.length
      ? 'Sampled ' + state.reuse.candidates.length + ' of pool ' + totalCandidates.toLocaleString()
      : 'Pool ' + totalCandidates.toLocaleString() + ' (complete sample)'));
}

// Judge tile: rendered ONLY with real judged data from the journal.
function renderJudgeTile(kpiStrip, judgeStats) {
  document.getElementById('reuse-kpi-judge')?.remove();
  if (!kpiStrip || !((judgeStats.reviewed || 0) > 0 && (judgeStats.rejected || 0) > 0)) return;
  const tile = document.createElement('div');
  tile.className = 'kpi-tile';
  tile.id = 'reuse-kpi-judge';
  const locNote = (judgeStats.rejected_loc || 0) > 0
    ? ' · ' + judgeStats.rejected_loc.toLocaleString() + ' LOC redundant'
    : '';
  tile.innerHTML =
    '<div class="kpi-label" data-tip="kpi.judge">Judge Arbitration</div>' +
    '<div class="kpi-value badge-judge" id="reuse-kpi-judge-value">' +
      judgeStats.rejected.toLocaleString() + '</div>' +
    '<div class="kpi-sub" id="reuse-kpi-judge-sub">' +
      'Gray-band candidates rejected' + locNote +
      ' of ' + judgeStats.reviewed.toLocaleString() + ' reviewed</div>';
  kpiStrip.appendChild(tile);
  attachTipDescriptions(tile);
}

function loadReuseData() {
  fetch('/api/reuse')
    .then(r => {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    })
    .then(data => {
      clearFetchError('reuse');
      state.reuse.candidates = data.candidates || [];
      state.reuse.prewrite = data.prewrite_searches || [];

      const summary = data.summary || {};
      state.reuse.summary = summary;
      const errMsg = (summary.errors || []).map(e => (e.repo || 'repo') + ': ' + e.message).join('; ');
      if (summary.degraded) {
        showFetchError('reuse', errMsg);
      }
      state.health.reuseDegraded = !!summary.degraded;
      state.health.reuseError = errMsg;
      renderOverviewHealth();
      state.reuse.pools = summary.repo_candidate_pools || {};

      const totalCandidates = summary.total_candidates !== undefined
        ? summary.total_candidates
        : state.reuse.candidates.length;
      renderReuseKpis(summary, totalCandidates);

      const callersKpi = document.getElementById('reuse-kpi-callers');
      renderJudgeTile(callersKpi && callersKpi.closest('.kpi-strip'), summary.judge || {});

      updateDetectorPills(summary.detector_counts || {}, state.reuse.candidates.length);
      populateReuseRepoFilter();
      renderReuseList();
      renderPrewriteTable();
      renderReuseReposTable(state.summary ? (state.summary.repo_aggs || {}) : {});
    })
    .catch(err => showFetchError('reuse', err.message));
}

function updateDetectorPills(counts, total) {
  const labels = {
    D1: 'D1 Callee', D2: 'D2 Interface', D3: 'D3 Bypass',
    D4: 'D4 Wrapper', D5: 'D5 Class', D1L: 'D1L Unresolved', DT: 'DT Tests',
  };
  document.querySelectorAll('#reuse-detector-pills .filter-pill').forEach(pill => {
    const det = pill.getAttribute('data-det');
    if (det === 'ALL') pill.textContent = 'All (' + total + ')';
    else if (counts && counts[det] !== undefined) pill.textContent = (labels[det] || det) + ' (' + counts[det] + ')';
  });
}

function reuseCandidateCountsByRepo() {
  const counts = {};
  state.reuse.candidates.forEach(c => {
    const repo = c.repo_path || c.repo || '(unattributed)';
    counts[repo] = (counts[repo] || 0) + 1;
  });
  return counts;
}

function searchHitRateCell(rData) {
  const calls = rData.search_calls || 0;
  const pct = rData.search_hit_rate_pct;
  if (calls === 0 || pct === null || pct === undefined) return '<span class="text-muted">—</span>';
  return '<span class="text-right">' + pct.toFixed(1) + '%</span> <span class="text-muted">(' + calls + ' searches)</span>';
}

function lastScanCell(rData) {
  const scan = rData.last_reuse_scan;
  if (!scan) return '<span class="text-muted">—</span>';
  const when = new Date(scan.ts * 1000).toLocaleDateString();
  const size = scan.result_count.toLocaleString() + ' candidates'
    + (scan.limit_capped ? ' <span class="text-muted">(truncated)</span>' : '');
  let delta = '';
  if (scan.prev_result_count !== undefined && scan.prev_result_count !== null) {
    const d = scan.result_count - scan.prev_result_count;
    if (d !== 0) {
      const sign = d > 0 ? '+' : '−';
      const cls = d > 0 ? 'status-warn' : 'status-ok';
      const baseline = scan.prev_ts ? ' vs previous scan ' + new Date(scan.prev_ts * 1000).toLocaleDateString() : '';
      delta = ' <span class="badge ' + cls + '" title="Pool change' + baseline + '">Δ ' + sign + Math.abs(d).toLocaleString() + '</span>';
    }
  }
  return '<span class="text-muted">' + when + ' ·</span> ' + size + delta;
}

function renderReuseReposTable(repoAggs) {
  const tbody = document.getElementById('reuse-repos-tbody');
  if (!tbody) return;
  const entries = Object.entries(repoAggs).filter(([_, rData]) => (rData.reuse_calls || 0) > 0);
  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="6" class="empty-cell">No anti-reinvention workspace activity recorded</td></tr>';
    return;
  }

  const candidateCounts = reuseCandidateCountsByRepo();
  entries.sort((a, b) => (b[1].reuse_calls || 0) - (a[1].reuse_calls || 0));
  const maxCalls = entries[0][1].reuse_calls || 1;
  tbody.innerHTML = '';
  entries.forEach(([rName, rData]) => {
    const label = rData.display || rName;
    const calls = rData.reuse_calls || 0;
    const saved = rData.reuse_saved_tokens || 0;
    const pct = (calls / maxCalls) * 100.0;
    const wtBadge = rData.worktree ? '<span class="badge badge-wt">worktree</span>' : '<span class="badge badge-repo">repo</span>';
    const sampled = candidateCounts[rName];
    const pool = state.reuse.pools[rName];
    const candCell = pool === undefined
      ? '<span class="text-muted">—</span>'
      : (pool === 0 ? '0' : (sampled || 0).toLocaleString() + ' <span class="text-muted">/ ' + pool.toLocaleString() + '</span>');

    const tr = createRepoRow(() => filterReuseByRepo(label));
    tr.innerHTML =
      repoTrackCellHtml('<strong>' + escapeHtml(label) + '</strong>', wtBadge, pct) +
      '<td class="text-right num-col">' + calls.toLocaleString() + '</td>' +
      '<td class="text-right num-col">' + searchHitRateCell(rData) + '</td>' +
      '<td class="text-right num-col">' + candCell + '</td>' +
      '<td class="text-right num-col">' + lastScanCell(rData) + '</td>' +
      '<td class="text-right num-col text-muted">~' + saved.toLocaleString() + '</td>';
    tbody.appendChild(tr);
  });
}

function filterReuseByRepo(repoName) {
  const select = document.getElementById('reuse-repo-filter');
  if (!select) return;
  if (!Array.from(select.options).some(o => o.value === repoName)) {
    const opt = document.createElement('option');
    opt.value = repoName;
    opt.textContent = repoName;
    select.appendChild(opt);
  }
  select.value = (select.value === repoName) ? 'ALL' : repoName;
  if (select.value === 'ALL') {
    announce('Filter reset: all repositories');
  } else {
    announce('Filtered by repository ' + repoName);
    syncRepoScope(select.value);
  }
  renderReuseList();
}

function populateReuseRepoFilter() {
  const select = document.getElementById('reuse-repo-filter');
  if (!select) return;
  const keep = select.value || 'ALL';
  const repos = new Set();
  state.reuse.candidates.forEach(c => repos.add(c.repo));
  populateRepoSelect('reuse-repo-filter', 'All Repositories', Array.from(repos).sort(), keep, true);
}

function setReuseDetectorFilter(det) {
  state.reuse.filterDet = det;
  document.querySelectorAll('.filter-pill[data-det]').forEach(btn => {
    const isAct = btn.getAttribute('data-det') === det;
    btn.classList.toggle('active', isAct);
    btn.setAttribute('aria-pressed', isAct ? 'true' : 'false');
  });
  state.reuse.selectedId = null;
  renderReuseList();
}

function getFilteredReuseCandidates() {
  const repoSelect = document.getElementById('reuse-repo-filter');
  const repo = repoSelect ? repoSelect.value : 'ALL';
  const searchInput = document.getElementById('reuse-search');
  const search = searchInput ? searchInput.value.toLowerCase().trim() : '';
  return state.reuse.candidates.filter(c => {
    if (repo !== 'ALL' && c.repo !== repo) return false;
    if (state.reuse.filterDet !== 'ALL' && c.detector !== state.reuse.filterDet) return false;
    if (search) {
      const matchSymbol = (c.symbol || '').toLowerCase().includes(search);
      const matchCanonical = (c.canonical || '').toLowerCase().includes(search);
      const matchFile = (c.canonical_file || '').toLowerCase().includes(search)
        || (c.candidate_info && (c.candidate_info.file || '').toLowerCase().includes(search));
      if (!matchSymbol && !matchCanonical && !matchFile) return false;
    }
    return true;
  });
}

function buildReuseCard(c, isSelected) {
  const card = document.createElement('div');
  card.className = 'select-item-card' + (isSelected ? ' selected' : '');
  card.tabIndex = 0;
  card.setAttribute('role', 'button');
  card.setAttribute('aria-pressed', isSelected ? 'true' : 'false');
  card.setAttribute('aria-label', 'Candidate ' + c.symbol + ' to ' + c.canonical);
  const select = () => { state.reuse.selectedId = c.id; renderReuseList(); };
  card.onclick = select;
  card.onkeydown = (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); select(); } };

  const overlapText = c.overlap_applicable ? c.overlap_pct + '% overlap' : escapeHtml(detectorEvidenceLabel(c.detector));
  const confBadge = c.confidence ? '<span class="badge" data-tip="card.confidence">' + escapeHtml(c.confidence) + '</span>' : '';
  const candFile = (c.candidate_info && c.candidate_info.file) ? c.candidate_info.file + ':' + c.candidate_info.line_start : '';
  const canFile = c.canonical_file + ':' + c.canonical_line;
  const fileLocHtml = candFile && candFile !== canFile
    ? '<span title="Candidate &rarr; Canonical">' + escapeHtml(candFile) + ' &rarr; ' + escapeHtml(canFile) + '</span>'
    : '<span>' + escapeHtml(canFile) + '</span>';

  card.innerHTML =
    '<div class="item-card-head">' +
    '  <div class="badge-row">' +
    '    <span class="badge badge-' + c.detector.toLowerCase() + '" data-tip="det.' + c.detector.toLowerCase() + '">' + c.detector + '</span>' +
    '    <span class="badge" data-tip="card.overlap">' + overlapText + '</span>' +
    '    ' + confBadge + '<span class="badge status-warn" data-tip="card.loc_saving">-' + c.loc_saving + ' LOC</span>' +
    '  </div>' +
    '  <span class="badge badge-repo">' + escapeHtml(c.repo) + '</span>' +
    '</div>' +
    '<div class="item-key">' + escapeHtml(c.symbol) + '</div>' +
    '<div class="item-snippet">&rarr; canonical: <code>' + escapeHtml(c.canonical) + '</code> <span class="badge status-ok" data-tip="card.callers">[callers=' + c.canonical_callers + ']</span></div>' +
    '<div class="item-meta">' + fileLocHtml +
    (c.shared_evidence ? '<span>&bull;</span><span>' + escapeHtml(c.shared_evidence) + '</span>' : '') +
    '<span>&bull;</span><span data-tip="card.usage">' + escapeHtml(reuseUsageText(c)) + '</span></div>';
  return card;
}

function renderReuseList() {
  const container = document.getElementById('reuse-cards-container');
  if (!container) return;
  const filteredAll = getFilteredReuseCandidates();
  const view = viewReuseCandidates(filteredAll);
  const filtered = view.items;
  updateListCount('reuse-list-count', filtered.length, view.total, view.label);

  if (filtered.length === 0) {
    const trulyEmpty = state.reuse.candidates.length === 0 && state.reuse.filterDet === 'ALL';
    container.innerHTML = trulyEmpty
      ? '<div class="empty-cell">No duplicate candidates found in the indexed corpus.</div>'
      : '<div class="empty-cell">No candidates match the current filters.</div>';
    renderReuseDossier(null);
    return;
  }

  let selIdx = filtered.findIndex(c => c.id === state.reuse.selectedId);
  if (selIdx < 0) {
    const stillVisible = filteredAll.some(c => c.id === state.reuse.selectedId);
    selIdx = 0;
    state.reuse.selectedId = filtered[0] ? filtered[0].id : null;
    if (stillVisible) announce('Selection moved: the previous candidate is outside the current view');
  }
  container.innerHTML = '';
  filtered.forEach(c => container.appendChild(buildReuseCard(c, c.id === state.reuse.selectedId)));
  attachTipDescriptions(container);
  renderReuseDossier(filtered[selIdx] || null);
}

function codePanelHtml(panelClass, badgeClass, badgeText, info, snippet) {
  const file = info ? info.file : '';
  const range = info ? (info.line_start + '-' + info.line_end) : '0-0';
  const path = file + (file ? ':' + range : '');
  return '<div class="code-panel ' + panelClass + '">' +
    '  <div class="code-panel-head">' +
    '    <span class="badge ' + badgeClass + '">' + badgeText + '</span>' +
    '    <span class="code-panel-file" title="' + escapeHtml(path) + '">' + escapeHtml(path) + '</span>' +
    '  </div>' +
    '  <pre><code>' + escapeHtml(snippet || '(Source snippet unavailable)') + '</code></pre>' +
    '</div>';
}

function renderReuseDossier(c) {
  const container = document.getElementById('reuse-dossier-container');
  if (!container) return;
  container.scrollTop = 0;
  if (!c) {
    container.innerHTML = '<div class="empty-cell dossier-empty">Select a duplication candidate to inspect side-by-side source code.</div>';
    return;
  }

  const overlapText = c.overlap_applicable ? c.overlap_pct + '% overlap' : escapeHtml(detectorEvidenceLabel(c.detector));
  container.innerHTML =
    '<div class="dossier-head">' +
    '  <div class="dossier-head-row">' +
    '    <div class="badge-row">' +
    '      <span class="badge badge-' + c.detector.toLowerCase() + '" data-tip="det.' + c.detector.toLowerCase() + '">' + c.detector + '</span>' +
    '      <span class="badge status-ok" data-tip="card.overlap">' + overlapText + '</span>' +
    '      <span class="badge badge-repo">' + escapeHtml(c.repo) + '</span>' +
    '    </div>' +
    '    <span class="badge status-warn" data-tip="card.loc_saving">Save ~' + c.loc_saving + ' LOC</span>' +
    '  </div>' +
    '  <div class="dossier-title">' + escapeHtml(c.symbol) + '</div>' +
    '  <div class="dossier-sub">Authoritative Target: <strong>' + escapeHtml(c.canonical) + '</strong> <span class="badge status-ok">[callers=' + c.canonical_callers + ']</span></div>' +
    '</div>' +
    '<div class="prevention-banner"><strong>What This Prevents:</strong> ' + escapeHtml(c.prevention_narrative) + '</div>' +
    (c.shared_evidence ? '<div class="dossier-section-title">Shared Footprint &amp; Overlap Evidence</div><div class="chips-row"><span class="chip chip-tag">' + escapeHtml(c.shared_evidence) + '</span></div>' : '') +
    '<div class="dossier-section-title">Side-by-Side Source Code Comparison</div>' +
    '<div class="side-by-side-grid">' +
      codePanelHtml('code-panel-candidate', 'status-warn', 'DUPLICATE CANDIDATE', c.candidate_info, c.candidate_snippet) +
      codePanelHtml('code-panel-canonical', 'status-ok', 'CANONICAL REUSE TARGET', c.canonical_info, c.canonical_snippet) +
    '</div>' +
    '<div class="action-recommendation">' +
    '  <strong>Consolidation Recommendation:</strong> Import authoritative helper <code>' + escapeHtml(c.canonical) + '</code> <span class="badge status-ok" data-tip="card.callers">[callers=' + c.canonical_callers + ']</span> and remove redundant definition <code>' + escapeHtml(c.symbol) + '</code> to eliminate drift and save ~' + c.loc_saving + ' LOC.' +
    '</div>';
  attachTipDescriptions(container);
}

function renderPrewriteTable() {
  const tbody = document.getElementById('prewrite-tbody');
  if (!tbody) return;
  const prewriteSearches = state.reuse.prewrite;
  if (!prewriteSearches || prewriteSearches.length === 0) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty-cell">No pre-write helper searches recorded yet</td></tr>';
    updateListCount('prewrite-list-count', 0, 0, 'newest first');
    return;
  }

  const visible = prewriteSearches.slice(0, FEED_LIMIT);
  tbody.innerHTML = '';
  visible.forEach(s => {
    const tr = document.createElement('tr');
    const ts = s.ts ? new Date(s.ts * 1000).toLocaleTimeString() : '—';
    const rName = s.repo_name || UNATTRIBUTED;
    tr.innerHTML =
      '<td class="num-col text-muted">' + ts + '</td>' +
      '<td><span class="repo-tag">' + escapeHtml(rName) + '</span></td>' +
      '<td><code>' + escapeHtml(s.tool) + '</code></td>' +
      '<td class="args-cell" title="' + escapeHtml(s.terms || '') + '"><code>' + escapeHtml(
        explainToolArgs(s.tool, s.tool === 'reuse' ? { paths: s.terms } : { terms: s.terms })
        || s.terms || 'Every file'
      ) + '</code></td>' +
      '<td class="text-right num-col">' + (s.shown || 0) + '</td>';
    tbody.appendChild(tr);
  });
  updateListCount('prewrite-list-count', visible.length, prewriteSearches.length, 'newest first');
}
