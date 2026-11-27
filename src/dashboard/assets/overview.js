// ========================= Mission Control Surfaces =========================

function renderOverviewKpis(data, totalCalls, totalErrors) {
  const tokens = data.graph_saved_tokens !== undefined ? data.graph_saved_tokens : (data.saved_tokens || 0);
  const dollars = data.graph_saved_dollars !== undefined ? data.graph_saved_dollars : (data.saved_dollars || 0.0);
  setText('ovkpi-queries', totalCalls.toLocaleString());
  setText('ovkpi-queries-sub', reliabilityText(totalCalls, totalErrors, data));
  setText('ovkpi-tokens', '~' + tokens.toLocaleString());
  setText('ovkpi-dollars', '~$' + dollars.toFixed(2) + ' avoided vs full reads');
}

// Top repositories across all pillars: the five heaviest graph workspaces.
function renderOverviewRepos(repoAggs) {
  const tbody = document.getElementById('ov-repos-tbody');
  if (!tbody) return;

  const entries = Object.entries(repoAggs)
    .map(([rName, rData]) => [rName, {
      ...rData,
      calls: rData.graph_calls !== undefined ? rData.graph_calls : rData.calls,
      errors: rData.graph_errors !== undefined ? rData.graph_errors : (rData.errors || 0),
    }])
    .filter(([_, rData]) => rData.calls > 0)
    .sort((a, b) => b[1].calls - a[1].calls)
    .slice(0, OVERVIEW_TOP_N);

  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="3" class="empty-cell">No graph workspace activity recorded</td></tr>';
    return;
  }

  const maxCalls = entries[0][1].calls || 1;
  tbody.innerHTML = '';
  entries.forEach(([rName, rData]) => {
    const label = rData.display || rName;
    const errs = rData.errors || 0;
    const rel = rData.graph_reliability_pct !== undefined
      ? rData.graph_reliability_pct
      : (rData.reliability_pct || (rData.calls ? 100.0 * (rData.calls - errs) / rData.calls : 100.0));
    const pct = (rData.calls / maxCalls) * 100.0;
    const wtBadge = rData.worktree ? '<span class="badge badge-wt">worktree</span>' : '';
    const statusCell = errs === 0
      ? ''
      : '<span class="badge status-err">' + rel.toFixed(1) + '% (' + errs + ' err)</span>';

    const tr = document.createElement('tr');
    tr.innerHTML =
      repoTrackCellHtml('<strong>' + escapeHtml(label) + '</strong>', wtBadge, pct) +
      '<td class="text-right num-col">' + rData.calls.toLocaleString() + '</td>' +
      '<td>' + statusCell + '</td>';
    tbody.appendChild(tr);
  });
}

// Top agents across all pillars: the five most-active identities.
function renderOverviewAgents(agentAggs) {
  const tbody = document.getElementById('ov-agents-tbody');
  if (!tbody) return;

  const entries = Object.entries(agentAggs)
    .map(([aName, aData]) => [aName, { calls: aData.calls || 0, saved: aData.saved_tokens || 0 }])
    .filter(([_, aData]) => aData.calls > 0)
    .sort((a, b) => b[1].calls - a[1].calls)
    .slice(0, OVERVIEW_TOP_N);

  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="3" class="empty-cell">No agent activity recorded</td></tr>';
    return;
  }

  const total = entries.reduce((acc, [_, aData]) => acc + (aData.calls || 0), 0);
  tbody.innerHTML = '';
  entries.forEach(([aName, aData]) => {
    const share = total ? (aData.calls / total * 100.0) : 0.0;
    const shareStr = (share > 0 && share < 0.1) ? '<0.1%' : share.toFixed(1) + '%';
    const tr = document.createElement('tr');
    tr.innerHTML =
      '<td><div class="repo-name-wrap">' + agentTagHtml(aName) +
      ' <span class="text-muted">(' + shareStr + ')</span></div></td>' +
      '<td class="text-right num-col">' + aData.calls.toLocaleString() + '</td>' +
      '<td class="text-right num-col text-muted">~' + aData.saved.toLocaleString() + '</td>';
    tbody.appendChild(tr);
  });
}

function renderOverviewActivity() {
  const tbody = document.getElementById('ov-recent-tbody');
  if (!tbody) return;
  const rows = state.recentQueries;
  if (rows.length === 0) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty-cell">No queries recorded</td></tr>';
    return;
  }
  tbody.innerHTML = '';
  rows.forEach(q => tbody.appendChild(createQueryRow(q, false)));
}

// System health: the alert rows only. A healthy system renders no banner —
// quiet until an anomaly occurs.
function renderOverviewHealth() {
  const el = document.getElementById('ov-health');
  if (!el) return;
  const h = state.health;
  const rows = [];

  if (h.anchorBroken > 0 || h.anchorModified > 0) {
    rows.push(
      '<div class="health-row">' +
      (h.anchorBroken > 0 ? '<span class="badge status-err">' + h.anchorBroken + ' broken</span>' : '') +
      (h.anchorModified > 0 ? '<span class="badge status-warn">' + h.anchorModified + ' modified</span>' : '') +
      '<span class="health-text">memory anchors need attention</span>' +
      '<button class="btn-touch" data-health-action="memory">Review</button>' +
      '</div>'
    );
  }
  if (h.reuseDegraded) {
    rows.push(
      '<div class="health-row">' +
      '<span class="badge status-warn">DEGRADED</span>' +
      '<span class="health-text">reuse scan: ' + escapeHtml(h.reuseError) + '</span>' +
      '</div>'
    );
  }
  if (h.graphErrors > 0) {
    rows.push(
      '<div class="health-row">' +
      '<span class="badge status-err">' + h.graphErrors + ' errors</span>' +
      '<span class="health-text">graph invocations exited non-zero in the retained window</span>' +
      '</div>'
    );
  }

  if (rows.length === 0) {
    el.classList.add('hidden');
    el.innerHTML = '';
    return;
  }
  el.classList.remove('hidden');
  el.innerHTML = rows.join('');
}
