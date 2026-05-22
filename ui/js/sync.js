function syncApp() {
  return {
    config: {
      watch_dirs: [],
      ignore_patterns: [],
      poll_interval_secs: 30,
      debounce_ms: 500,
      enabled: true,
    },
    status: {
      total_tracked: 0, synced: 0, pending: 0, conflicted: 0, errors: 0, entries: [],
    },
    loading: true, saving: false,
    wsStatus: 'disconnected', liveEvents: [],
    watchDirInput: '', ignorePatternInput: '', statusFilter: 'all',

    async init() {
      renderNav('sync');
      await this.load();
    },

    async load() {
      try {
        const [cfg, st] = await Promise.all([
          getSyncConfig().catch(() => null),
          getSyncStatus().catch(() => null),
        ]);
        if (cfg && cfg.watch_dirs) {
          this.config.watch_dirs = cfg.watch_dirs;
          this.config.ignore_patterns = cfg.ignore_patterns || [];
          this.config.poll_interval_secs = cfg.poll_interval_secs || 30;
          this.config.debounce_ms = cfg.debounce_ms || 500;
          this.config.enabled = cfg.enabled !== false;
        }
        if (st) {
          this.status.total_tracked = st.total_tracked || 0;
          this.status.synced = st.synced || 0;
          this.status.pending = st.pending || 0;
          this.status.conflicted = st.conflicted || 0;
          this.status.errors = st.errors || 0;
          this.status.entries = st.entries || [];
        }
      } catch (e) {
        toast('Failed to load sync data: ' + e.message, true);
      }
      this.loading = false;
      this.connectWs();
    },

    async saveConfig() {
      this.saving = true;
      try {
        await updateSyncConfig({
          watch_dirs: this.config.watch_dirs,
          ignore_patterns: this.config.ignore_patterns,
          poll_interval_secs: this.config.poll_interval_secs,
          debounce_ms: this.config.debounce_ms,
          enabled: this.config.enabled,
        });
        toast('Sync config saved.');
      } catch (e) {
        toast('Failed to save: ' + e.message, true);
      }
      this.saving = false;
    },

    async downloadConfig() {
      try {
        await downloadSyncConfig();
        toast('Config downloaded');
      } catch (e) {
        toast('Download failed: ' + e.message, true);
      }
    },

    addWatchDir() {
      const dir = this.watchDirInput.trim();
      if (!dir) return;
      if (!this.config.watch_dirs.includes(dir)) {
        this.config.watch_dirs.push(dir);
      }
      this.watchDirInput = '';
    },

    removeWatchDir(idx) {
      this.config.watch_dirs.splice(idx, 1);
    },

    addIgnorePattern() {
      const pat = this.ignorePatternInput.trim();
      if (!pat) return;
      if (!this.config.ignore_patterns.includes(pat)) {
        this.config.ignore_patterns.push(pat);
      }
      this.ignorePatternInput = '';
    },

    removeIgnorePattern(idx) {
      this.config.ignore_patterns.splice(idx, 1);
    },

    filteredEntries() {
      if (this.statusFilter === 'all') return this.status.entries;
      return this.status.entries.filter(function(e) { return e.sync_status === this.statusFilter; }, this);
    },

    statusBadge(s) {
      var map = {
        synced: '<span class="badge badge-green">Synced</span>',
        pending_upload: '<span class="badge badge-yellow">Pending</span>',
        pending_download: '<span class="badge badge-yellow">Pending</span>',
        conflicted: '<span class="badge badge-red">Conflicted</span>',
        error: '<span class="badge badge-gray">Error</span>',
      };
      return map[s] || '<span class="badge">' + escapeHtml(s) + '</span>';
    },

    connectWs() {
      if (this.wsStatus === 'connecting' || this.wsStatus === 'connected') return;
      this.wsStatus = 'disconnected';
      this.addLiveEvent('WebSocket sync requires sync-token — use the CLI daemon for live sync');
      return;
    },

    handleWsEvent(data) {
      if (data.type === 'revoked') {
        this.addLiveEvent('Sync revoked — space share was removed');
        return;
      }
      var name = data.original_name || data.file_id || 'unknown';
      var type = data.typ || data.type || 'unknown';
      this.addLiveEvent(type + ': ' + name);
      this.load();
    },

    addLiveEvent(msg) {
      this.liveEvents.unshift({
        msg: msg,
        time: new Date().toLocaleTimeString(),
      });
      if (this.liveEvents.length > 50) {
        this.liveEvents.pop();
      }
    },

    clearEvents() {
      this.liveEvents = [];
    },

    formatBytes(b) {
      if (typeof b !== 'number') return '0 B';
      if (b < 1024) return b + ' B';
      if (b < 1048576) return (b / 1024).toFixed(1) + ' KB';
      if (b < 1073741824) return (b / 1048576).toFixed(1) + ' MB';
      return (b / 1073741824).toFixed(2) + ' GB';
    },

    timeAgo(ts) {
      if (!ts) return '';
      var d = new Date(ts + 'Z');
      var now = new Date();
      var diff = (now - d) / 1000;
      if (diff < 60) return 'just now';
      if (diff < 3600) return Math.floor(diff / 60) + 'm ago';
      if (diff < 86400) return Math.floor(diff / 3600) + 'h ago';
      return Math.floor(diff / 86400) + 'd ago';
    },
  };
}
