// ========================= System Health Banner =========================

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
