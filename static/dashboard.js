let wsReconnectTimeout;

const CONFIG = {
  MAX_LOGS: 500,
  LOGS_TO_SHOW: 100,
  WS_RECONNECT_DELAY: 2000,
  COPY_FEEDBACK_DURATION: 1000,
  DEFAULT_POLL_INTERVAL: 1000
};

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
  logState.ws = ws;

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

function formatHashrate(h) {
  if (h === null || h === undefined) return null;
  if (h < 1) return '0 H/s';
  const i = Math.max(0, Math.min(Math.floor(Math.log10(h) / 3), SI_PREFIXES.length - 1));
  const scaled = h / Math.pow(1000, i);
  const truncated = Math.floor(scaled * 100) / 100;
  return truncated.toFixed(2) + ' ' + SI_PREFIXES[i] + 'H/s';
}

function formatPhDays(v) {
  if (v === null || v === undefined) return null;
  return v.toFixed(2) + ' PHd';
}

function formatTruncated(n) {
  if (n === null || n === undefined) return '-';
  if (Math.abs(n) < 0.005) return '0.00';
  const truncated = Math.floor(n * 100) / 100;
  return truncated.toLocaleString(undefined, {minimumFractionDigits: 2, maximumFractionDigits: 2});
}

function efficiencyPct(upstreamAccepted, downstreamAccepted, downstreamRejected) {
  const total = (downstreamAccepted || 0) + (downstreamRejected || 0);
  if (total === 0) return '-';
  return (((upstreamAccepted || 0) / total) * 100).toFixed(2) + '%';
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
    el.dataset.full = String(raw);
    el.dataset.formatted = formatted;
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
      if (!full || full === 'undefined') return;
      try {
        await navigator.clipboard.writeText(full);
        const formatted = el.dataset.formatted || el.textContent;
        el.textContent = 'Copied!';
        setTimeout(() => { el.textContent = formatted; }, CONFIG.COPY_FEEDBACK_DURATION);
      } catch (e) { console.error('Copy failed:', e); }
    });

    el.addEventListener('mouseenter', () => {
      if (el.dataset.full) el.textContent = el.dataset.full;
    });

    el.addEventListener('mouseleave', () => {
      if (el.dataset.formatted) el.textContent = el.dataset.formatted;
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
    if (!full || full === 'undefined') return;
    try {
      await navigator.clipboard.writeText(full);
      const formatted = el.dataset.formatted || el.textContent;
      el.textContent = 'Copied!';
      setTimeout(() => { el.textContent = formatted; }, CONFIG.COPY_FEEDBACK_DURATION);
    } catch (e) { console.error('Copy failed:', e); }
  });
  container.addEventListener('mouseenter', (e) => {
    const el = e.target.closest(selector);
    if (el && el.dataset.full) el.textContent = el.dataset.full;
  }, true);
  container.addEventListener('mouseleave', (e) => {
    const el = e.target.closest(selector);
    if (el && el.dataset.formatted) el.textContent = el.dataset.formatted;
  }, true);
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
  set('btc_height', data.height);
  const link = document.getElementById('btc_height_link');
  if (link && data.height != null) link.href = `https://mempool.space/block/${data.height}`;
  copyable('network_difficulty', formatDifficulty(data.network_difficulty), data.network_difficulty);
  set('network_hashrate', data.network_hashrate, formatHashrate);
  set('mempool_txs', data.mempool_txs);
}

function renderSystemData(data) {
  set('cpu_usage_percent', data.cpu_usage_percent, formatTruncated);
  set('memory_usage_percent', data.memory_usage_percent, formatTruncated);
  set('disk_usage_percent', data.disk_usage_percent, formatTruncated);
  set('uptime', data.uptime);
}

function renderWorkerRows(workers) {
  return workers.sort((a, b) => b.hashrate_1m - a.hashrate_1m).map(w => {
    const lastShare = w.last_share != null ? `${w.last_share}s ago` : '-';
    const bestShare = formatDifficulty(w.best_share);
    return `<tr>
      <td>${w.name || '(default)'}</td>
      <td>${w.session_count}</td>
      <td>${formatHashrate(w.hashrate_1m)}</td>
      <td>${formatTruncated(w.sps_1m)}</td>
      <td>${bestShare || '-'}</td>
      <td>${w.ph_days != null ? w.ph_days.toFixed(2) : '-'}</td>
      <td>${lastShare}</td>
    </tr>`;
  }).join('');
}

function renderSessionRows(sessions) {
  return sessions.sort((a, b) => b.hashrate_1m - a.hashrate_1m).map(session => {
    const sessionUser = session.username || '';
    const shortSessionUser = truncateMiddle(sessionUser);
    const lastShare = session.last_share != null ? `${session.last_share}s ago` : '-';
    const bestShare = formatDifficulty(session.best_share);
    return `<tr>
      <td><span class="copyable hover-expand session-username" data-full="${sessionUser}" data-formatted="${shortSessionUser}">${shortSessionUser}</span></td>
      <td>${formatHashrate(session.hashrate_1m)}</td>
      <td>${formatTruncated(session.sps_1m)}</td>
      <td>${bestShare || '-'}</td>
      <td>${session.ph_days != null ? session.ph_days.toFixed(2) : '-'}</td>
      <td>${lastShare}</td>
    </tr>`;
  }).join('');
}

let pollInterval;
let pollController;

function startPolling(refreshFn, intervalMs) {
  function poll() {
    if (pollController) pollController.abort();
    pollController = new AbortController();
    refreshFn(pollController.signal);
  }
  poll();
  clearInterval(pollInterval);
  pollInterval = setInterval(poll, intervalMs || CONFIG.DEFAULT_POLL_INTERVAL);
}

window.addEventListener('beforeunload', () => {
  clearInterval(pollInterval);
  if (pollController) pollController.abort();
});
