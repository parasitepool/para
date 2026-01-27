const protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';

function connectWs() {
  const logsEl = document.getElementById('logs');
  const ws = new WebSocket(`${protocol}//${location.host}/ws/logs`);
  ws.onmessage = (e) => {
    const [level, ...rest] = e.data.split('\t');
    const line = document.createElement('div');
    const lvl = document.createElement('span');
    lvl.className = 'log-' + level.trim().toLowerCase();
    lvl.textContent = level;
    line.appendChild(lvl);
    line.appendChild(document.createTextNode(' ' + rest.join('\t')));
    logsEl.appendChild(line);
    while (logsEl.children.length > 100) {
      logsEl.removeChild(logsEl.firstChild);
    }
    logsEl.scrollTop = logsEl.scrollHeight;
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
