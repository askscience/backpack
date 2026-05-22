const API_BASE_URL = localStorage.getItem('api_base_url') || 'http://localhost:8080';

function setApiBaseUrl(url) {
  localStorage.setItem('api_base_url', url);
  window.API_BASE_URL = url;
}

function getToken() {
  return localStorage.getItem('space_token') || '';
}

function getAdminToken() {
  return localStorage.getItem('admin_token') || '';
}

function setToken(token) {
  localStorage.setItem('space_token', token);
}

function setAdminToken(token) {
  localStorage.setItem('admin_token', token);
}

function setSessionToken(token) {
  localStorage.setItem('session_token', token);
}

function getSessionToken() {
  return localStorage.getItem('session_token') || '';
}

function clearTokens() {
  localStorage.removeItem('space_token');
  localStorage.removeItem('admin_token');
  localStorage.removeItem('session_token');
}

function isAdmin() {
  return !!getAdminToken();
}

async function apiRequest(path, options = {}) {
  const { method = 'GET', body, isAdminReq = false } = options;
  const url = `${API_BASE_URL}${path}`;

  const headers = {};

  if (isAdminReq) {
    const adminToken = getAdminToken();
    if (adminToken) {
      headers['Authorization'] = `Bearer ${adminToken}`;
    }
  } else {
    const token = getToken();
    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }
  }

  if (body && !(body instanceof FormData)) {
    headers['Content-Type'] = 'application/json';
  }

  const res = await fetch(url, {
    method,
    headers,
    body: body instanceof FormData ? body : body ? JSON.stringify(body) : undefined,
  });

  if (res.status === 404 && isAdminReq) {
    throw new Error('Not found — check admin token');
  }
  if (res.status === 403) {
    clearTokens();
    setTimeout(() => { window.location.href = 'index.html'; }, 100);
    throw new Error('Forbidden — invalid or expired token');
  }
  if (res.status === 413) {
    throw new Error('Quota exceeded — file too large or space is full');
  }
  if (!res.ok) {
    const text = await res.text();
    let msg = text;
    try {
      const j = JSON.parse(text);
      msg = j.error || j.message || text;
    } catch {}
    throw new Error(msg);
  }

  const ct = res.headers.get('content-type') || '';
  if (ct.includes('application/json')) {
    return res.json();
  }
  if (ct.includes('text/plain') || ct.includes('text/html')) {
    return res.text();
  }
  return res;
}

async function getServerInfo() {
  return apiRequest('/', { isAdminReq: false });
}

async function getInventory() {
  return apiRequest('/inventory');
}

async function uploadFiles(files) {
  const fd = new FormData();
  for (const file of files) {
    fd.append('file', file);
  }
  return apiRequest('/upload', { method: 'POST', body: fd });
}

function getDownloadUrl(fileId) {
  return `${API_BASE_URL}/download/${fileId}`;
}

async function downloadFile(fileId, filename) {
  const res = await fetch(getDownloadUrl(fileId), {
    headers: { 'Authorization': `Bearer ${getToken()}` },
  });
  if (!res.ok) throw new Error('Download failed');
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename || 'download';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

function triggerDownload(fileId, filename) {
  downloadFile(fileId, filename).catch(function(e) {
    toast('Download failed: ' + e.message, true);
  });
}

async function deleteFile(fileId) {
  return apiRequest(`/files/${fileId}`, { method: 'DELETE' });
}

async function searchFiles(query) {
  return apiRequest(`/search?q=${encodeURIComponent(query)}`);
}

async function askQuestion(question) {
  return apiRequest('/ask', {
    method: 'POST',
    body: { question },
  });
}

async function getAdminSpaces() {
  return apiRequest('/api/admin/spaces', { isAdminReq: true });
}

async function getAdminSpace(id) {
  return apiRequest(`/api/admin/spaces/${id}`, { isAdminReq: true });
}

async function createAdminSpace(label, quotaMb) {
  return apiRequest('/api/admin/spaces', {
    method: 'POST',
    body: { label, quota_mb: quotaMb },
    isAdminReq: true,
  });
}

async function deleteAdminSpace(id, mode = 'purge') {
  return apiRequest(`/api/admin/spaces/${id}?mode=${mode}`, {
    method: 'DELETE',
    isAdminReq: true,
  });
}

async function createAdminShare(spaceId, label) {
  return apiRequest(`/api/admin/spaces/${spaceId}/share`, {
    method: 'POST',
    body: { label },
    isAdminReq: true,
  });
}

async function revokeAdminShare(spaceId, shareToken) {
  return apiRequest(`/api/admin/spaces/${spaceId}/shares/${encodeURIComponent(shareToken)}/revoke`, {
    method: 'POST',
    isAdminReq: true,
  });
}

async function updateAdminSpace(spaceId, data) {
  return apiRequest(`/api/admin/spaces/${spaceId}`, {
    method: 'PUT',
    body: data,
    isAdminReq: true,
  });
}

async function regenerateAdminToken(spaceId) {
  return apiRequest(`/api/admin/spaces/${spaceId}/regenerate-token`, {
    method: 'POST',
    isAdminReq: true,
  });
}

async function reactivateAdminSpace(spaceId) {
  return apiRequest(`/api/admin/spaces/${spaceId}/reactivate`, {
    method: 'POST',
    isAdminReq: true,
  });
}

async function getAdminAllShares() {
  return apiRequest('/api/admin/shares', { isAdminReq: true });
}

async function deleteAdminShare(shareToken) {
  return apiRequest(`/api/admin/shares/${encodeURIComponent(shareToken)}`, {
    method: 'DELETE',
    isAdminReq: true,
  });
}

async function getAdminArchives() {
  return apiRequest('/api/admin/archives', { isAdminReq: true });
}

// ── Sync API ───────────────────────────────────────────────────────

async function getSyncConfig() {
  return apiRequest('/sync/config');
}

async function updateSyncConfig(data) {
  return apiRequest('/sync/config', {
    method: 'PUT',
    body: data,
  });
}

async function getSyncConfigDownloadUrl() {
  return `${API_BASE_URL}/sync/config/download`;
}

async function downloadSyncConfig() {
  const res = await fetch(getSyncConfigDownloadUrl(), {
    headers: { 'Authorization': `Bearer ${getToken()}` },
  });
  if (!res.ok) throw new Error('Config download failed');
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = '.backpack-sync.toml';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

async function getSyncStatus() {
  return apiRequest('/sync/status');
}

// ── Auth ────────────────────────────────────────────────────────────

async function apiLogout() {
  const session = getSessionToken();
  if (session) {
    try {
      await fetch(`${API_BASE_URL}/api/webauthn/logout`, {
        method: 'POST',
        headers: { 'Authorization': `Bearer ${session}` },
      });
    } catch (_) {}
  }
}
