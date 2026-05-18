//! Session supervisor: non-intrusive indicator + consent gate + session log.
//!
//! Compliance: no screen tinting, flashing overlays, or full-screen filters.
//! The owner of the host machine is informed via:
//!   1. A desktop notification on connect and disconnect.
//!   2. A console line in the terminal that started the host.
//!   3. An optional native-OS yes/no consent dialog when --require-consent is set.
//!   4. An append-only ~/.remote-lab/sessions.log of every connection.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Decision returned by the consent gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    Allow,
    Deny,
}

/// Shared session-supervisor state. Cheap to clone (Arc internally where needed).
pub struct Supervisor {
    log_path: PathBuf,
    /// Peer IPs that the host operator has already approved for this run.
    approved: Mutex<HashSet<String>>,
    /// Whether to actually try desktop notifications. Disabled with --no-notifications.
    notifications_enabled: bool,
}

impl Supervisor {
    pub fn new(notifications_enabled: bool) -> Self {
        Self {
            log_path: default_log_path(),
            approved: Mutex::new(HashSet::new()),
            notifications_enabled,
        }
    }

    /// Ask the host operator whether to allow a viewer. Returns Allow if `require` is false.
    ///
    /// `require` toggles the prompt; when false this is a no-op that returns Allow.
    /// The prompt is delivered to the terminal that started `remote-host` and waits up to
    /// `timeout` for a `y` / `n` line. If stdin is not available or the timer fires, the
    /// connection is denied.
    pub async fn gate(&self, peer: SocketAddr, require: bool, timeout: Duration) -> Consent {
        if !require {
            return Consent::Allow;
        }
        let peer_ip = peer.ip().to_string();
        if self.approved.lock().unwrap().contains(&peer_ip) {
            return Consent::Allow;
        }
        let decision = prompt_terminal(&peer_ip, timeout).await;
        if decision == Consent::Allow {
            self.approved.lock().unwrap().insert(peer_ip);
        }
        decision
    }

    /// Called when a viewer establishes a WebSocket session.
    pub fn on_connect(&self, peer: SocketAddr) {
        let line = format!("viewer connected: {}", peer);
        eprintln!("[remote-lab] {}", line);
        self.append_log(&format!(
            "{} CONNECT  {}",
            iso_now(),
            peer
        ));
        if self.notifications_enabled {
            self.notify("Remote session started", &format!("Viewer connected from {}", peer));
        }
    }

    /// Called when a viewer disconnects. Pass the connect timestamp so we can record duration.
    pub fn on_disconnect(&self, peer: SocketAddr, started: SystemTime) {
        let secs = SystemTime::now()
            .duration_since(started)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = format!("viewer disconnected: {} (duration {}s)", peer, secs);
        eprintln!("[remote-lab] {}", line);
        self.append_log(&format!(
            "{} DISCONN  {}  duration_s={}",
            iso_now(),
            peer,
            secs
        ));
        if self.notifications_enabled {
            self.notify(
                "Remote session ended",
                &format!("Viewer from {} disconnected after {}s", peer, secs),
            );
        }
    }

    pub fn on_denied(&self, peer: SocketAddr, reason: &str) {
        eprintln!("[remote-lab] connection denied: {} ({})", peer, reason);
        self.append_log(&format!(
            "{} DENIED   {}  reason={}",
            iso_now(),
            peer,
            reason
        ));
        if self.notifications_enabled {
            self.notify(
                "Remote session denied",
                &format!("Blocked viewer from {} ({})", peer, reason),
            );
        }
    }

    fn append_log(&self, line: &str) {
        if let Some(parent) = self.log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(f, "{}", line);
        }
    }

    fn notify(&self, summary: &str, body: &str) {
        // We don't want notifier failures to take down the session — log and move on.
        if let Err(e) = notify_rust::Notification::new()
            .summary(summary)
            .body(body)
            .appname("remote-lab")
            .timeout(notify_rust::Timeout::Milliseconds(5000))
            .show()
        {
            eprintln!("[remote-lab] notification failed: {e}");
        }
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

fn default_log_path() -> PathBuf {
    let home = std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .unwrap_or_else(|| ".".into());
    PathBuf::from(home).join(".remote-lab").join("sessions.log")
}

fn iso_now() -> String {
    // Minimal RFC 3339-ish timestamp in UTC without pulling in chrono.
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert epoch seconds to YYYY-MM-DDTHH:MM:SSZ using a small civil-date helper.
    let (y, m, d, hh, mm, ss) = civil_from_unix(secs as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, hh, mm, ss)
}

/// Convert Unix epoch seconds to (year, month, day, hour, min, sec) in UTC.
/// Based on Howard Hinnant's days_from_civil algorithm.
fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400) as u32;
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day % 3600) / 60;
    let ss = secs_of_day % 60;

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i32 + era as i32 * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hh, mm, ss)
}

/// Terminal-based consent prompt with timeout. Returns Deny on timeout or unreadable stdin.
async fn prompt_terminal(peer_ip: &str, timeout: Duration) -> Consent {
    use tokio::io::{AsyncBufReadExt, BufReader};
    eprintln!(
        "[remote-lab] CONSENT: allow viewer from {peer_ip}? type `y` + Enter within {} s to allow, anything else denies.",
        timeout.as_secs()
    );
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    match tokio::time::timeout(timeout, reader.next_line()).await {
        Ok(Ok(Some(line))) => {
            if line.trim().eq_ignore_ascii_case("y") || line.trim().eq_ignore_ascii_case("yes") {
                Consent::Allow
            } else {
                Consent::Deny
            }
        }
        _ => Consent::Deny,
    }
}

/// Apply a clipboard write coming from the viewer. Best-effort: failures are logged.
pub fn apply_clipboard_write(text: &str) -> Result<()> {
    let mut cb = arboard::Clipboard::new().context("open clipboard")?;
    cb.set_text(text.to_string()).context("set clipboard text")?;
    Ok(())
}
