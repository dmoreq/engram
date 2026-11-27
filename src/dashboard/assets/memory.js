// ========================== Agent Memory UI ==========================

function memorySelectionId(m) { return (m.store_id || m.store) + ':' + m.key + ':' + m.rev; }

function memoryActivity(m) {
  return Math.max(m.created_at || 0, m.anchored_at || 0, m.last_recalled_ts || 0);
}

function viewMemories(items) {
  const spec = MEMORY_VIEWS[memView] || MEMORY_VIEWS.recent10;
  const sorted = items.slice().sort((a, b) => {
    if (spec.sort === 'used') {
      const used = (b.recall_count || 0) - (a.recall_count || 0);
      if (used !== 0) return used;
    }
    const activity = memoryActivity(b) - memoryActivity(a);
    if (activity !== 0) return activity;
    const recalled = (b.last_recalled_ts || 0) - (a.last_recalled_ts || 0);
    if (recalled !== 0) return recalled;
    const created = (b.created_at || 0) - (a.created_at || 0);
    if (created !== 0) return created;
    return String(a.key).localeCompare(String(b.key));
  });
  const visible = spec.limit > 0 ? sorted.slice(0, spec.limit) : sorted;
  return { items: visible, total: items.length, label: spec.label };
}

function loadMemoriesData() {
  fetch('/api/memories')
    .then(r => {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.json();
    })
    .then(data => {
      clearFetchError('memory');
      state.memories.degraded = !!data.degraded;
      state.memories.errors = data.errors || [];
      if (data.degraded) {
        showFetchError('memory', (data.errors || []).map(e => e.store + ': ' + e.message).join('; '));
      }
      state.memories.notes = data.notes || [];
      updateMemoryKpis();
      populateMemoryStoreFilter();
      renderMemoryList();
    })
    .catch(err => showFetchError('memory', err.message));
}

function summarizeMemoryKinds(currentNotes) {
  if (currentNotes.length === 0) return 'No active memory notes';
  const kinds = {};
  currentNotes.forEach(m => { const k = m.kind || 'fact'; kinds[k] = (kinds[k] || 0) + 1; });
  const parts = [];
  if (kinds.decision) parts.push(kinds.decision + ' decisions');
  if (kinds.gotcha) parts.push(kinds.gotcha + ' gotchas');
  if (kinds.procedure) parts.push(kinds.procedure + ' procedures');
  if (kinds.fact) parts.push(kinds.fact + ' facts');
  if (kinds.task) parts.push(kinds.task + ' tasks');
  return parts.length > 0 ? parts.slice(0, 3).join(' · ') : 'Stored project knowledge';
}

function updateMemoryKpis() {
  const currentNotes = state.memories.notes.filter(m => m.is_current);
  const notesWithRefs = currentNotes.filter(m => m.refs && m.refs.length > 0);
  let validCount = 0;
  let modifiedCount = 0;
  let brokenCount = 0;
  let unverifiedCount = 0;
  notesWithRefs.forEach(m => {
    if (m.ref_status === 'valid') validCount++;
    else if (m.ref_status === 'broken') brokenCount++;
    else if (m.ref_status === 'modified') modifiedCount++;
    else if (m.ref_status === 'unverified') unverifiedCount++;
  });

  // No anchored note means the ratio is unmeasurable, not 100%.
  const currencyRate = notesWithRefs.length > 0
    ? (100.0 * validCount / notesWithRefs.length).toFixed(1) + '%'
    : '—';
  const currencySubText = notesWithRefs.length === 0
    ? 'No notes with code refs'
    : validCount + ' valid · ' + modifiedCount + ' modified · ' + brokenCount + ' broken'
      + (unverifiedCount > 0 ? ' · ' + unverifiedCount + ' unverified' : '');
  setText('mem-kpi-currency', currencyRate);
  setText('mem-kpi-currency-sub', currencySubText);
  setText('ovkpi-currency', currencyRate);
  setText('ovkpi-currency-sub', currencySubText);

  setText('mem-kpi-active-count', currentNotes.length > 0 ? currentNotes.length.toLocaleString() : '—');
  setText('mem-kpi-active-sub', summarizeMemoryKinds(currentNotes));

  const totalRecalls = currentNotes.reduce((acc, m) => acc + (m.recall_count || 0), 0);
  setText('mem-kpi-recalls-count', currentNotes.length > 0 ? totalRecalls.toLocaleString() : '—');
  setText('mem-kpi-recalls-sub', currentNotes.length === 0
    ? 'No active memory notes'
    : (totalRecalls > 0 ? 'Exact-key memories leveraged by agents' : 'No recalls recorded in retained journal'));

  state.health.anchorModified = modifiedCount;
  state.health.anchorBroken = brokenCount;
  renderOverviewHealth();
  updateActionCenter(currentNotes, modifiedCount, brokenCount);
}

function buildActionItem(m) {
  const isModified = m.ref_status === 'modified';
  const badge = isModified ? '<span class="badge status-warn">[MODIFIED REF]</span>' : '<span class="badge status-err">[BROKEN REF]</span>';
  const explanation = isModified
    ? 'Target file was modified after the last verify. Review the note, then Verify to refresh its anchor.'
    : 'Referenced symbol or file path is missing from the current code index. Re-run <code>eg build</code> in that repository, or edit the note refs.';
  const touchBtn = isModified
    ? '<button class="btn-touch" data-key="' + escapeHtml(m.key) + '" data-store-id="' + escapeHtml(m.store_id || m.store) + '" data-rev="' + Number(m.rev) + '">Verify</button>'
    : '';
  const refsList = (m.refs || []).map(r => '<code>' + escapeHtml(r) + '</code>').join(', ');

  const item = document.createElement('div');
  item.className = 'action-item';
  item.innerHTML =
    '<div class="action-item-info">' +
    '  <div class="action-item-title">' + badge + ' <strong>' + escapeHtml(m.key) + '</strong>' +
    '    <span class="badge badge-repo">' + escapeHtml(m.store) + '</span>' +
    '    <span class="text-muted">r' + m.rev + '</span></div>' +
    '  <div class="action-item-desc text-muted">' + explanation + '</div>' +
    '  <div class="action-item-refs text-muted">Refs: ' + refsList + '</div>' +
    '</div>' + touchBtn;
  return item;
}

function updateActionCenter(currentNotes, modifiedCount, brokenCount) {
  const centerEl = document.getElementById('mem-action-center');
  const summaryEl = document.getElementById('action-center-summary');
  const itemsEl = document.getElementById('action-center-items');
  if (!centerEl || !itemsEl) return;

  const actionable = currentNotes.filter(m => m.ref_status === 'modified' || m.ref_status === 'broken');
  if (actionable.length === 0) {
    centerEl.classList.add('hidden');
    itemsEl.innerHTML = '';
    return;
  }

  centerEl.classList.remove('hidden');
  if (summaryEl) {
    const verifyAll = modifiedCount > 0 ? '<button class="btn-touch" id="verify-all-btn">Verify all ' + modifiedCount + '</button>' : '';
    summaryEl.innerHTML = modifiedCount + ' modified · ' + brokenCount + ' broken requiring attention' + verifyAll;
  }
  itemsEl.innerHTML = '';
  actionable.forEach(m => itemsEl.appendChild(buildActionItem(m)));
}

function touchMemory(btn) {
  const key = btn.dataset.key;
  const storeId = btn.dataset.storeId;
  const rev = parseInt(btn.dataset.rev || '0', 10);
  fetch('/api/memory/touch', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ key: key, store_id: storeId, rev: rev })
  })
    .then(r => r.json().then(body => ({ status: r.status, body: body })))
    .then(res => {
      if (res.body && res.body.ok) {
        btn.disabled = true;
        btn.textContent = 'Verified';
        loadMemoriesData();
        return;
      }
      showTouchError(btn, (res.body && res.body.error) || ('HTTP ' + res.status));
    })
    .catch(err => showTouchError(btn, 'network error: ' + err.message));
}

function showTouchError(btn, message) {
  let el = btn.parentElement ? btn.parentElement.querySelector('.touch-error') : null;
  if (!el) {
    el = document.createElement('span');
    el.className = 'touch-error text-muted';
    if (btn.parentElement) btn.parentElement.appendChild(el);
  }
  el.textContent = message;
}

function verifyAllModified() {
  const currentNotes = state.memories.notes.filter(m => m.is_current && m.ref_status === 'modified');
  if (currentNotes.length === 0) return;
  if (!confirm('Verify all ' + currentNotes.length + ' modified note(s)? This refreshes their anchor timestamps.')) return;
  let pending = currentNotes.length;
  let failed = 0;
  currentNotes.forEach(m => {
    fetch('/api/memory/touch', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key: m.key, store_id: m.store_id || m.store, rev: m.rev })
    })
      .then(r => r.json())
      .then(res => { if (!res.ok) failed++; })
      .catch(() => { failed++; })
      .finally(() => {
        pending--;
        if (pending === 0) {
          announce(failed === 0 ? 'Verified ' + currentNotes.length + ' notes' : failed + ' of ' + currentNotes.length + ' verifies failed');
          loadMemoriesData();
        }
      });
  });
}

function renderMemoryReposTable(repoAggs) {
  const tbody = document.getElementById('mem-repos-tbody');
  if (!tbody) return;
  const entries = Object.entries(repoAggs).filter(([_, rData]) => (rData.memory_calls || 0) > 0);
  if (entries.length === 0) {
    tbody.innerHTML = '<tr><td colspan="5" class="empty-cell">No memory workspace activity recorded</td></tr>';
    return;
  }

  entries.sort((a, b) => (b[1].memory_calls || 0) - (a[1].memory_calls || 0));
  const maxCalls = entries[0][1].memory_calls || 1;
  tbody.innerHTML = '';
  entries.forEach(([rName, rData]) => {
    const label = rData.display || rName;
    const calls = rData.memory_calls || 0;
    const errs = rData.memory_errors || 0;
    const reads = rData.memory_reads || 0;
    const writes = rData.memory_writes || 0;
    const rel = rData.memory_reliability_pct !== undefined
      ? rData.memory_reliability_pct
      : (calls ? 100.0 * (calls - errs) / calls : 100.0);
    const pct = (calls / maxCalls) * 100.0;
    const wtBadge = rData.worktree ? '<span class="badge badge-wt">worktree</span>' : '<span class="badge badge-repo">repo</span>';
    const hitRate = reads === 0
      ? '<span class="text-muted">—</span>'
      : (rData.memory_read_hit_rate_pct !== undefined && rData.memory_read_hit_rate_pct !== null
        ? rData.memory_read_hit_rate_pct.toFixed(1) + '%'
        : '<span class="text-muted">—</span>');

    const tr = createRepoRow(() => filterMemoryByRepo(label));
    tr.innerHTML =
      repoTrackCellHtml('<strong>' + escapeHtml(label) + '</strong>', wtBadge, pct) +
      '<td class="text-right num-col">' + calls.toLocaleString() + '</td>' +
      '<td class="text-right num-col text-muted">' + reads.toLocaleString() + ' r · ' + writes.toLocaleString() + ' w</td>' +
      '<td class="text-right num-col">' + hitRate + '</td>' +
      '<td>' + (errs === 0 ? '' : '<span class="badge status-err">' + rel.toFixed(1) + '% (' + errs + ' err)</span>') + '</td>';
    tbody.appendChild(tr);
  });
}

function filterMemoryByRepo(repoName) {
  const select = document.getElementById('mem-store-filter');
  if (!select) return;
  // Options carry value = store_id and text = short store name; rows pass
  // the display name, so match either side. Never fabricate an option.
  let matchedValue = '';
  for (let i = 0; i < select.options.length; i++) {
    const opt = select.options[i];
    if (opt.value === repoName || opt.textContent === repoName) {
      matchedValue = opt.value;
      break;
    }
  }
  setMemoryStoreFilter(matchedValue || 'ALL', matchedValue ? repoName : undefined);
}

function populateMemoryStoreFilter() {
  const select = document.getElementById('mem-store-filter');
  if (!select) return;
  const keep = select.value || 'ALL';
  const stores = new Map();
  state.memories.notes.forEach(m => {
    const id = m.store_id || m.store;
    if (!stores.has(id)) stores.set(id, m.store);
  });

  select.innerHTML = '<option value="ALL">All Stores</option>';
  Array.from(stores.keys()).sort().forEach(id => {
    const opt = document.createElement('option');
    opt.value = id;
    opt.textContent = stores.get(id);
    select.appendChild(opt);
  });
  if (Array.from(select.options).some(o => o.value === keep)) {
    select.value = keep;
  } else {
    select.value = 'ALL';
    if (keep !== 'ALL') announce('Filter "' + keep + '" reset: store no longer active');
  }
  state.memories.filterStore = select.value;

  const pillGroup = document.getElementById('mem-store-pills');
  if (!pillGroup) return;
  pillGroup.innerHTML = '';
  const appendPill = (storeId, label) => {
    const btn = document.createElement('button');
    const isAct = select.value === storeId;
    btn.className = 'filter-pill' + (isAct ? ' active' : '');
    btn.setAttribute('data-store', storeId);
    btn.setAttribute('aria-pressed', isAct ? 'true' : 'false');
    btn.textContent = label;
    pillGroup.appendChild(btn);
  };
  appendPill('ALL', 'All (' + state.memories.notes.length + ')');
  Array.from(stores.keys()).sort().forEach(id => {
    const count = state.memories.notes.filter(m => (m.store_id || m.store) === id).length;
    appendPill(id, stores.get(id) + ' (' + count + ')');
  });
}

function syncMemoryStorePills(currentStore) {
  state.memories.filterStore = currentStore;
  document.querySelectorAll('#mem-store-pills .filter-pill').forEach(pill => {
    const isAct = pill.getAttribute('data-store') === currentStore;
    pill.classList.toggle('active', isAct);
    pill.setAttribute('aria-pressed', isAct ? 'true' : 'false');
  });
}

function setMemoryStoreFilter(storeId, label) {
  const select = document.getElementById('mem-store-filter');
  const targetStore = (select && select.value === storeId && storeId !== 'ALL') ? 'ALL' : storeId;
  if (select) select.value = targetStore;
  syncMemoryStorePills(targetStore);
  state.memories.selectedId = null;
  if (targetStore === 'ALL') {
    announce('Filter reset: all stores');
  } else {
    announce('Filtered by store ' + (label || targetStore));
  }
  renderMemoryList();
}

function getFilteredMemories() {
  const storeSelect = document.getElementById('mem-store-filter');
  const store = storeSelect ? storeSelect.value : 'ALL';
  const kindSelect = document.getElementById('mem-kind-filter');
  const kind = kindSelect ? kindSelect.value : 'ALL';
  const searchEl = document.getElementById('mem-search');
  const search = searchEl ? searchEl.value.toLowerCase().trim() : '';
  const supersededEl = document.getElementById('mem-show-superseded');
  const showSuperseded = supersededEl ? supersededEl.checked : false;

  return state.memories.notes.filter(m => {
    if (!showSuperseded && !m.is_current) return false;
    if (store !== 'ALL' && (m.store_id || m.store) !== store) return false;
    if (kind !== 'ALL' && m.kind !== kind) return false;
    if (search) {
      const hit = m.key.toLowerCase().includes(search)
        || m.body.toLowerCase().includes(search)
        || (m.tags || []).some(t => t.toLowerCase().includes(search))
        || (m.refs || []).some(r => r.toLowerCase().includes(search));
      if (!hit) return false;
    }
    return true;
  });
}

function buildMemoryCard(m, isSelected) {
  const card = document.createElement('div');
  card.className = 'select-item-card' + (isSelected ? ' selected' : '');
  card.tabIndex = 0;
  card.setAttribute('role', 'button');
  card.setAttribute('aria-pressed', isSelected ? 'true' : 'false');
  card.setAttribute('aria-label', 'Memory note ' + m.key);
  const select = () => { state.memories.selectedId = memorySelectionId(m); renderMemoryList(); };
  card.onclick = select;
  card.onkeydown = (ev) => { if (ev.key === 'Enter' || ev.key === ' ') { ev.preventDefault(); select(); } };

  const kindClass = 'badge status-' + (m.kind === 'decision' ? 'ambig' : m.kind === 'gotcha' ? 'warn' : m.kind === 'fact' ? 'ok' : m.kind === 'procedure' ? 'neutral' : 'err');
  const createdStr = m.created_at ? new Date(m.created_at * 1000).toLocaleDateString() : '—';
  const lastUsedStr = m.last_recalled_ts
    ? new Date(m.last_recalled_ts * 1000).toLocaleString([], { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
    : 'never';
  const verifiedStr = m.anchored_at ? new Date(m.anchored_at * 1000).toLocaleDateString() : '';
  const refBadge = m.ref_status === 'broken'
    ? '<span class="badge status-err">[broken-ref]</span>'
    : m.ref_status === 'modified' ? '<span class="badge status-warn">[modified-ref]</span>' : '';

  card.innerHTML =
    '<div class="item-card-head">' +
    '  <div class="badge-row">' +
    '    <span class="' + kindClass + '">' + escapeHtml(m.kind.toUpperCase()) + '</span>' +
    '    <span class="badge status-neutral">r' + m.rev + '</span> ' + refBadge +
    '  </div>' +
    '  <span class="badge badge-repo">' + escapeHtml(m.store) + '</span>' +
    '</div>' +
    '<div class="item-key">' + escapeHtml(m.key) + '</div>' +
    '<div class="item-snippet">' + escapeHtml(m.body) + '</div>' +
    '<div class="item-meta">' +
    '  <span>' + escapeHtml(m.scope) + '</span><span>&bull;</span>' +
    '  <span>created ' + createdStr + '</span><span>&bull;</span>' +
    '  <span data-tip="card.recalled">recalled ' + (m.recall_count || 0) + '&times;</span><span>&bull;</span>' +
    '  <span>last used ' + lastUsedStr + '</span>' +
    (verifiedStr ? '<span>&bull;</span><span data-tip="card.verified">verified ' + verifiedStr + '</span>' : '') +
    (m.refs && m.refs.length ? '<span>&bull;</span><span>' + m.refs.length + (m.refs.length === 1 ? ' ref' : ' refs') + '</span>' : '') +
    '</div>';
  return card;
}

function renderMemoryList() {
  const container = document.getElementById('memory-cards-container');
  if (!container) return;
  const filteredAll = getFilteredMemories();
  const view = viewMemories(filteredAll);
  const filtered = view.items;
  updateListCount('mem-list-count', filtered.length, view.total, view.label);

  if (filtered.length === 0) {
    container.innerHTML = state.memories.notes.length === 0
      ? '<div class="empty-cell">No memory notes recorded yet. Agents write them with <code>eg mem write --body "..." --kind gotcha &lt;key&gt;</code></div>'
      : '<div class="empty-cell">No memory notes match the current filters.</div>';
    renderMemoryDossier(null);
    return;
  }

  let selIdx = filtered.findIndex(m => memorySelectionId(m) === state.memories.selectedId);
  if (selIdx < 0) {
    const stillVisible = filteredAll.some(m => memorySelectionId(m) === state.memories.selectedId);
    selIdx = 0;
    state.memories.selectedId = filtered[0] ? memorySelectionId(filtered[0]) : null;
    if (stillVisible) announce('Selection moved: the previous note is outside the current view');
  }
  container.innerHTML = '';
  filtered.forEach(m => container.appendChild(buildMemoryCard(m, memorySelectionId(m) === state.memories.selectedId)));
  attachTipDescriptions(container);
  renderMemoryDossier(filtered[selIdx] || null);
}

function buildMemoryRefsHtml(m) {
  if (m.resolved_refs && m.resolved_refs.length) {
    const chips = m.resolved_refs.map(r => {
      const status = r.status || (r.verified ? 'valid' : 'broken');
      const verClass = status === 'broken' ? 'chip-ref broken'
        : status === 'modified' ? 'chip-ref modified'
        : status === 'unverified' ? 'chip-ref unverified' : 'chip-ref verified';
      const verBadge = status === 'broken' ? ' [BROKEN REF]'
        : status === 'modified' ? ' [MODIFIED REF]'
        : status === 'unverified' ? ' [UNVERIFIED — NO INDEX]'
        : ' [VERIFIED ' + escapeHtml(r.type || 'file').toUpperCase() + ']';
      const locStr = r.location ? ' &middot; ' + escapeHtml(r.location) : '';
      return '<span class="' + verClass + '"><strong>' + escapeHtml(r.ref) + '</strong>' + verBadge + locStr + '</span>';
    }).join('');
    return '<div class="dossier-section-title">Code References</div><div class="chips-row">' + chips + '</div>';
  }
  if (m.refs && m.refs.length) {
    const chips = m.refs.map(r => '<span class="chip chip-ref">' + escapeHtml(r) + '</span>').join('');
    return '<div class="dossier-section-title">Referenced Symbols &amp; Paths</div><div class="chips-row">' + chips + '</div>';
  }
  return '';
}

function renderMemoryDossier(m) {
  const container = document.getElementById('memory-dossier-container');
  if (!container) return;
  container.scrollTop = 0;
  if (!m) {
    container.innerHTML = '<div class="empty-cell dossier-empty">Select a memory note from the list to view full dossier.</div>';
    return;
  }

  const kindClass = 'badge status-' + (m.kind === 'decision' ? 'ambig' : m.kind === 'gotcha' ? 'warn' : m.kind === 'fact' ? 'ok' : 'err');
  const dateStr = m.created_at ? new Date(m.created_at * 1000).toLocaleString() : '—';
  const lastRecalledStr = m.last_recalled_ts ? new Date(m.last_recalled_ts * 1000).toLocaleString() : 'Never recalled in the retained journal';
  const tagsHtml = (m.tags && m.tags.length)
    ? '<div class="dossier-section-title">Tags</div><div class="chips-row">' + m.tags.map(t => '<span class="chip chip-tag">#' + escapeHtml(t) + '</span>').join('') + '</div>'
    : '';
  const touchBtn = m.ref_status === 'modified' && m.is_current
    ? '<button class="btn-touch btn-touch-sm" data-key="' + escapeHtml(m.key) + '" data-store-id="' + escapeHtml(m.store_id || m.store) + '" data-rev="' + Number(m.rev) + '">Verify</button>'
    : '';
  const globalFlag = (m.store_id || m.store) === 'global' ? ' --global' : '';
  const refsArg = m.refs && m.refs.length ? ' --refs "' + escapeHtml(m.refs.join(' ')) + '"' : '';

  container.innerHTML =
    '<div class="dossier-head">' +
    '  <div class="dossier-head-row">' +
    '    <div class="badge-row">' +
    '      <span class="' + kindClass + '">' + escapeHtml(m.kind.toUpperCase()) + '</span>' +
    '      <span class="badge status-neutral">Rev ' + m.rev + '</span>' +
    '      <span class="badge badge-repo">' + escapeHtml(m.store) + '</span> ' + touchBtn +
    '    </div>' +
    '    <span class="dossier-sub">' + dateStr + '</span>' +
    '  </div>' +
    '  <div class="dossier-title">' + escapeHtml(m.key) + '</div>' +
    '  <div class="dossier-sub">Scope: <strong>' + escapeHtml(m.scope) + '</strong> &middot; Source: <code>' + escapeHtml(m.source) + '</code></div>' +
    '</div>' +
    '<div class="recall-banner"><div class="cli-lines"><span>Recalls: <strong>' + m.recall_count + '</strong> &middot; Last: ' + lastRecalledStr + '</span></div></div>' +
    '<div class="dossier-section-title">Memory Content</div>' +
    '<div class="dossier-body">' + escapeHtml(m.body) + '</div>' +
    buildMemoryRefsHtml(m) + tagsHtml +
    '<div class="action-recommendation">' +
    '  <div class="action-item-title">Agent Workflow &amp; CLI Invocations</div>' +
    '  <div class="cli-lines">' +
    '    <div><span class="text-muted">Read: </span><code>eg mem get "' + escapeHtml(m.key) + '"' + globalFlag + '</code></div>' +
    '    <div><span class="text-muted">Save: </span><code>eg mem write "' + escapeHtml(m.key) + '" --kind ' + escapeHtml(m.kind) + refsArg + globalFlag + ' --body "..."</code></div>' +
    '  </div>' +
    '</div>';
}
