(() => {
  const bridge = window.__INIT_UPDATER__;
  if (!bridge) return;
  const card = document.getElementById('desktop-update');
  const title = document.getElementById('desktop-update-title');
  const description = document.getElementById('desktop-update-description');
  const progress = document.getElementById('desktop-update-progress');
  const installButton = document.getElementById('desktop-update-install');
  const skipButton = document.getElementById('desktop-update-skip');
  let current = {status: 'idle'};
  let polling = false;
  let acting = false;

  function render(state) {
    current = state;
    card.hidden = state.dismissed || !['available', 'downloading', 'installing', 'restarting', 'error'].includes(state.status);
    if (card.hidden) return;
    const running = ['downloading', 'installing', 'restarting'].includes(state.status);
    installButton.hidden = running;
    skipButton.hidden = running;
    installButton.disabled = acting || Boolean(state.blocked);
    skipButton.disabled = acting;
    installButton.textContent = state.status === 'error' ? 'Retry' : 'Update';
    progress.hidden = state.status !== 'downloading';
    card.setAttribute('aria-busy', String(running));
    if (state.status === 'available') {
      title.textContent = 'Update available';
      description.textContent = state.blocked || `Init ${state.version} is ready. Init will restart after updating.`;
    } else if (state.status === 'downloading') {
      title.textContent = 'Downloading update';
      if (state.total > 0) {
        const percent = Math.min(100, Math.floor(state.downloaded / state.total * 100));
        progress.max = state.total;
        progress.value = Math.min(state.downloaded, state.total);
        description.textContent = `Init ${state.version} · ${percent}% downloaded`;
      } else {
        progress.removeAttribute('value');
        description.textContent = `Downloading Init ${state.version}…`;
      }
    } else if (state.status === 'installing') {
      title.textContent = 'Installing update';
      description.textContent = 'Init will restart automatically. Your services will keep running.';
    } else if (state.status === 'restarting') {
      title.textContent = 'Restarting Init';
      description.textContent = 'Your update is installed. Init will reopen shortly.';
    } else {
      title.textContent = 'Couldn’t update Init';
      description.textContent = state.message || 'Check your connection and try again.';
    }
  }

  async function refresh() {
    if (polling || acting) return;
    polling = true;
    try {
      const state = await bridge.status();
      if (!acting) render(state);
    } catch {
      // The native window may be closing or restarting.
    } finally {
      polling = false;
    }
  }

  async function act(action) {
    if (acting) return;
    acting = true;
    installButton.disabled = true;
    skipButton.disabled = true;
    try {
      const state = await bridge[action]();
      acting = false;
      render(state);
    } catch (error) {
      acting = false;
      render({...current, status: 'error', dismissed: false, message: String(error)});
    }
  }

  installButton.addEventListener('click', () => act('install'));
  skipButton.addEventListener('click', () => act('skip'));
  refresh();
  window.setInterval(refresh, 1000);
})();
