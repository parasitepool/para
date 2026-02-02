let wsReconnectTimeout;

const CONFIG = {
  MAX_LOGS: 500,
  LOGS_TO_SHOW: 100,
  WS_RECONNECT_DELAY: 2000,
  COPY_FEEDBACK_DURATION: 1000,
  DEFAULT_POLL_INTERVAL: 1000
};

function connectWs() {
  const logsEl = document.getElementById('logs');
  const pauseBtn = document.getElementById('log-pause');
  const filterInput = document.getElementById('log-filter');
  const clearBtn = document.getElementById('log-clear');
  if (!logsEl) return;

  let paused = false;
  let allLogs = [];
  let pausedLogs = [];
  const maxLogs = CONFIG.MAX_LOGS;

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

  function matchesFilter(logEntry, filter) {
    if (!filter) return true;
    const searchText = (logEntry.level + ' ' + logEntry.text).toLowerCase();
    return searchText.includes(filter.toLowerCase());
  }

  function renderLogs() {
    const filter = filterInput?.value || '';
    logsEl.innerHTML = '';
    const source = paused ? pausedLogs : allLogs;
    const filtered = source.filter(log => matchesFilter(log, filter));
    const toShow = filtered.slice(-CONFIG.LOGS_TO_SHOW);
    toShow.forEach(log => {
      logsEl.appendChild(createLogLine(log.level, log.text));
    });
    if (!paused) {
      logsEl.scrollTop = logsEl.scrollHeight;
    }
  }

  pauseBtn?.addEventListener('click', () => {
    paused = !paused;
    pauseBtn.textContent = paused ? 'Resume' : 'Pause';
    pauseBtn.className = paused ? 'paused' : '';
    if (paused) {
      pausedLogs = [...allLogs];
    } else {
      renderLogs();
    }
  });

  filterInput?.addEventListener('input', renderLogs);

  clearBtn?.addEventListener('click', () => {
    allLogs = [];
    pausedLogs = [];
    if (filterInput) filterInput.value = '';
    renderLogs();
  });

  const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(`${protocol}//${location.host}/ws/logs`);
  ws.onmessage = (e) => {
    const [level, ...rest] = e.data.split('\t');
    const text = rest.join('\t');
    allLogs.push({ level, text });
    while (allLogs.length > maxLogs) {
      allLogs.shift();
    }
    if (!paused) renderLogs();
  };
  ws.onerror = (e) => {
    console.error('WebSocket error:', e);
    ws.close();
  };
  ws.onclose = () => {
    clearTimeout(wsReconnectTimeout);
    wsReconnectTimeout = setTimeout(connectWs, CONFIG.WS_RECONNECT_DELAY);
  };
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
  return truncated.toFixed(2) + ' ' + SI_PREFIXES[i];
}

function formatHashrate(h) {
  if (h === null || h === undefined) return null;
  if (h === 0) return '0 H/s';
  const i = Math.min(Math.floor(Math.log10(h) / 3), SI_PREFIXES.length - 1);
  const scaled = h / Math.pow(1000, i);
  const truncated = Math.floor(scaled * 100) / 100;
  return truncated.toFixed(2) + ' ' + SI_PREFIXES[i] + 'H/s';
}

function formatTruncated(n) {
  if (n === null || n === undefined) return '-';
  const truncated = Math.floor(n * 100) / 100;
  return truncated.toLocaleString(undefined, {minimumFractionDigits: 2, maximumFractionDigits: 2});
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

let pollInterval;

function startPolling(refreshFn, intervalMs) {
  refreshFn();
  clearInterval(pollInterval);
  pollInterval = setInterval(refreshFn, intervalMs || CONFIG.DEFAULT_POLL_INTERVAL);
}

window.addEventListener('beforeunload', () => {
  clearInterval(pollInterval);
});
