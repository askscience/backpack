function formatBytes(bytes) {
  if (!bytes || bytes === 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  const val = bytes / Math.pow(1024, i);
  return `${val.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function formatDate(isoString) {
  if (!isoString) return '';
  const normalized = isoString.includes('T') ? isoString : isoString.replace(' ', 'T');
  const d = new Date(normalized + (normalized.endsWith('Z') ? '' : 'Z'));
  return d.toLocaleDateString('en-US', {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
}

function timeAgo(isoString) {
  if (!isoString) return '';
  const now = new Date();
  const normalized = isoString.includes('T') ? isoString : isoString.replace(' ', 'T');
  const d = new Date(normalized + (normalized.endsWith('Z') ? '' : 'Z'));
  const diff = now - d;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return 'just now';
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  return formatDate(isoString);
}

function fileIconPath(mime) {
  if (!mime) return 'icons/file-generic.svg';
  const m = mime.toLowerCase();
  if (m.startsWith('image/')) return 'icons/file-image.svg';
  if (m.startsWith('video/')) return 'icons/file-video.svg';
  if (m.startsWith('audio/')) return 'icons/file-audio.svg';
  if (m.includes('pdf')) return 'icons/file-pdf.svg';
  if (m.includes('spreadsheet') || m.includes('excel') || m.includes('xlsx') || m.includes('csv')) return 'icons/file-spreadsheet.svg';
  if (m.includes('presentation') || m.includes('powerpoint') || m.includes('pptx')) return 'icons/file-presentation.svg';
  if (m.includes('document') || m.includes('word') || m.includes('docx')) return 'icons/file-document.svg';
  if (m.includes('zip') || m.includes('tar') || m.includes('gzip') || m.includes('compress')) return 'icons/file-archive.svg';
  if (m.includes('text') || m.includes('json') || m.includes('xml') || m.includes('code') || m.includes('javascript') || m.includes('python')) return 'icons/file-code.svg';
  return 'icons/file-generic.svg';
}

function fileIcon(mime) {
  return fileIconPath(mime);
}

function escapeHtml(str) {
  const div = document.createElement('div');
  div.textContent = str;
  return div.innerHTML;
}

function getTokenParam() {
  const token = localStorage.getItem('space_token');
  return token ? `?token=${encodeURIComponent(token)}` : '';
}

function getFileExtension(filename) {
  const i = filename.lastIndexOf('.');
  return i > 0 ? filename.slice(i).toLowerCase() : '';
}

function truncate(str, len = 60) {
  if (!str || str.length <= len) return str || '';
  return str.slice(0, len) + '…';
}

function isImage(mime) {
  return mime && mime.toLowerCase().startsWith('image/');
}

function getInlineUrl(fileId) {
  return `${API_BASE_URL}/files/${fileId}/inline`;
}

function cardGradient(mime) {
  if (!mime) return 'linear-gradient(135deg, #1a1a2e, #0f3460)';
  const m = mime.toLowerCase();
  if (m.startsWith('video/')) return 'linear-gradient(135deg, #1a1a2e, #16213e)';
  if (m.startsWith('audio/')) return 'linear-gradient(135deg, #1a1a2e, #2d1b69)';
  if (m.includes('pdf')) return 'linear-gradient(135deg, #2d1b69, #16213e)';
  if (m.includes('spreadsheet') || m.includes('excel') || m.includes('xlsx') || m.includes('csv'))
    return 'linear-gradient(135deg, #0f3460, #1a1a2e)';
  if (m.includes('presentation') || m.includes('powerpoint') || m.includes('pptx'))
    return 'linear-gradient(135deg, #2d1b69, #0f3460)';
  if (m.includes('document') || m.includes('word') || m.includes('docx'))
    return 'linear-gradient(135deg, #16213e, #0f3460)';
  if (m.includes('zip') || m.includes('tar') || m.includes('gzip') || m.includes('compress'))
    return 'linear-gradient(135deg, #2d1b69, #1a1a2e)';
  if (m.includes('text') || m.includes('json') || m.includes('xml') || m.includes('code') || m.includes('javascript') || m.includes('python'))
    return 'linear-gradient(135deg, #0f3460, #1a1a2e)';
  return 'linear-gradient(135deg, #1a1a2e, #0f3460)';
}

function fileNameWithoutExt(name) {
  if (!name) return '';
  const i = name.lastIndexOf('.');
  return i > 0 ? name.slice(0, i) : name;
}

async function copyToClipboard(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.position = 'fixed';
    ta.style.opacity = '0';
    document.body.appendChild(ta);
    ta.select();
    document.execCommand('copy');
    document.body.removeChild(ta);
    return true;
  }
}
