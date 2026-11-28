// ===================== Telemetry Views Rendering =====================

function renderGraphKpis(data, totalCalls, totalErrors) {
  setText('metric-total-calls', totalCalls.toLocaleString());
  setText('metric-reliability', reliabilityText(totalCalls, totalErrors, data));

  // A null percentile means no latency samples: unmeasured, not instant.
  const p50 = data.graph_p50_ms !== undefined ? data.graph_p50_ms : data.p50_ms;
  setText('metric-p50', (p50 === null || p50 === undefined || totalCalls === 0)
    ? '—'
    : p50.toFixed(1) + 'ms');

  const p95 = data.graph_p95_ms !== undefined ? data.graph_p95_ms : data.p95_ms;
  setText('metric-latency-sub', (totalCalls === 0 || p95 === null || p95 === undefined)
    ? 'No measured queries'
    : 'p95 = ' + p95.toFixed(1) + 'ms');

  const tokens = data.graph_saved_tokens !== undefined ? data.graph_saved_tokens : (data.saved_tokens || 0);
  const dollars = data.graph_saved_dollars !== undefined ? data.graph_saved_dollars : (data.saved_dollars || 0.0);
  setText('metric-tokens', '~' + tokens.toLocaleString());
  setText('metric-dollars', '~$' + dollars.toFixed(2) + ' avoided vs full reads');
}

function loadSummaryData() {
  fetch('/api/summary')
    .then(r => {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    })
    .then(data => {
      clearFetchError('telemetry');
      setDataAge(data.generated_at);
      state.summary = data;
      const totalCalls = data.graph_calls !== undefined ? data.graph_calls : (data.total_calls || 0);
      const totalErrors = data.graph_errors !== undefined ? data.graph_errors : (data.error_count || 0);

      renderGraphKpis(data, totalCalls, totalErrors);
      renderReposTable(data.repo_aggs || {});
      renderAgentsTable(data.agent_aggs || {});
      renderReuseReposTable(data.repo_aggs || {});
      renderMemoryReposTable(data.repo_aggs || {});
      renderToolsTable(data.tool_aggs || {}, totalCalls);
      renderRecentQueries(data.graph_recent_queries || []);
      applyTelemetryFilters();

      state.health.graphErrors = totalErrors;
      renderOverviewHealth();
      updateMemoryKpis();
    })
    .catch(err => showFetchError('telemetry', err.message));
}

function populateRepoSelect(selectId, defaultLabel, labels, selectedVal, resetNotice) {
  const select = document.getElementById(selectId);
  if (!select) return;
  select.innerHTML = '<option value="ALL">' + defaultLabel + '</option>';
  labels.forEach(label => {
    const opt = document.createElement('option');
    opt.value = label;
    opt.textContent = label;
    select.appendChild(opt);
  });
  if (selectedVal && Array.from(select.options).some(o => o.value === selectedVal)) {
    select.value = selectedVal;
  } else {
    select.value = 'ALL';
    if (resetNotice && selectedVal && selectedVal !== 'ALL') {
      announce('Filter "' + selectedVal + '" reset: repository no longer active');
    }
  }
}

function buildRepoRow(rName, rData, maxCalls) {
  const label = rData.display || rName;
  const worktrees = Object.entries(rData.worktrees || {});
  const errs = rData.errors || 0;
  const pct = (rData.calls / maxCalls) * 100.0;
  const lastStr = rData.last_ts ? new Date(rData.last_ts * 1000).toLocaleString() : '—';
  const wtBadge = rData.worktree ? '<span class="badge badge-wt">worktree</span>' : '<span class="badge badge-repo">repo</span>';
  const wtCount = worktrees.length
    ? '<span class="text-muted wt-count">' + worktrees.length + (worktrees.length === 1 ? ' worktree' : ' worktrees') + '</span>'
    : '';
  const statusCell = errs === 0
    ? ''
    : '<span class="badge status-err">' + rData.reliability_pct.toFixed(1) + '% (' + errs + ' err)</span>';
  const labelHtml = '<strong class="repo-filter-label" role="button" tabindex="0" title="Filter telemetry by ' + escapeHtml(label) + '">' + escapeHtml(label) + '</strong>';

  const tr = createRepoRow(() => filterByRepo(label));
  tr.innerHTML =
    repoTrackCellHtml(labelHtml, wtBadge, pct, wtCount) +
    '<td class="text-right num-col">' + rData.calls.toLocaleString() + '</td>' +
    '<td>' + statusCell + '</td>' +
    '<td class="text-muted num-col">' + lastStr + '</td>';

  const labelTarget = tr.querySelector('.repo-filter-label');
  if (labelTarget) {
    labelTarget.onclick = (ev) => { ev.stopPropagation(); filterByRepo(label); };
    labelTarget.onkeydown = (ev) => {
      if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); ev.stopPropagation(); filterByRepo(label); }
    };
  }
  return tr;
}

function renderReposTable(repoAggs) {
  const tbody = document.getElementById('repos-tbody');
  if (!tbody) return;
  const entries = Object.entries(repoAggs).map(([rName, rData]) => {
    const calls = rData.graph_calls !== undefined ? rData.graph_calls : rData.calls;
    const errs = rData.graph_errors !== undefined ? rData.graph_errors : (rData.errors || 0);
    const rel = rData.graph_reliability_pct !== undefined
      ? rData.graph_reliability_pct
      : (rData.reliability_pct || (calls ? 100.0 * (calls - errs) / calls : 100.0));
    return [rName, { ...rData, calls, errors: errs, reliability_pct: rel }];
  }).filter(([_, rData]) => rData.calls > 0);

  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="4" class="empty-cell">No graph workspace activity recorded</td></tr>';
    return;
  }

  entries.sort((a, b) => b[1].calls - a[1].calls);
  const maxCalls = entries[0][1].calls || 1;
  const labels = entries.map(([rName, rData]) => rData.display || rName);
  const select = document.getElementById('repo-filter');
  const curRepoVal = select ? select.value : 'ALL';

  tbody.innerHTML = '';
  entries.forEach(([rName, rData]) => tbody.appendChild(buildRepoRow(rName, rData, maxCalls)));
  populateRepoSelect('repo-filter', 'All Repositories', labels, curRepoVal, true);
  populateRepoSelect('scope-filter', 'All Workspaces', labels, state.selectedRepo, false);
}

function buildAgentRowCells(aName, aData, maxCalls, totalCalls) {
  const pct = (aData.calls / maxCalls) * 100.0;
  const share = totalCalls ? (aData.calls / totalCalls * 100.0) : 0.0;
  const shareStr = (share > 0 && share < 0.1) ? '<0.1%' : share.toFixed(1) + '%';
  return '<td>' +
    '  <div class="repo-name-wrap">' + agentTagHtml(aName) + ' <span class="text-muted">(' + shareStr + ')</span></div>' +
    '  <div class="row-track"><div class="row-track-fill row-track-fill-agent" style="width: ' + pct.toFixed(1) + '%;"></div></div>' +
    '</td>' +
    '<td class="text-right num-col">' + aData.graphCalls.toLocaleString() + '</td>' +
    '<td class="text-right num-col">' + aData.reuseCalls.toLocaleString() + '</td>' +
    '<td class="text-right num-col text-muted">' + aData.reads.toLocaleString() + ' r · ' + aData.writes.toLocaleString() + ' w</td>' +
    '<td class="text-right num-col text-muted">~' + aData.saved.toLocaleString() + '</td>';
}

function renderAgentsTable(agentAggs) {
  const tbody = document.getElementById('agents-tbody');
  const select = document.getElementById('agent-filter');
  if (!tbody) return;

  const entries = Object.entries(agentAggs)
    .map(([aName, aData]) => [aName, {
      graphCalls: aData.graph_calls || 0, reuseCalls: aData.reuse_calls || 0,
      reads: aData.memory_reads || 0, writes: aData.memory_writes || 0,
      saved: aData.saved_tokens || 0, calls: aData.calls || 0,
    }])
    .filter(([_, aData]) => aData.calls > 0);

  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty-cell">No agent activity recorded</td></tr>';
    return;
  }

  entries.sort((a, b) => b[1].calls - a[1].calls);
  const maxCalls = entries[0][1].calls || 1;
  const totalCalls = entries.reduce((acc, [_, aData]) => acc + (aData.calls || 0), 0);
  const curAgentVal = select ? select.value : 'ALL';

  tbody.innerHTML = '';
  entries.forEach(([aName, aData]) => {
    const tr = document.createElement('tr');
    tr.className = 'agent-row-item';
    tr.tabIndex = 0;
    tr.setAttribute('role', 'row');
    tr.onclick = () => filterByAgent(aName);
    tr.onkeydown = (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); filterByAgent(aName); } };
    tr.innerHTML = buildAgentRowCells(aName, aData, maxCalls, totalCalls);
    tbody.appendChild(tr);
  });

  if (select) {
    select.innerHTML = '<option value="ALL">All Agents</option>';
    Object.keys(agentAggs).sort().forEach(aName => {
      const opt = document.createElement('option');
      opt.value = aName;
      opt.textContent = aName;
      select.appendChild(opt);
    });
    if (curAgentVal && Array.from(select.options).some(o => o.value === curAgentVal)) {
      select.value = curAgentVal;
    } else {
      select.value = 'ALL';
      if (curAgentVal && curAgentVal !== 'ALL') announce('Filter "' + curAgentVal + '" reset: agent no longer active');
    }
  }
}

function filterByAgent(agentName) {
  const select = document.getElementById('agent-filter');
  if (!select) return;
  select.value = agentName;
  announce('Filtered by agent ' + agentName);
  applyTelemetryFilters();
}

function renderToolsTable(toolAggs, totalCalls) {
  const tbody = document.getElementById('tools-tbody');
  if (!tbody) return;

  const entries = Object.entries(toolAggs).filter(([tName]) => GRAPH_TOOLS.includes(tName));
  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="3" class="empty-cell">No graph tool calls recorded</td></tr>';
    return;
  }

  entries.sort((a, b) => b[1].calls - a[1].calls);
  const maxCalls = entries[0][1].calls || 1;
  tbody.innerHTML = '';
  entries.forEach(([tName, tData]) => {
    const tr = document.createElement('tr');
    const calls = tData.calls;
    const pct = (calls / maxCalls) * 100.0;
    const p50Str = tData.p50 === null || tData.p50 === undefined ? '—' : tData.p50.toFixed(1) + 'ms';
    const share = totalCalls ? (calls / totalCalls * 100.0) : 0.0;
    const shareStr = (share > 0 && share < 0.1) ? '<0.1%' : share.toFixed(1) + '%';
    tr.innerHTML =
      '<td>' +
      '  <div class="tool-name-wrap"><code>' + escapeHtml(tName) + '</code> <span class="text-muted">(' + shareStr + ')</span></div>' +
      '  <div class="row-track"><div class="row-track-fill" style="width: ' + pct.toFixed(1) + '%;"></div></div>' +
      '</td>' +
      '<td class="text-right num-col">' + calls.toLocaleString() + '</td>' +
      '<td class="text-right num-col text-muted">' + p50Str + '</td>';
    tbody.appendChild(tr);
  });
}

function renderRecentQueries(queries) {
  const tbody = document.getElementById('recent-tbody');
  if (!tbody) return;

  const graphQueries = queries.filter(q => GRAPH_TOOLS.includes(q.tool));
  recentQueryTotal = graphQueries.length;
  if (graphQueries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="8" class="empty-cell">No graph navigation queries recorded</td></tr>';
    updateListCount('recent-list-count', 0, 0, 'newest first');
    return;
  }

  const visible = graphQueries.slice(0, FEED_LIMIT);
  tbody.innerHTML = '';
  visible.forEach(q => tbody.appendChild(createQueryRow(q, true)));
  updateListCount('recent-list-count', visible.length, recentQueryTotal, 'newest first');
}

const STATIC_TOOL_DESCRIPTIONS = {
  build: 'Refresh the code index',
  mem_export: 'Back up memory notes',
  mem_import: 'Load memory notes from a backup',
  stats: 'Show repository statistics',
  doctor: 'Run a health check',
  usage: 'Show usage report',
  unresolved: 'List unresolved call sites',
};

// Plain-language summary of what a command did, from its journaled tool and args.
function explainToolArgs(tool, args) {
  if (STATIC_TOOL_DESCRIPTIONS[tool]) return STATIC_TOOL_DESCRIPTIONS[tool];
  const a = args || {};
  const name = a.name || a.symbol || '';
  const terms = a.terms || '';
  const paths = a.paths || '';
  switch (tool) {
    case 'callers': return name ? 'Find who calls ' + name : 'Find who calls a function';
    case 'callees': return name ? 'Find what ' + name + ' calls' : 'Find what a function calls';
    case 'path': return a.from && a.to ? 'Route from ' + a.from + ' to ' + a.to : 'Call route between two functions';
    case 'context': return name ? 'Show the code around ' + name : 'Show code around a function';
    case 'impact': return name ? 'Show what breaks if ' + name + ' changes' : 'Show the blast radius of a change';
    case 'symbols': return a.file ? 'List symbols in ' + a.file : (name ? 'Find ' + name : 'Find symbols');
    case 'imports': return a.file ? 'Show imports of ' + a.file : 'Show the import map';
    case 'search': return terms ? 'Search the code for "' + terms + '"' : 'Search the code';
    case 'reuse': return paths ? 'Look for duplicate code in ' + paths : 'Look for duplicate code in every file';
    case 'mem_write': return a.key ? 'Save the note "' + a.key + '"' : 'Save a memory note';
    case 'mem_get': return a.key ? 'Read the note "' + a.key + '"' : 'Read a memory note';
    case 'mem_query': return terms ? 'Search notes for "' + terms + '"' : 'List memory notes';
    case 'mem_delete': return a.key ? 'Delete the note "' + a.key + '"' : 'Delete a memory note';
    case 'mem_touch': return a.key ? 'Mark the note "' + a.key + '" as verified' : 'Verify a memory note';
    default: return '';
  }
}

function applyTelemetryFilters() {
  const repoEl = document.getElementById('repo-filter');
  const selectedRepo = repoEl ? repoEl.value : 'ALL';
  const agentEl = document.getElementById('agent-filter');
  const selectedAgent = agentEl ? agentEl.value : 'ALL';
  const statusEl = document.getElementById('status-filter');
  const selectedStatus = statusEl ? statusEl.value : 'ALL';
  const searchEl = document.getElementById('query-search');
  const search = searchEl ? searchEl.value.toLowerCase().trim() : '';

  document.querySelectorAll('#recent-tbody tr.query-row').forEach(row => {
    const repo = row.getAttribute('data-repo');
    const agent = row.getAttribute('data-agent');
    const code = row.getAttribute('data-code');
    const matchesStatus = selectedStatus === 'ALL'
      || (selectedStatus === 'ERR' ? parseInt(code, 10) >= 3 : code === selectedStatus);
    const show = (selectedRepo === 'ALL' || repo === selectedRepo)
      && (selectedAgent === 'ALL' || agent === selectedAgent)
      && matchesStatus
      && (!search || row.textContent.toLowerCase().includes(search));
    row.style.display = show ? '' : 'none';
  });
}

// Shared repo scope: best-effort mirror across telemetry and reuse filters.
function syncRepoScope(value) {
  state.selectedRepo = value;
  ['scope-filter', 'repo-filter', 'reuse-repo-filter'].forEach(id => {
    const sel = document.getElementById(id);
    if (sel && Array.from(sel.options).some(o => o.value === value)) sel.value = value;
  });
  applyTelemetryFilters();
  renderReuseList();
}

function filterByRepo(repoName) {
  const select = document.getElementById('repo-filter');
  if (!select) return;
  if (repoName !== 'ALL' && !Array.from(select.options).some(o => o.value === repoName)) return;
  select.value = repoName;
  syncRepoScope(repoName);
  announce(repoName === 'ALL' ? 'Filter reset: all repositories' : 'Filtered by repository ' + repoName);
  applyTelemetryFilters();
}
