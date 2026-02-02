const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';

function connectWs() {
  const logsEl = document.getElementById('logs');
  const pauseBtn = document.getElementById('log-pause');
  const filterInput = document.getElementById('log-filter');
  const clearBtn = document.getElementById('log-clear');

  let paused = false;
  let allLogs = [];
  let pausedLogs = [];
  const maxLogs = 500;

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
    const toShow = filtered.slice(-100);
    toShow.forEach(log => {
      logsEl.appendChild(createLogLine(log.level, log.text));
    });
    if (!paused) {
      logsEl.scrollTop = logsEl.scrollHeight;
    }
  }

  if (pauseBtn) {
    pauseBtn.addEventListener('click', () => {
      paused = !paused;
      pauseBtn.textContent = paused ? 'Resume' : 'Pause';
      pauseBtn.className = paused ? 'paused' : '';
      if (paused) {
        pausedLogs = [...allLogs];
      } else {
        renderLogs();
      }
    });
  }

  if (filterInput) {
    filterInput.addEventListener('input', renderLogs);
  }

  if (clearBtn) {
    clearBtn.addEventListener('click', () => {
      allLogs = [];
      pausedLogs = [];
      if (filterInput) filterInput.value = '';
      renderLogs();
    });
  }

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
  ws.onclose = () => setTimeout(connectWs, 2000);
}

function formatDifficulty(d) {
  if (d == null) return null;
  if (d < 1) {
    return d.toFixed(8).replace(/\.?0+$/, '');
  }
  const prefixes = ['', 'K', 'M', 'G', 'T', 'P', 'E'];
  const i = Math.min(Math.floor(Math.log10(d) / 3), prefixes.length - 1);
  const scaled = d / Math.pow(10, i * 3);
  const truncated = Math.floor(scaled * 100) / 100;
  return truncated.toFixed(2) + prefixes[i];
}

function formatHashrate(h) {
  if (h == null) return null;
  if (h === 0) return '0 H/s';
  const prefixes = ['', 'K', 'M', 'G', 'T', 'P', 'E'];
  const i = Math.min(Math.floor(Math.log10(h) / 3), prefixes.length - 1);
  const scaled = h / Math.pow(1000, i);
  const truncated = Math.floor(scaled * 100) / 100;
  return truncated.toFixed(2) + ' ' + prefixes[i] + 'H/s';
}

function fmt2t(n) {
  if (n == null) return '-';
  const truncated = Math.floor(n * 100) / 100;
  return truncated.toLocaleString(undefined, {minimumFractionDigits: 2, maximumFractionDigits: 2});
}

function setupCopyOnClick(id) {
  const el = document.getElementById(id);
  if (!el) return;
  el.addEventListener('click', async function() {
    const full = this.dataset.full;
    if (!full || full === 'undefined') return;
    try {
      await navigator.clipboard.writeText(full);
      const prev = this.textContent;
      this.textContent = 'Copied!';
      setTimeout(() => { this.textContent = this.dataset.formatted || prev; }, 1000);
    } catch (e) {}
  });
}

function setupHoverExpand(id) {
  const el = document.getElementById(id);
  if (!el) return;
  el.addEventListener('mouseenter', () => {
    if (el.dataset.full) el.textContent = el.dataset.full;
  });
  el.addEventListener('mouseleave', () => {
    if (el.dataset.formatted) el.textContent = el.dataset.formatted;
  });
}

function setupCopyable(id) {
  setupCopyOnClick(id);
  setupHoverExpand(id);
}

function setupLogToggle() {
  const showBtn = document.getElementById('log-show');
  const hideBtn = document.getElementById('log-hide');
  const controls = document.getElementById('log-controls');
  const logs = document.getElementById('logs');
  if (showBtn && controls && logs) {
    showBtn.addEventListener('click', () => {
      showBtn.style.display = 'none';
      controls.style.display = '';
      logs.style.display = '';
    });
  }
  if (hideBtn && showBtn && controls && logs) {
    hideBtn.addEventListener('click', () => {
      showBtn.style.display = '';
      controls.style.display = 'none';
      logs.style.display = 'none';
    });
  }
}

function startPolling(refreshFn, intervalMs) {
  refreshFn();
  setInterval(refreshFn, intervalMs || 1000);
}
