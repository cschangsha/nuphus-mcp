//! Cross-process automation lock.
//!
//! Desktop (mouse/keyboard/screen) and browser automation operate on **exclusive
//! machine resources** — at most one Agent may run an automation operation at any
//! given moment. Multiple nuphus-mcp instances (one per Agent via stdio transport)
//! coordinate through a shared lock file.
//!
//! Semantics (per the product decision):
//! - **Full coverage**: both `browser_*` and `desktop_*` tools acquire the lock.
//! - **Short hold**: the lock is held only for the duration of a single tool call;
//!   it is released immediately after the call (Drop guard), never bound to an Agent
//!   session. An idle server holds no lock, so any Agent can acquire it.
//! - **Busy rejection**: when another live holder exists, acquisition fails with a
//!   clear "busy" message instead of blocking; the caller should retry later.
//! - **Crash self-healing**: a stale lock from a crashed process expires via TTL
//!   (90s) and is reclaimed.
//!
//! Hardening (audit fixes):
//! - **Atomic publish**: the record is written to a sibling temp file and then
//!   hard-linked into place — `hard_link` fails atomically when the lock exists,
//!   so no process ever observes a half-written lock record, and acquisition
//!   keeps create-if-absent semantics.
//! - **Heartbeat renewal**: the holder refreshes `expires_at` every TTL/3 while
//!   alive, so a legitimately long tool call (e.g. `browser_wait_for` with a big
//!   timeout) cannot outlive its own lock. Crash residual is still reclaimed by
//!   TTL — the heartbeat dies with the process and only ever moves `expires_at`
//!   a bounded interval into the future.
//! - **Token ownership + rename-before-delete**: every acquisition stamps a
//!   unique `token` into the record. Release/reclaim first *rename* the lock
//!   file aside (an atomic capture of whatever was at the path), verify the
//!   captured record's token, and only then delete it — a late Drop can never
//!   delete another owner's freshly published lock.
//!
//! Lock file location: `{data_dir}/Nuphus/nuphus-mcp/automation.lock` — deliberately
//! **outside** the browser profile directory, so it stays valid regardless of profile
//! layout and can later be shared with the main Nuphus app's direct channel.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a stale lock may live before being reclaimed (crash self-healing).
/// Kept fresh by the holder's heartbeat; only a dead/hung holder lets it lapse.
const LOCK_TTL_SECS: u64 = 90;

/// Heartbeat cadence: refresh `expires_at` well before the TTL lapses.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(LOCK_TTL_SECS / 3);

/// Lock file name (inside `{data_dir}/Nuphus/nuphus-mcp/`).
const LOCK_FILE_NAME: &str = "automation.lock";

/// Process-wide uniqueness source for lock tokens (pid alone can be recycled).
static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Serialized holder info written into the lock file.
#[derive(Debug, Serialize, Deserialize)]
struct LockRecord {
    pid: u32,
    /// Unique per acquisition — ownership proof for release/reclaim.
    /// Records written by older versions lack it and fail ownership checks,
    /// so they are only ever removed via the TTL reclaim path.
    #[serde(default)]
    token: String,
    tool: String,
    acquired_at: u64,
    expires_at: u64,
}

/// Cross-process automation lock handle.
pub struct AutomationLock {
    path: PathBuf,
}

/// RAII guard: dropping releases the lock (and stops the heartbeat).
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    token: String,
    /// Dropping the sender disconnects the heartbeat's `recv_timeout` → it exits.
    heartbeat_stop: Option<mpsc::Sender<()>>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

impl AutomationLock {
    /// Resolve the shared lock file path (cross-platform data dir).
    ///
    /// Under `#[cfg(test)]` the lock lives in the temp dir so unit/integration tests
    /// never touch the real user data dir (parallel tests would otherwise fight over
    /// the production lock file).
    pub fn new() -> Self {
        #[cfg(test)]
        {
            let dir = std::env::temp_dir().join("nuphus_mcp_lock_test");
            let _ = fs::create_dir_all(&dir);
            Self {
                path: dir.join(LOCK_FILE_NAME),
            }
        }
        #[cfg(not(test))]
        {
            let base =
                dirs::data_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let dir = base.join("Nuphus").join("nuphus-mcp");
            let _ = fs::create_dir_all(&dir);
            Self {
                path: dir.join(LOCK_FILE_NAME),
            }
        }
    }

    /// Try to acquire the lock for a single tool call.
    ///
    /// Returns `Ok(guard)` when the lock is free (or a stale lock is reclaimed).
    /// Returns `Err(busy)` when another live holder owns it — the caller surfaces
    /// this as a semantic failure and the Agent retries later.
    pub fn acquire(&self, tool: &str) -> Result<LockGuard, String> {
        for _attempt in 0..3 {
            let now = unix_now();
            let record = LockRecord {
                pid: std::process::id(),
                token: new_token(),
                tool: tool.to_string(),
                acquired_at: now,
                expires_at: now + LOCK_TTL_SECS,
            };
            if publish_new(&self.path, &record)? {
                tracing::debug!(
                    "[automation-lock] acquired for '{tool}' (pid={}, token={})",
                    record.pid,
                    record.token
                );
                return Ok(LockGuard::new(self.path.clone(), record.token));
            }

            // Lock file exists: inspect the holder.
            match read_lock_record(&self.path) {
                Some(rec) if rec.expires_at > unix_now() => {
                    // Live holder → busy. Report who holds it (tool + pid) so the
                    // Agent can decide when to retry.
                    return Err(format!(
                        "Automation is busy: another Agent (pid={}) is running '{}'. \
                         Only one Agent can operate the desktop/browser at a time; retry later.",
                        rec.pid, rec.tool
                    ));
                }
                _ => {
                    // Stale (expired) or unreadable lock → reclaim it and retry.
                    tracing::warn!(
                        "[automation-lock] stale lock detected, reclaiming ({})",
                        self.path.display()
                    );
                    reclaim_stale(&self.path);
                }
            }
        }
        Err(
            "automation lock contention: could not acquire after reclaiming a stale lock; \
             retry later."
                .to_string(),
        )
    }

    /// Path of the lock file (exposed for tests / diagnostics).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl LockGuard {
    fn new(path: PathBuf, token: String) -> Self {
        let (tx, rx) = mpsc::channel();
        let heartbeat = spawn_heartbeat(
            path.clone(),
            token.clone(),
            LOCK_TTL_SECS,
            HEARTBEAT_INTERVAL,
            rx,
        );
        Self {
            path,
            token,
            heartbeat_stop: Some(tx),
            heartbeat: Some(heartbeat),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Disconnecting the sender wakes the heartbeat's recv_timeout immediately;
        // join guarantees no in-flight renewal can rewrite the record after release.
        drop(self.heartbeat_stop.take());
        if let Some(handle) = self.heartbeat.take() {
            let _ = handle.join();
        }
        release_owned(&self.path, &self.token);
    }
}

/// Unique token per acquisition: pid + counter + nanos (pid reuse across
/// process restarts cannot forge ownership).
fn new_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed),
        nanos
    )
}

/// Atomically publish the lock record **only if no lock file exists**: write the
/// payload to a unique sibling temp file, then hard-link it into place.
/// `hard_link` fails with `AlreadyExists` when the lock is held, so there is no
/// create-then-write window where another process observes a half-written record.
///
/// Returns `Ok(true)` when published, `Ok(false)` when the lock already exists.
fn publish_new(path: &Path, record: &LockRecord) -> Result<bool, String> {
    let payload = serde_json::to_string(record)
        .map_err(|e| format!("automation lock serialize failed: {e}"))?;
    let tmp = scratch_path(path, "tmp", &record.token);
    {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("automation lock tmp create failed: {e}"))?;
        if let Err(e) = file
            .write_all(payload.as_bytes())
            .and_then(|_| file.sync_all())
        {
            let _ = fs::remove_file(&tmp);
            return Err(format!("automation lock write failed: {e}"));
        }
    }
    let result = match fs::hard_link(&tmp, path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("automation lock publish failed: {e}")),
    };
    let _ = fs::remove_file(&tmp);
    result
}

/// Atomically remove a stale lock: move it aside first, then re-check the captured
/// record. Only the exact record we inspected is ever deleted — if it turned live
/// between our check and the move (a heartbeat renewal), it is put back untouched.
fn reclaim_stale(path: &Path) {
    let claim = scratch_path(path, "reclaim", &new_token());
    if fs::rename(path, &claim).is_err() {
        return; // already gone/moved — another process is handling it
    }
    let still_stale = match read_lock_record(&claim) {
        Some(rec) => rec.expires_at <= unix_now(),
        None => true, // unreadable/garbage → stale
    };
    if still_stale {
        let _ = fs::remove_file(&claim);
    } else {
        // Became live in the race window — restore it (fails harmlessly if a new
        // lock was published at the path meanwhile; the record stays live either way).
        let _ = fs::rename(&claim, path);
    }
}

/// Release **only our own** record: rename the lock file aside (atomically capturing
/// whatever was at the path), verify the captured record carries our token, and only
/// then delete it. If the record is someone else's — they reclaimed the path after
/// our TTL lapsed — put it back untouched.
fn release_owned(path: &Path, token: &str) {
    let claim = scratch_path(path, "release", token);
    match fs::rename(path, &claim) {
        Ok(()) => match read_lock_record(&claim) {
            Some(rec) if rec.token == token => {
                let _ = fs::remove_file(&claim);
                tracing::debug!(
                    "[automation-lock] released (pid={}, token={token})",
                    std::process::id()
                );
            }
            _ => {
                // Not ours: restore the rightful owner's lock. If a third party
                // already published a new lock at the path, the captured record's
                // owner lost the path anyway — remove the capture to avoid litter.
                if fs::rename(&claim, path).is_err() {
                    let _ = fs::remove_file(&claim);
                }
            }
        },
        Err(_) => {} // lock file already gone (reclaimed) — nothing to release
    }
}

/// Heartbeat loop: refresh `expires_at` every `interval` while we still own the
/// record. Stops when the stop channel disconnects (guard drop) or ownership is
/// lost. Crash safety: the thread dies with the process and each renewal only
/// moves `expires_at` `ttl_secs` into the future, so crash residual is still
/// reclaimed by the normal TTL path.
fn spawn_heartbeat(
    path: PathBuf,
    token: String,
    ttl_secs: u64,
    interval: Duration,
    stop: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        match stop.recv_timeout(interval) {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if !renew(&path, &token, ttl_secs) {
                    tracing::debug!("[automation-lock] heartbeat stopped: lock ownership lost");
                    break;
                }
            }
            // Stop signal, or the sender disconnected (guard dropped / panicked).
            _ => break,
        }
    })
}

/// Refresh `expires_at` while we still own the record. Returns false when the
/// lock was lost (record gone or owned by someone else) — the heartbeat stops.
fn renew(path: &Path, token: &str, ttl_secs: u64) -> bool {
    let Some(mut rec) = read_lock_record(path) else {
        return false;
    };
    if rec.token != token {
        return false;
    }
    rec.expires_at = unix_now() + ttl_secs;
    let Ok(payload) = serde_json::to_string(&rec) else {
        return true; // transient: retry next beat
    };
    let tmp = scratch_path(path, "renew", token);
    {
        use std::io::Write;
        let Ok(mut file) = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
        else {
            return true; // transient: retry next beat
        };
        if file.write_all(payload.as_bytes()).is_err() {
            let _ = fs::remove_file(&tmp);
            return true;
        }
    }
    // Atomic replace; readers never see a half-written record.
    let ok = fs::rename(&tmp, path).is_ok();
    if !ok {
        let _ = fs::remove_file(&tmp);
    }
    ok
}

/// Sibling scratch-file path for atomic publish/renew/release choreography.
fn scratch_path(path: &Path, kind: &str, token: &str) -> PathBuf {
    path.with_extension(format!("{kind}.{token}"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_lock_record(path: &std::path::Path) -> Option<LockRecord> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Use a temp lock path to avoid polluting the real data dir during tests.
    fn temp_lock(dir_name: &str) -> AutomationLock {
        let path = std::env::temp_dir()
            .join("nuphus_mcp_lock_test")
            .join(dir_name);
        let _ = fs::create_dir_all(&path);
        AutomationLock {
            path: path.join(LOCK_FILE_NAME),
        }
    }

    #[test]
    fn acquire_release_cycle() {
        let lock = temp_lock("cycle");
        let _ = fs::remove_file(lock.path());
        {
            let guard = lock.acquire("browser_navigate").expect("first acquire ok");
            assert!(guard.path.exists());
        } // guard dropped → released
        assert!(!lock.path().exists(), "lock file removed after guard drop");
    }

    #[test]
    fn busy_while_held() {
        let lock = temp_lock("busy");
        let _ = fs::remove_file(lock.path());
        let _guard = lock.acquire("desktop_mouse").expect("first acquire ok");
        let err = lock
            .acquire("browser_click")
            .expect_err("second acquire busy");
        assert!(err.contains("busy"), "error mentions busy: {err}");
        assert!(
            err.contains("desktop_mouse"),
            "error names the holding tool"
        );
    }

    #[test]
    fn stale_lock_is_reclaimed() {
        let lock = temp_lock("stale");
        let _ = fs::remove_file(lock.path());
        let stale = LockRecord {
            pid: 999_999,
            token: "stale-token".into(),
            tool: "browser_navigate".into(),
            acquired_at: 0,
            expires_at: unix_now().saturating_sub(1), // already expired
        };
        fs::write(lock.path(), serde_json::to_string(&stale).unwrap()).unwrap();
        let guard = lock
            .acquire("browser_snapshot")
            .expect("stale lock reclaimed");
        // New holder must be this process.
        let rec = read_lock_record(lock.path()).expect("record readable");
        assert_eq!(rec.pid, std::process::id());
        assert_eq!(rec.token, guard.token);
        drop(guard);
    }

    #[test]
    fn guard_drop_does_not_delete_others_lock() {
        let lock = temp_lock("owner_check");
        let _ = fs::remove_file(lock.path());
        // Simulate: we hold the lock, then another process reclaims it (writes its
        // own record over ours — e.g. after our TTL expired). Dropping our guard
        // must NOT delete the new owner's lock.
        {
            let guard = lock.acquire("desktop_screenshot").expect("acquire ok");
            let stolen = LockRecord {
                pid: 888_888,
                token: "other-owner-token".into(),
                tool: "desktop_mouse".into(),
                acquired_at: unix_now(),
                expires_at: unix_now() + LOCK_TTL_SECS,
            };
            fs::write(lock.path(), serde_json::to_string(&stolen).unwrap()).unwrap();
            drop(guard); // must restore the foreign record, not delete it
            assert!(lock.path().exists(), "new owner's lock preserved");
            let rec = read_lock_record(lock.path()).expect("foreign record intact");
            assert_eq!(rec.token, "other-owner-token");
            assert_eq!(rec.pid, 888_888);
        }
        let _ = fs::remove_file(lock.path());
    }

    #[test]
    fn publish_leaves_no_scratch_files() {
        let lock = temp_lock("no_scratch");
        let _ = fs::remove_file(lock.path());
        {
            let _guard = lock.acquire("browser_navigate").expect("acquire ok");
        }
        let dir = lock.path().parent().expect("lock has parent dir");
        let leftovers: Vec<_> = fs::read_dir(dir)
            .expect("dir readable")
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.contains(".tmp.") || name.contains(".renew.") || name.contains(".release.")
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "scratch files leaked: {:?}",
            leftovers
        );
    }

    #[test]
    fn tokens_are_unique_per_acquisition() {
        let lock = temp_lock("tokens");
        let _ = fs::remove_file(lock.path());
        let t1 = {
            let g = lock.acquire("desktop_mouse").expect("acquire 1");
            g.token.clone()
        };
        let t2 = {
            let g = lock.acquire("desktop_mouse").expect("acquire 2");
            g.token.clone()
        };
        assert_ne!(t1, t2, "each acquisition must stamp a unique token");
    }

    #[test]
    fn renew_extends_expiry_while_owned() {
        let lock = temp_lock("renew");
        let _ = fs::remove_file(lock.path());
        let guard = lock.acquire("browser_wait_for").expect("acquire ok");
        let before = read_lock_record(lock.path()).expect("record").expires_at;
        assert!(renew(lock.path(), &guard.token, LOCK_TTL_SECS));
        let after = read_lock_record(lock.path()).expect("record").expires_at;
        assert!(after >= before, "renew must not move expiry backwards");
        // Wrong token must not rewrite the record.
        assert!(!renew(lock.path(), "forged-token", LOCK_TTL_SECS));
        drop(guard);
    }

    #[test]
    fn heartbeat_thread_refreshes_expiry_until_stopped() {
        let lock = temp_lock("heartbeat");
        let _ = fs::remove_file(lock.path());
        // Hold the lock without the production heartbeat interfering: use a guard
        // but measure with an extra fast heartbeat on the same record.
        let guard = lock.acquire("browser_wait_for").expect("acquire ok");
        let initial = read_lock_record(lock.path()).expect("record").expires_at;

        // Distinct TTL makes the refresh unambiguous: expires_at is a seconds-granularity
        // timestamp, so an equal-TTL beat landing in the same second as acquisition
        // would be indistinguishable from "no refresh".
        let (tx, rx) = mpsc::channel();
        let handle = spawn_heartbeat(
            lock.path().to_path_buf(),
            guard.token.clone(),
            LOCK_TTL_SECS + 100,
            Duration::from_millis(20),
            rx,
        );
        // Poll until the heartbeat has visibly refreshed the record (bounded).
        let mut renewed = initial;
        for _ in 0..100 {
            std::thread::sleep(Duration::from_millis(20));
            renewed = read_lock_record(lock.path()).expect("record").expires_at;
            if renewed > initial {
                break;
            }
        }
        drop(tx); // stop signal
        handle.join().expect("heartbeat joins");

        assert!(
            renewed > initial,
            "heartbeat must refresh expires_at ({renewed} > {initial})"
        );
        drop(guard);
        assert!(!lock.path().exists(), "lock released after guard drop");
    }

    #[test]
    fn heartbeat_stops_when_lock_lost() {
        let lock = temp_lock("heartbeat_lost");
        let _ = fs::remove_file(lock.path());
        let guard = lock.acquire("desktop_mouse").expect("acquire ok");
        // Simulate reclaim by another process: our record disappears.
        fs::remove_file(lock.path()).expect("remove lock");

        let (_tx, rx) = mpsc::channel();
        let handle = spawn_heartbeat(
            lock.path().to_path_buf(),
            guard.token.clone(),
            LOCK_TTL_SECS,
            Duration::from_millis(20),
            rx,
        );
        // The heartbeat must notice the lost ownership and exit on its own —
        // no stop signal needed.
        handle.join().expect("heartbeat exits when lock lost");
        // Guard drop must tolerate the missing file.
        drop(guard);
    }

    #[test]
    fn concurrent_acquire_exactly_one_wins() {
        use std::sync::{Arc, Barrier};
        let lock = temp_lock("race");
        let _ = fs::remove_file(lock.path());
        let path = lock.path().to_path_buf();
        let threads = 8;
        let barrier = Arc::new(Barrier::new(threads));
        let mut handles = Vec::new();
        for _ in 0..threads {
            let barrier = barrier.clone();
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                let lock = AutomationLock { path };
                barrier.wait();
                // Return the guard itself so every racer is still holding (or
                // still rejected) when we count — sequential wins after an early
                // release would not prove mutual exclusion.
                lock.acquire("desktop_screenshot")
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("racer thread joins"))
            .collect();
        let wins = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(wins, 1, "exactly one racer may hold the lock at a time");
        for r in &results {
            if let Err(e) = r {
                assert!(e.contains("busy"), "losers see busy, not errors: {e}");
            }
        }
        drop(results); // all guards released here
        assert!(
            !lock.path().exists(),
            "winning guard released the lock on drop"
        );
    }
}
