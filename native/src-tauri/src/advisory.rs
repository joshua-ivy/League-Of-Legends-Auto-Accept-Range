//! Riot-break advisory: polls Chud's Worker for an operator-flipped notice
//! ("Vanguard update broke all skin apps") and pairs it with the LOCALLY
//! installed Vanguard version so the popup can tell each user whether they
//! just need to update League or must wait for Riot.
//!
//! Advisory JSON (KV `current` on the party-relay Worker, operator-edited):
//!   { "id": "vanguard-2026-07", "active": true,
//!     "title": "...", "message": "...",
//!     "fixed_vanguard": "1.18.4.47" }   // empty/absent = no fix shipped yet
//!
//! The client never trusts the advisory to decide "you're fine" on its own:
//! `active` + a local-version comparison against `fixed_vanguard` picks the
//! stance. Local >= fixed means the user already has Riot's fix, so no popup.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::skins::slog::{log_info, log_warn};

const ADVISORY_URL: &str = "https://chud-party-relay.jivy26.workers.dev/advisory";
const POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);
const MAX_ADVISORY_BYTES: u64 = 16 * 1024;

/// Last computed UI payload — the `advisory_status` command reads this so the
/// webview can catch up if `advisory-changed` fired before its listener attached
/// (same race the updater's belt-and-suspenders check covers).
static LAST_PAYLOAD: Mutex<Option<Value>> = Mutex::new(None);

pub fn last_payload() -> Option<Value> {
    LAST_PAYLOAD.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Poll the advisory endpoint for the app's lifetime: once at launch, then
/// every `POLL_INTERVAL` — the interval is what catches a break that starts
/// while Chud sits open overnight. Best-effort: failures keep the last state.
pub async fn run(app: AppHandle) {
    let allowed = crate::net::built_in_allowed_origins();
    let client = crate::net::build_external_client(15.0, allowed.clone());
    loop {
        match crate::net::get_json_checked(&client, ADVISORY_URL, &allowed, MAX_ADVISORY_BYTES).await {
            Ok(adv) => {
                let payload = evaluate(&adv, local_vanguard_version().as_deref());
                let changed = {
                    let mut last = LAST_PAYLOAD.lock().unwrap_or_else(|e| e.into_inner());
                    let changed = last.as_ref() != Some(&payload);
                    *last = Some(payload.clone());
                    changed
                };
                if changed {
                    log_info!("[ADVISORY] state changed: {payload}");
                    let _ = app.emit("advisory-changed", payload);
                }
            }
            Err(e) => log_warn!("[ADVISORY] fetch failed (keeping last state): {e}"),
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Fold the server advisory + local Vanguard version into the UI payload.
/// `stance`: "clear" (nothing to show), "wait" (break active, no fix yet),
/// "update" (fix shipped and this machine doesn't have it — or Vanguard
/// version couldn't be read, in which case updating is still the right advice).
fn evaluate(adv: &Value, local: Option<&str>) -> Value {
    let active = adv.get("active").and_then(Value::as_bool).unwrap_or(false);
    let id = adv.get("id").and_then(Value::as_str).unwrap_or("advisory");
    let title = adv.get("title").and_then(Value::as_str).unwrap_or("Riot update is blocking skin apps");
    let message = adv.get("message").and_then(Value::as_str).unwrap_or("");
    let fixed = adv.get("fixed_vanguard").and_then(Value::as_str).unwrap_or("");

    let stance = if !active {
        "clear"
    } else if fixed.is_empty() {
        "wait"
    } else {
        match (local.and_then(parse_version), parse_version(fixed)) {
            (Some(l), Some(f)) if l >= f => "clear",
            _ => "update",
        }
    };

    json!({
        "show": stance != "clear",
        "id": id,
        "stance": stance,
        "title": title,
        "message": message,
        "fixedVanguard": fixed,
        "localVanguard": local,
    })
}

/// Numeric version components, comparable lexicographically. Build metadata
/// after `+` is dropped; `.` and `-` both separate (Riot writes "1.18.4-47",
/// the file resource yields "1.18.4.47").
fn parse_version(s: &str) -> Option<Vec<u64>> {
    let core = s.split('+').next().unwrap_or("");
    let parts: Vec<u64> =
        core.split(['.', '-']).map(|p| p.trim().parse::<u64>()).collect::<Result<_, _>>().ok()?;
    (!parts.is_empty()).then_some(parts)
}

fn vanguard_vgc_path() -> Option<PathBuf> {
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let p = program_files.join("Riot Vanguard").join("vgc.exe");
    if p.exists() {
        return Some(p);
    }
    let fallback = PathBuf::from(r"C:\Program Files\Riot Vanguard\vgc.exe");
    fallback.exists().then_some(fallback)
}

/// Installed Vanguard version from vgc.exe's VS_FIXEDFILEINFO, as
/// "maj.min.patch.build" (e.g. "1.18.4.47"). `None` if Vanguard isn't
/// installed or the resource can't be read.
#[cfg(windows)]
pub fn local_vanguard_version() -> Option<String> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };

    let path = vanguard_vgc_path()?;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wide.as_ptr()), 0, size, buf.as_mut_ptr().cast()).ok()?;

        let mut info_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut info_len: u32 = 0;
        let root: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
        if !VerQueryValueW(buf.as_ptr().cast(), PCWSTR(root.as_ptr()), &mut info_ptr, &mut info_len).as_bool()
            || info_ptr.is_null()
            || (info_len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>()
        {
            return None;
        }
        let info = &*(info_ptr as *const VS_FIXEDFILEINFO);
        Some(format!(
            "{}.{}.{}.{}",
            info.dwFileVersionMS >> 16,
            info.dwFileVersionMS & 0xffff,
            info.dwFileVersionLS >> 16,
            info.dwFileVersionLS & 0xffff
        ))
    }
}

#[cfg(not(windows))]
pub fn local_vanguard_version() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_handles_riot_and_resource_formats() {
        assert_eq!(parse_version("1.18.4.47"), Some(vec![1, 18, 4, 47]));
        assert_eq!(parse_version("1.18.4-47"), Some(vec![1, 18, 4, 47]));
        assert_eq!(parse_version("1.18.4-47+20260721.170745"), Some(vec![1, 18, 4, 47]));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn inactive_advisory_is_clear() {
        let p = evaluate(&json!({"active": false}), Some("1.18.4.46"));
        assert_eq!(p["stance"], "clear");
        assert_eq!(p["show"], false);
    }

    #[test]
    fn active_without_fix_is_wait_regardless_of_local() {
        let p = evaluate(&json!({"active": true, "id": "x", "title": "t", "message": "m"}), Some("1.18.4.47"));
        assert_eq!(p["stance"], "wait");
        assert_eq!(p["show"], true);
    }

    #[test]
    fn active_with_fix_shows_only_below_fixed_version() {
        let adv = json!({"active": true, "fixed_vanguard": "1.18.4.47"});
        assert_eq!(evaluate(&adv, Some("1.18.4.46"))["stance"], "update");
        assert_eq!(evaluate(&adv, Some("1.18.4.47"))["stance"], "clear");
        assert_eq!(evaluate(&adv, Some("1.18.5.1"))["stance"], "clear");
        // Unknown local version: updating is still the right advice.
        assert_eq!(evaluate(&adv, None)["stance"], "update");
        // Riot's dash format on the wire compares equal to the resource format.
        assert_eq!(evaluate(&json!({"active": true, "fixed_vanguard": "1.18.4-47"}), Some("1.18.4.47"))["stance"], "clear");
    }
}
