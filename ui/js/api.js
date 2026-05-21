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

function clearTokens() {
  localStorage.removeItem('space_token');
  localStorage.removeItem('admin_token');
}

function isAdmin() {
  return !!getAdminToken();
}

async function apiRequest(path, options = {}) {
  const { method = 'GET', body, isAdminReq = false, useTokenParam = true } = options;
  let url = `${API_BASE_URL}${path}`;

  const headers = {};
  let token = getToken();

  if (isAdminReq) {
    const adminToken = getAdminToken();
    if (adminToken) {
      headers['Authorization'] = `Bearer ${adminToken}`;
    }
  } else if (token && useTokenParam) {
    const sep = path.includes('?') ? '&' : '?';
    url += `${sep}token=${encodeURIComponent(token)}`;
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
  return apiRequest('/', { useTokenParam: false });
}

async function getInventory() {
  return apiRequest('/inventory');
}

async function uploadFiles(files) {
  const fd = new FormData();
  for (const file of files) {
    fd.append('file', file);
  }
  return apiRequest('/upload', { method: 'POST', body: fd, useTokenParam: true });
}

function getDownloadUrl(fileId) {
  const token = getToken();
  const sep = '?';
  return `${API_BASE_URL}/download/${fileId}${sep}token=${encodeURIComponent(token)}`;
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
