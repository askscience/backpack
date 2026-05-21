function getTheme() {
  return localStorage.getItem('theme') || 'light';
}

function setTheme(theme) {
  localStorage.setItem('theme', theme);
  document.documentElement.setAttribute('data-theme', theme);
}

function toggleTheme() {
  const current = getTheme();
  setTheme(current === 'dark' ? 'light' : 'dark');
}

function initTheme() {
  const saved = getTheme();
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
  const theme = saved !== 'light' && saved !== 'dark'
    ? (prefersDark ? 'dark' : 'light')
    : saved;
  setTheme(theme);
}

function logout() {
  clearTokens();
  window.location.href = 'index.html';
}

function renderNav(currentPage) {
  const hasSpace = !!localStorage.getItem('space_token');
  const hasAdmin = !!localStorage.getItem('admin_token');

  const nav = document.getElementById('main-nav');
  if (!nav) return;

  let html = `
    <nav class="top-nav" x-data="{ menuOpen: false }">
      <div class="nav-inner">
        <a href="${hasAdmin ? 'admin.html' : 'dashboard.html'}" class="nav-logo">BACKPACK</a>
        <div class="nav-links" :class="{ 'open': menuOpen }">
  `;

  if (hasSpace) {
    const spacePages = [
      { id: 'dashboard', label: 'Dashboard', href: 'dashboard.html' },
      { id: 'files', label: 'Files', href: 'files.html' },
      { id: 'search', label: 'Search', href: 'search.html' },
      { id: 'ask', label: 'Ask', href: 'ask.html' },
    ];
    for (const p of spacePages) {
      const active = p.id === currentPage;
      html += `<a href="${p.href}" class="nav-link ${active ? 'active' : ''}">${p.label}</a>`;
    }
  }

  if (hasAdmin) {
    const active = currentPage === 'admin';
    html += `<a href="admin.html" class="nav-link ${active ? 'active' : ''}">Admin</a>`;
  }

  html += `<div class="nav-sep"></div>`;
  html += `<a href="#" onclick="logout();return false" class="nav-link nav-logout-link">Logout</a>`;

  html += `
        </div>
        <div class="nav-right">
          <button onclick="toggleTheme()" class="theme-btn" aria-label="Toggle theme">
            <svg class="theme-icon sun-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <circle cx="12" cy="12" r="5"/><line x1="12" y1="1" x2="12" y2="3"/><line x1="12" y1="21" x2="12" y2="23"/><line x1="4.22" y1="4.22" x2="5.64" y2="5.64"/><line x1="18.36" y1="18.36" x2="19.78" y2="19.78"/><line x1="1" y1="12" x2="3" y2="12"/><line x1="21" y1="12" x2="23" y2="12"/><line x1="4.22" y1="19.78" x2="5.64" y2="18.36"/><line x1="18.36" y1="5.64" x2="19.78" y2="4.22"/>
            </svg>
            <svg class="theme-icon moon-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
              <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z"/>
            </svg>
          </button>
          <button @click="menuOpen = !menuOpen" class="hamburger" aria-label="Menu">
            <span></span><span></span><span></span>
          </button>
        </div>
      </div>
    </nav>
  `;

  nav.innerHTML = html;
}

function showLoading(container) {
  const el = document.getElementById(container);
  if (!el) return;
  el.innerHTML = `<div class="loading"><div class="loading-spinner"></div></div>`;
}

function showError(container, message) {
  const el = document.getElementById(container);
  if (!el) return;
  el.innerHTML = `<div class="error-state"><p>${escapeHtml(message)}</p></div>`;
}

function showEmpty(container, message) {
  const el = document.getElementById(container);
  if (!el) return;
  el.innerHTML = `<div class="empty-state"><p>${escapeHtml(message)}</p></div>`;
}

function modal(html) {
  const existing = document.querySelector('.modal-overlay');
  if (existing) existing.remove();

  const overlay = document.createElement('div');
  overlay.className = 'modal-overlay';
  overlay.innerHTML = `<div class="modal">${html}</div>`;
  overlay.addEventListener('click', (e) => {
    if (e.target === overlay) overlay.remove();
  });
  document.body.appendChild(overlay);
  return overlay;
}

function closeModal() {
  const m = document.querySelector('.modal-overlay');
  if (m) m.remove();
}

function toast(msg, isError = false) {
  const el = document.createElement('div');
  el.className = `toast show${isError ? ' error' : ''}`;
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => {
    el.classList.remove('show');
    setTimeout(() => el.remove(), 300);
  }, 3000);
}
