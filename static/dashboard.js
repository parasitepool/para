let wsReconnectTimeout;

const CONFIG = {
  MAX_LOGS: 500,
  LOGS_TO_SHOW: 100,
  WS_RECONNECT_DELAY: 2000,
  COPY_FEEDBACK_DURATION: 1000,
  DEFAULT_POLL_INTERVAL: 5000,
  STALE_AFTER_MS: 15000,
  FILTER_DEBOUNCE_MS: 250
};

function withAuthOptions(options = {}) {
  return { ...options, credentials: options.credentials || 'same-origin' };
}

function escapeHtml(value) {
  return String(value ?? '').replace(/[&<>"']/g, ch => ({
    '&': '&amp;',
    '<': '&lt;',
    '>': '&gt;',
    '"': '&quot;',
    "'": '&#39;',
  })[ch]);
}


function redirectToLogin() {
  location.href = `/login?next=${encodeURIComponent(location.pathname + location.search)}`;
}

async function initAuthProbe() {
  try {
    const res = await fetch('/api/router/status', withAuthOptions());
    if (res.status === 401) {
      redirectToLogin();
      return false;
    }
  } catch (_) {
    return true;
  }

  for (const id of ['halt-btn', 'boost-btn', 'capacity-btn']) {
    document.getElementById(id)?.classList.remove('hidden');
  }

  return true;
}

async function logout() {
  try {
    await fetch('/logout', { method: 'POST', credentials: 'same-origin' });
  } catch (_) {}
  location.reload();
}

function initNavbar() {
  for (const item of document.querySelectorAll('.navbar-item')) {
    if (item.getAttribute('href') === location.pathname) {
      item.setAttribute('aria-current', 'page');
    }
  }

  document.getElementById('navbar-logout')?.addEventListener('click', logout);
}

document.addEventListener('DOMContentLoaded', initNavbar);

const logState = {
  paused: false,
  allLogs: [],
  pausedLogs: [],
  ws: null,
};

function createLogLine(level, text) {
  const line = document.createElement('div');
  const lvl = document.createElement('span');
  lvl.className = 'log-' + level.trim().toLowerCase();
  lvl.textContent = level;
  line.appendChild(lvl);
  line.appendChild(document.createTextNode(' ' + text));
  line.dataset.text = (level + ' ' + text).toLowerCase();
  return line;
}

function renderLogs() {
  const logsEl = document.getElementById('logs');
  const filterInput = document.getElementById('log-filter');
  if (!logsEl) return;

  const filter = filterInput?.value?.toLowerCase() || '';
  const source = logState.paused ? logState.pausedLogs : logState.allLogs;

  const fragment = document.createDocumentFragment();
  let count = 0;
  for (let i = source.length - 1; i >= 0 && count < CONFIG.LOGS_TO_SHOW; i--) {
    const log = source[i];
    if (filter && !(log.level + ' ' + log.text).toLowerCase().includes(filter)) continue;
    fragment.prepend(createLogLine(log.level, log.text));
    count++;
  }

  logsEl.innerHTML = '';
  logsEl.appendChild(fragment);

  if (!logState.paused) {
    logsEl.scrollTop = logsEl.scrollHeight;
  }
}

function connectWs() {
  const logsEl = document.getElementById('logs');
  if (!logsEl) return;

  if (logState.ws) {
    logState.ws.onclose = null;
    logState.ws.onerror = null;
    logState.ws.onmessage = null;
    logState.ws.close();
  }

  const levelToggle = document.getElementById('log-level-toggle');
  const levelOptions = document.getElementById('log-level-options');

  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(`${protocol}//${location.host}/ws/logs`);
  let opened = false;
  logState.ws = ws;

  ws.onopen = () => {
    opened = true;
  };

  ws.onmessage = (e) => {
    const [level, ...rest] = e.data.split('\t');
    const text = rest.join('\t');
    if (level === 'level') {
      if (levelToggle) levelToggle.textContent = text.toUpperCase();
      if (levelOptions) levelOptions.classList.add('hidden');
      return;
    }
    logState.allLogs.push({ level, text });
    if (logState.allLogs.length > CONFIG.MAX_LOGS) {
      logState.allLogs.splice(0, logState.allLogs.length - CONFIG.MAX_LOGS);
    }
    if (!logState.paused) renderLogs();
  };
  ws.onerror = () => ws.close();
  ws.onclose = () => {
    if (logState.ws !== ws) return;
    if (!opened) {
      logState.ws = null;
      return;
    }
    clearTimeout(wsReconnectTimeout);
    wsReconnectTimeout = setTimeout(connectWs, CONFIG.WS_RECONNECT_DELAY);
  };

  document.querySelectorAll('.log-level').forEach(btn => {
    btn.onclick = () => {
      if (logState.ws?.readyState === WebSocket.OPEN) {
        logState.ws.send('set-level:' + btn.dataset.level);
      }
    };
  });
}

function initLogControls() {
  const pauseBtn = document.getElementById('log-pause');
  const filterInput = document.getElementById('log-filter');
  const clearBtn = document.getElementById('log-clear');
  const levelToggle = document.getElementById('log-level-toggle');
  const levelOptions = document.getElementById('log-level-options');
  if (!pauseBtn) return;

  pauseBtn.addEventListener('click', () => {
    logState.paused = !logState.paused;
    pauseBtn.textContent = logState.paused ? 'Resume' : 'Pause';
    pauseBtn.className = logState.paused ? 'paused' : '';
    if (logState.paused) {
      logState.pausedLogs = [...logState.allLogs];
    } else {
      renderLogs();
    }
  });

  filterInput?.addEventListener('input', renderLogs);

  clearBtn?.addEventListener('click', () => {
    logState.allLogs = [];
    logState.pausedLogs = [];
    if (filterInput) filterInput.value = '';
    renderLogs();
  });

  levelToggle?.addEventListener('click', () => {
    levelOptions?.classList.toggle('hidden');
  });

  document.addEventListener('click', (e) => {
    if (levelOptions && !levelOptions.classList.contains('hidden')
        && !e.target.closest('#log-level-controls')) {
      levelOptions.classList.add('hidden');
    }
  });
}

const SI_PREFIXES = ['', 'K', 'M', 'G', 'T', 'P', 'E', 'Z', 'Y'];

function formatDifficulty(d) {
  if (d === null || d === undefined) return null;
  if (d < 1) {
    return d.toFixed(8).replace(/\.?0+$/, '');
  }
  const i = Math.min(Math.floor(Math.log10(d) / 3), SI_PREFIXES.length - 1);
  const scaled = d / Math.pow(10, i * 3);
  const truncated = Math.floor(scaled * 100) / 100;
  return truncated.toFixed(2) + SI_PREFIXES[i];
}

function formatSi(v, unit) {
  if (v === null || v === undefined) return null;
  if (v < 1) return '0 ' + unit;
  const i = Math.max(0, Math.min(Math.floor(Math.log10(v) / 3), SI_PREFIXES.length - 1));
  const scaled = v / Math.pow(1000, i);
  const truncated = Math.floor(scaled * 100) / 100;
  return truncated.toFixed(2) + ' ' + SI_PREFIXES[i] + unit;
}

function formatHashrate(h) { return formatSi(h, 'H/s'); }
function formatHashDays(v) { return formatSi(v, 'Hd'); }

function formatTimestamp(ts) {
  if (ts === null || ts === undefined) return '-';
  return new Date(ts * 1000).toLocaleString();
}

function formatAmount(value) {
  if (value === null || value === undefined) return '-';
  return typeof value === 'number' ? `${value} sats` : String(value);
}

function parseSi(s, units) {
  s = s.trim();
  for (const u of units) {
    if (s.endsWith(u)) s = s.slice(0, -u.length).trim();
  }
  for (let i = SI_PREFIXES.length - 1; i > 0; i--) {
    const p = SI_PREFIXES[i];
    if (s.endsWith(p) || s.endsWith(p.toLowerCase())) {
      const num = parseFloat(s.slice(0, -p.length));
      if (isNaN(num) || num < 0) return null;
      return num * Math.pow(1000, i);
    }
  }
  const num = parseFloat(s);
  if (isNaN(num) || num < 0) return null;
  return num;
}

function parseHashDays(s) { return parseSi(s, ['Hd']); }

function formatTruncated(n) {
  if (n === null || n === undefined) return '-';
  if (Math.abs(n) < 0.005) return '0.00';
  const truncated = Math.floor(n * 100) / 100;
  return truncated.toLocaleString(undefined, {minimumFractionDigits: 2, maximumFractionDigits: 2});
}

function rejectionPct(accepted, rejected) {
  const total = (accepted || 0) + (rejected || 0);
  return total > 0 ? formatTruncated((rejected / total) * 100) + '%' : '0.00%';
}

function rejectionDetail(accepted, rejected) {
  return `${rejected || 0} / ${(accepted || 0) + (rejected || 0)}`;
}

function rejectionWorkDetail(accepted, rejected) {
  return `${formatDifficulty(rejected || 0)} / ${formatDifficulty((accepted || 0) + (rejected || 0))}`;
}

function truncateMiddle(s, maxLen = 15, edgeLen = 6) {
  return s && s.length > maxLen ? s.slice(0, edgeLen) + '...' + s.slice(-edgeLen) : s;
}

function formatPing(ms) {
  if (ms === null || ms === undefined) return null;
  return String(ms);
}

function formatDuration(secs) {
  if (!Number.isFinite(secs) || secs < 0) return null;
  const units = [
    ['y', 31536000],
    ['d', 86400],
    ['h', 3600],
    ['m', 60],
  ];
  for (const [suffix, size] of units) {
    if (secs >= size) {
      return (secs / size).toFixed(1) + suffix;
    }
  }
  return secs.toFixed(1) + 's';
}

function formatStatus(status) {
  if (status === null || status === undefined) return '-';
  return status
    .split('_')
    .map(part => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ');
}

function statusClass(status) {
  if (status === null || status === undefined) return '';
  return status === 'active' ? 'connected' : status;
}

function set(id, value, formatter = v => v) {
  const el = document.getElementById(id);
  if (el && !el.matches(':hover')) {
    el.textContent = (value !== null && value !== undefined) ? formatter(value) : '-';
  }
  return el;
}

function setClass(id, className) {
  const el = document.getElementById(id);
  if (el) el.className = className;
}

function copyable(id, formatted, raw) {
  const el = set(id, formatted);
  if (el) {
    if (raw === null || raw === undefined) {
      delete el.dataset.full;
    } else {
      el.dataset.full = String(raw);
    }
    el.dataset.formatted =
      (formatted !== null && formatted !== undefined) ? String(formatted) : '-';
  }
  return el;
}

function rejectionCopyable(id, accepted, rejected) {
  copyable(id, rejectionPct(accepted, rejected), rejectionDetail(accepted, rejected));
}

function rejectionWorkCopyable(id, accepted, rejected) {
  copyable(id, rejectionPct(accepted, rejected), rejectionWorkDetail(accepted, rejected));
}

function initCopyables() {
  document.querySelectorAll('.copyable').forEach(el => {
    el.addEventListener('click', async () => {
      const full = el.dataset.full;
      if (!full) return;
      try {
        await navigator.clipboard.writeText(full);
        const formatted = el.dataset.formatted || el.textContent;
        el.textContent = 'Copied!';
        el.classList.add('copied');
        setTimeout(() => {
          el.textContent = formatted;
          el.classList.remove('copied');
        }, CONFIG.COPY_FEEDBACK_DURATION);
      } catch (e) { console.error('Copy failed:', e); }
    });
  });
}

function initDelegatedCopyables(containerId, selector = '.copyable') {
  const container = document.getElementById(containerId);
  if (!container) return;
  container.addEventListener('click', async (e) => {
    const el = e.target.closest(selector);
    if (!el) return;
    const full = el.dataset.full;
    if (!full) return;
    try {
      await navigator.clipboard.writeText(full);
      const formatted = el.dataset.formatted || el.textContent;
      el.textContent = 'Copied!';
      el.classList.add('copied');
      setTimeout(() => {
        el.textContent = formatted;
        el.classList.remove('copied');
      }, CONFIG.COPY_FEEDBACK_DURATION);
    } catch (e) { console.error('Copy failed:', e); }
  });
}

function setupLogToggle() {
  const showBtn = document.getElementById('log-show');
  const hideBtn = document.getElementById('log-hide');
  const controls = document.getElementById('log-controls');
  const logs = document.getElementById('logs');
  if (!showBtn || !hideBtn || !controls || !logs) return;

  showBtn.addEventListener('click', () => {
    showBtn.classList.add('hidden');
    controls.classList.remove('hidden');
    logs.classList.remove('hidden');
  });

  hideBtn.addEventListener('click', () => {
    showBtn.classList.remove('hidden');
    controls.classList.add('hidden');
    logs.classList.add('hidden');
  });
}

function renderBitcoinData(data) {
  if (!data) {
    set('btc_height', null);
    const link = document.getElementById('btc_height_link');
    if (link) link.removeAttribute('href');
    copyable('network_difficulty', '-', null);
    set('network_hashrate', null);
    set('mempool_txs', null);
    return;
  }

  set('btc_height', data.height);
  const link = document.getElementById('btc_height_link');
  if (link && data.height != null) link.href = `https://mempool.space/block/${data.height}`;
  copyable('network_difficulty', formatDifficulty(data.network_difficulty), data.network_difficulty);
  set('network_hashrate', data.network_hashrate, formatHashrate);
  set('mempool_txs', data.mempool_txs);
}

function renderSystemData(data) {
  if (!data) {
    set('cpu_usage_percent', null);
    set('memory_usage_percent', null);
    set('disk_usage_percent', null);
    set('uptime', null);
    return;
  }

  set('cpu_usage_percent', data.cpu_usage_percent, formatTruncated);
  set('memory_usage_percent', data.memory_usage_percent, formatTruncated);
  set('disk_usage_percent', data.disk_usage_percent, formatTruncated);
  set('uptime', data.uptime, formatDuration);
}

function populateStats(prefix, stats) {
  if (!stats) {
    for (const id of ['hashrate_1m', 'sps_1m', 'best_share', 'last_share',
                      'rejected_shares', 'rejected_work', 'delivered_work', 'delivered_hash_days']) {
      set(`${prefix}_${id}`, null);
    }
    return;
  }
  set(`${prefix}_hashrate_1m`, stats.hashrate_1m, formatHashrate);
  set(`${prefix}_sps_1m`, stats.sps_1m, formatTruncated);
  copyable(`${prefix}_best_share`, formatDifficulty(stats.best_share), stats.best_share);
  set(`${prefix}_last_share`, stats.last_share, formatTimestampAgo);
  rejectionCopyable(`${prefix}_rejected_shares`, stats.accepted_shares, stats.rejected_shares);
  rejectionWorkCopyable(`${prefix}_rejected_work`, stats.accepted_work, stats.rejected_work);
  set(`${prefix}_delivered_work`, (stats.accepted_work || 0) + (stats.rejected_work || 0), formatDifficulty);
  copyable(`${prefix}_delivered_hash_days`, formatHashDays(stats.delivered_hash_days), stats.delivered_hash_days);
}

function populateDownstream(downstream) {
  set('users', downstream.user_count);
  set('workers', downstream.worker_count);
  set('sessions', downstream.session_count);
  set('idle', downstream.idle_count);
  set('disconnected', downstream.disconnected_count);
  populateStats('downstream', downstream.stats);
}

function populateUpstream(upstream) {
  if (!upstream) {
    for (const id of ['users', 'workers', 'idle', 'disconnected']) {
      set(`upstream_${id}`, null);
    }
    populateStats('upstream', null);
    return;
  }
  set('upstream_users', upstream.user_count);
  set('upstream_workers', upstream.worker_count);
  set('upstream_idle', upstream.idle_count);
  set('upstream_disconnected', upstream.disconnected_count);
  populateStats('upstream', upstream.stats);
}

function renderSessionRow(session) {
  const stats = session.stats;
  const sessionUser = session.username || '';
  const shortSessionUser = truncateMiddle(sessionUser);
  const lastShare = stats.last_share != null ? formatTimestampAgo(stats.last_share) : '-';
  const bestShare = formatDifficulty(stats.best_share);
  const safeSessionUser = escapeHtml(sessionUser);
  const safeShortSessionUser = escapeHtml(shortSessionUser);
  return `<tr>
    <td><span class=copyable data-full="${safeSessionUser}" data-formatted="${safeShortSessionUser}">${safeShortSessionUser}</span></td>
    <td>${escapeHtml(formatHashrate(stats.hashrate_1m))}</td>
    <td>${escapeHtml(formatTruncated(stats.sps_1m))}</td>
    <td>${escapeHtml(bestShare || '-')}</td>
    <td>${escapeHtml(stats.delivered_hash_days != null ? formatHashDays(stats.delivered_hash_days) : '-')}</td>
    <td>${escapeHtml(lastShare)}</td>
  </tr>`;
}

function compareStat(key) {
  return (a, b) => compareNumber(a.stats[key], b.stats[key]);
}

function compareNullableStat(key) {
  return (a, b) => compareNullableNumber(a.stats[key], b.stats[key]);
}

function initSessionsTable(root) {
  return initFilterTable({
    root,
    prefix: 'sessions',
    defaultSort: { column: 'hashrate_1m', direction: 'desc' },
    sorts: {
      username: 'string',
      hashrate_1m: compareStat('hashrate_1m'),
      sps_1m: compareStat('sps_1m'),
      best_share: compareNullableStat('best_share'),
      delivered_hash_days: compareStat('delivered_hash_days'),
      last_share: compareNullableStat('last_share'),
    },
    tiebreak: (a, b) => compareString(a.username, b.username),
    renderRow: renderSessionRow,
  });
}

function compareNumber(a, b) {
  return (Number(a) || 0) - (Number(b) || 0);
}

function compareString(a, b) {
  return String(a ?? '').toLowerCase().localeCompare(String(b ?? '').toLowerCase());
}

function compareNullableNumber(a, b) {
  const aNull = a == null;
  const bNull = b == null;
  if (aNull && bNull) return 0;
  if (aNull) return 1;
  if (bNull) return -1;
  return compareNumber(a, b);
}

function initFilterTable({
  root,
  prefix = '',
  defaultSort,
  sorts = {},
  ascByDefault = [],
  tiebreak,
  renderRow,
  onFilterChange,
  pageSize = 25,
}) {
  const tbody = root.querySelector('tbody');
  const headers = Array.from(root.querySelectorAll('th[data-sort]'));
  const validSorts = new Set(headers.map(th => th.dataset.sort));
  const searchInput = root.querySelector('input[type=search][data-filter]');
  const multiSelects = Array.from(root.querySelectorAll('.multi-select[data-filter]'));
  const selects = Array.from(root.querySelectorAll('select[data-filter]'));
  const resetButton = root.querySelector('.table-reset');
  const prevButton = root.querySelector('.page-prev');
  const nextButton = root.querySelector('.page-next');
  const pageStatus = root.querySelector('.page-status');

  let sortColumn = defaultSort.column;
  let sortDirection = defaultSort.direction;
  let currentPage = 0;
  let rows = [];
  let pageRows = [];
  let totalCount = 0;
  let filterTimer = null;

  function checkboxes(multi) {
    return Array.from(multi.querySelectorAll('input[type=checkbox]'));
  }

  function updateMultiLabel(multi) {
    const label = multi.querySelector('.multi-select-label');
    if (!label) return;

    const checked = checkboxes(multi).filter(input => input.checked);
    if (checked.length === 0) {
      label.textContent = 'Any';
    } else if (checked.length === 1) {
      label.textContent = checked[0].parentElement.textContent.trim();
    } else {
      label.textContent = `${checked.length} selected`;
    }
  }

  function toggleMultiSelect(multi) {
    const options = multi.querySelector('.multi-select-options');
    const toggle = multi.querySelector('.multi-select-toggle');
    if (!options || !toggle) return;

    const hidden = options.classList.toggle('hidden');
    toggle.setAttribute('aria-expanded', String(!hidden));
  }

  function closeMultiSelects() {
    for (const multi of multiSelects) {
      multi.querySelector('.multi-select-options')?.classList.add('hidden');
      multi.querySelector('.multi-select-toggle')?.setAttribute('aria-expanded', 'false');
    }
  }

  function compareRows(a, b) {
    const sort = sorts[sortColumn];
    let value;
    if (typeof sort === 'function') {
      value = sort(a, b);
    } else if (sort === 'string') {
      value = compareString(a[sortColumn], b[sortColumn]);
    } else {
      value = compareNumber(a[sortColumn], b[sortColumn]);
    }

    value = sortDirection === 'asc' ? value : -value;
    return value || tiebreak(a, b);
  }

  function addFilterParams(params) {
    if (searchInput) {
      const search = searchInput.value?.trim() || '';
      if (search) params.set(searchInput.dataset.filter, search);
    }
    for (const multi of multiSelects) {
      for (const input of checkboxes(multi)) {
        if (input.checked) params.append(multi.dataset.filter, input.value);
      }
    }
    for (const select of selects) {
      if (select.value) params.set(select.dataset.filter, select.value);
    }
  }

  function stateParam(name) {
    return prefix ? `${prefix}-${name}` : name;
  }

  function addStateParams(params, includePage) {
    addFilterParams(params);
    if (sortColumn !== defaultSort.column || sortDirection !== defaultSort.direction) {
      params.set(stateParam('sort'), sortColumn);
      params.set(stateParam('direction'), sortDirection);
    }
    if (includePage && currentPage > 0) {
      params.set(stateParam('page'), String(currentPage + 1));
    }
  }

  function buildQuery() {
    const params = new URLSearchParams();
    addFilterParams(params);

    if (Array.from(params.keys()).length === 0) {
      params.set('limit', String(pageSize));
    }

    return params.toString();
  }

  function syncUrl() {
    const params = new URLSearchParams(location.search);
    if (searchInput) params.delete(searchInput.dataset.filter);
    for (const multi of multiSelects) params.delete(multi.dataset.filter);
    for (const select of selects) params.delete(select.dataset.filter);
    for (const name of ['sort', 'direction', 'page']) params.delete(stateParam(name));
    addStateParams(params, true);
    const query = params.toString();
    history.replaceState(null, '', query ? `${location.pathname}?${query}` : location.pathname);
  }

  function loadStateFromUrl() {
    const params = new URLSearchParams(location.search);

    if (searchInput) searchInput.value = params.get(searchInput.dataset.filter) || '';
    for (const multi of multiSelects) {
      const values = new Set(params.getAll(multi.dataset.filter).flatMap(value => value.split(',')));
      for (const input of checkboxes(multi)) input.checked = values.has(input.value);
      updateMultiLabel(multi);
    }
    for (const select of selects) select.value = params.get(select.dataset.filter) || '';

    const requestedSort = params.get(stateParam('sort'));
    sortColumn = validSorts.has(requestedSort) ? requestedSort : defaultSort.column;
    const requestedDirection = params.get(stateParam('direction'));
    sortDirection = requestedDirection === 'asc' || requestedDirection === 'desc'
      ? requestedDirection
      : defaultSort.direction;

    const requestedPage = Number(params.get(stateParam('page')));
    currentPage = Number.isInteger(requestedPage) && requestedPage > 1 ? requestedPage - 1 : 0;
  }

  function applyFilterChange() {
    clearTimeout(filterTimer);
    filterTimer = null;
    syncUrl();
    onFilterChange?.();
  }

  function scheduleFilterChange() {
    currentPage = 0;
    clearTimeout(filterTimer);
    filterTimer = setTimeout(applyFilterChange, CONFIG.FILTER_DEBOUNCE_MS);
  }

  function resetFilters() {
    if (searchInput) searchInput.value = '';
    for (const multi of multiSelects) {
      for (const input of checkboxes(multi)) input.checked = false;
      updateMultiLabel(multi);
    }
    for (const select of selects) select.value = '';
    closeMultiSelects();

    sortColumn = defaultSort.column;
    sortDirection = defaultSort.direction;
    currentPage = 0;
    applyFilterChange();
  }

  function updateSortHeaders() {
    for (const th of headers) {
      const active = th.dataset.sort === sortColumn;
      th.classList.toggle('sort-asc', active && sortDirection === 'asc');
      th.classList.toggle('sort-desc', active && sortDirection === 'desc');
      th.setAttribute('aria-sort', active ? (sortDirection === 'asc' ? 'ascending' : 'descending') : 'none');
    }
  }

  function render() {
    if (!tbody || tbody.querySelector('.copyable:hover')) return;

    const sorted = rows.slice().sort(compareRows);
    totalCount = sorted.length;
    const pageCount = Math.max(1, Math.ceil(sorted.length / pageSize));
    const previousPage = currentPage;
    currentPage = Math.max(0, Math.min(currentPage, pageCount - 1));
    if (currentPage !== previousPage) syncUrl();
    const pageStart = currentPage * pageSize;
    pageRows = sorted.slice(pageStart, pageStart + pageSize);

    tbody.innerHTML = pageRows.map(renderRow).join('');

    if (pageStatus) {
      pageStatus.textContent = totalCount === 0
        ? '0/0'
        : `${currentPage + 1}/${pageCount} (${totalCount})`;
    }
    if (prevButton) prevButton.disabled = currentPage === 0;
    if (nextButton) nextButton.disabled = currentPage >= pageCount - 1;
    updateSortHeaders();
  }

  function setRows(newRows) {
    rows = newRows;
    render();
  }

  searchInput?.addEventListener('input', scheduleFilterChange);

  for (const multi of multiSelects) {
    multi.querySelector('.multi-select-toggle')?.addEventListener('click', () => toggleMultiSelect(multi));
    for (const input of checkboxes(multi)) {
      input.addEventListener('change', () => {
        updateMultiLabel(multi);
        currentPage = 0;
        applyFilterChange();
      });
    }
  }

  for (const select of selects) {
    select.addEventListener('change', () => {
      currentPage = 0;
      applyFilterChange();
    });
  }

  resetButton?.addEventListener('click', resetFilters);

  if (multiSelects.length > 0) {
    document.addEventListener('click', e => {
      const control = e.target.closest('.multi-select');
      if (control && root.contains(control)) return;
      closeMultiSelects();
    });
  }

  for (const th of headers) {
    th.addEventListener('click', () => {
      const column = th.dataset.sort;
      if (sortColumn === column) {
        sortDirection = sortDirection === 'asc' ? 'desc' : 'asc';
      } else {
        sortColumn = column;
        sortDirection = ascByDefault.includes(column) ? 'asc' : 'desc';
      }
      currentPage = 0;
      syncUrl();
      render();
    });
  }

  prevButton?.addEventListener('click', () => {
    if (currentPage > 0) {
      currentPage -= 1;
      syncUrl();
      render();
    }
  });

  nextButton?.addEventListener('click', () => {
    if ((currentPage + 1) * pageSize < totalCount) {
      currentPage += 1;
      syncUrl();
      render();
    }
  });

  loadStateFromUrl();
  syncUrl();
  updateSortHeaders();

  return { buildQuery, setRows, render };
}

async function fetchJson(url, options) {
  const r = await fetch(url, withAuthOptions(options));
  if (r.status === 401) {
    redirectToLogin();
    throw new Error(`${url}: ${r.status}`);
  }
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return r.json();
}

async function fetchJsonAllow404(url, options) {
  const r = await fetch(url, withAuthOptions(options));
  if (r.status === 404) return null;
  if (r.status === 401) {
    redirectToLogin();
    throw new Error(`${url}: ${r.status}`);
  }
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return r.json();
}

async function fetchJsonIfAllowed(url, options) {
  const r = await fetch(url, withAuthOptions(options));
  if (r.status === 401) return null;
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return r.json();
}

let lastUpdate = null;
let tickerInterval;
let lastTickerText = null;
let lastTickerStale = null;

function formatAgo(seconds) {
  if (seconds < 1) return 'just now';
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  return `${Math.floor(seconds / 3600)}h ago`;
}

function formatTimestampAgo(ts) {
  return formatAgo(Math.floor(Date.now() / 1000) - ts);
}

function renderTicker() {
  const el = document.getElementById('last-updated');
  if (!el) return;
  const elapsed = lastUpdate === null ? null : Date.now() - lastUpdate;
  const text = elapsed === null ? 'Updating...' : `Updated ${formatAgo(Math.floor(elapsed / 1000))}`;
  const stale = elapsed !== null && elapsed > CONFIG.STALE_AFTER_MS;
  if (text === lastTickerText && stale === lastTickerStale) return;
  lastTickerText = text;
  lastTickerStale = stale;
  el.textContent = text;
  el.classList.toggle('stale', stale);
}

function markUpdated() {
  lastUpdate = Date.now();
  renderTicker();
}

function ensureTicker() {
  const el = document.getElementById('last-updated');
  if (el) el.classList.remove('invisible');
  if (tickerInterval) return;
  renderTicker();
  tickerInterval = setInterval(renderTicker, 1000);
}

let pollInterval;
let pollController;

function startPolling(refreshFn, intervalMs) {
  async function poll() {
    if (pollController) pollController.abort();
    pollController = new AbortController();
    try {
      await refreshFn(pollController.signal);
      markUpdated();
    } catch (e) {
      if (e.name !== 'AbortError') console.error('refresh error:', e);
    }
  }
  ensureTicker();
  poll();
  clearInterval(pollInterval);
  pollInterval = setInterval(poll, intervalMs || CONFIG.DEFAULT_POLL_INTERVAL);
}

function stopPolling() {
  clearInterval(pollInterval);
  clearInterval(tickerInterval);
  pollInterval = null;
  tickerInterval = null;
  if (pollController) pollController.abort();
  const el = document.getElementById('last-updated');
  if (el) el.classList.add('invisible');
}

window.addEventListener('beforeunload', () => {
  clearInterval(pollInterval);
  clearInterval(tickerInterval);
  if (pollController) pollController.abort();
});
