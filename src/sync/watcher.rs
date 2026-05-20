use anyhow::Result;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
pub enum SyncEvent {
    Changed(PathBuf),
    Deleted(PathBuf),
}

/// Watches a directory tree for filesystem changes and emits debounced SyncEvents.
pub struct FileWatcher {
    pending: Arc<tokio::sync::Mutex<HashMap<PathBuf, (Instant, EventKind)>>>,
    debounce: Duration,
    ignore_patterns: Vec<String>,
}

impl FileWatcher {
    pub fn new(debounce_ms: u64, ignore_patterns: Vec<String>) -> Self {
        Self {
            pending: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            debounce: Duration::from_millis(debounce_ms),
            ignore_patterns,
        }
    }

    pub async fn start(
        &self,
        root_dir: &Path,
    ) -> Result<(mpsc::UnboundedReceiver<SyncEvent>, tokio::task::JoinHandle<()>)> {
        let root = root_dir.to_path_buf();
        let (tx, rx) = mpsc::unbounded_channel();
        let debounce = self.debounce;
        let ignore_patterns = self.ignore_patterns.clone();

        let pending_debounce = self.pending.clone();
        let pending_flush = self.pending.clone();
        let tx_events = tx.clone();
        let tx_flush = tx.clone();

        let (instant_tx, mut instant_rx) = mpsc::unbounded_channel::<notify::Event>();

        let watch_root = root.clone();
        std::thread::spawn(move || {
            let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
                match res {
                    Ok(event) => { let _ = instant_tx.send(event); }
                    Err(e) => { warn!("Notify error: {}", e); }
                }
            }).expect("Failed to create file watcher");
            if let Err(e) = watcher.watch(&watch_root, RecursiveMode::Recursive) {
                warn!("Failed to watch {}: {}", watch_root.display(), e);
            } else { info!("Watching: {}", watch_root.display()); }
            std::thread::park();
        });

        let handle_debounce = tokio::spawn(async move {
            while let Some(event) = instant_rx.recv().await {
                for path in &event.paths {
                    if should_ignore(path, &root, &ignore_patterns) { continue; }
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            pending_debounce.lock().await.insert(path.clone(),
                                (Instant::now(), EventKind::Modify(notify::event::ModifyKind::Any)));
                        }
                        EventKind::Remove(_) => {
                            let _ = tx_events.send(SyncEvent::Deleted(path.clone()));
                        }
                        _ => {}
                    }
                }
            }
        });

        let handle_flush = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(200));
            loop {
                interval.tick().await;
                let mut map = pending_flush.lock().await;
                let mut to_emit: Vec<PathBuf> = Vec::new();
                let mut to_keep: HashMap<PathBuf, (Instant, EventKind)> = HashMap::new();
                for (path, (start, kind)) in map.drain() {
                    if start.elapsed() >= debounce { to_emit.push(path); }
                    else { to_keep.insert(path, (start, kind)); }
                }
                *map = to_keep;
                for path in to_emit {
                    debug!("Sync event: changed {}", path.display());
                    if tx_flush.send(SyncEvent::Changed(path)).is_err() { return; }
                }
            }
        });

        let combined_handle = tokio::spawn(async move {
            tokio::select! { _ = handle_debounce => {}, _ = handle_flush => {} }
        });

        Ok((rx, combined_handle))
    }
}

pub fn should_ignore(path: &Path, root: &Path, patterns: &[String]) -> bool {
    if is_hidden_file(path) { return true; }
    if is_state_file(path) { return true; }
    let relative = match path.strip_prefix(root) {
        Ok(r) => r.to_string_lossy().to_string(),
        Err(_) => return false,
    };
    let filename = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
    for pattern in patterns {
        if matches_simple(&relative, pattern) || matches_simple(&filename, pattern) { return true; }
    }
    false
}

fn matches_simple(name: &str, pattern: &str) -> bool {
    if name == pattern { return true; }
    if pattern.starts_with("*.") { return name.ends_with(&pattern[1..]); }
    if pattern.ends_with("/*") { return name.starts_with(&pattern[..pattern.len() - 2]); }
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() >= 2 {
            let prefix_ok = parts[0].is_empty() || name.starts_with(parts[0]);
            let suffix_ok = parts[parts.len() - 1].is_empty() || name.ends_with(parts[parts.len() - 1]);
            return prefix_ok && suffix_ok;
        }
    }
    false
}

fn is_state_file(path: &Path) -> bool {
    let name = path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default();
    name == ".backpack-sync.toml" || name == ".backpack-sync.db"
        || name == ".backpack-sync.db-wal" || name == ".backpack-sync.db-shm"
        || name == ".DS_Store" || name.starts_with("~$")
}

fn is_hidden_file(path: &Path) -> bool {
    path.file_name().map(|f| f.to_string_lossy().starts_with('.')).unwrap_or(false)
}
