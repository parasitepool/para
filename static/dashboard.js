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

function copyOnClick(elementId) {
  document.getElementById(elementId).addEventListener('click', function() {
    const full = this.dataset.full;
    if (!full) return;
    navigator.clipboard.writeText(full).then(() => {
      const prev = this.textContent;
      this.textContent = 'Copied!';
      setTimeout(() => { this.textContent = prev; }, 1000);
    });
  });
}

function startPolling(refreshFn, intervalMs) {
  refreshFn();
  setInterval(refreshFn, intervalMs || 1000);
}
