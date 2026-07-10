//! Camera Assist (M3): recenter the camera on the player when they drift from
//! screen center or the anchor is lost. Mirrors Auto-Range's structure — a
//! recenter thread + an async ranked kill-switch — and reuses the chat-aware
//! release. Detection accuracy depends on `vision::find_player_anchor`, which is
//! still a stub pending live validation (see vision.rs).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::AppHandle;

use crate::{emit_state, input::Injector, lcu, safety, vision, winutil, AppState, LockExt};

pub fn start(app: AppHandle, state: Arc<AppState>, generation: u64) {
    if !state.chat_listener_started.swap(true, Ordering::SeqCst) {
        crate::auto_range::start_chat_listener(state.clone());
    }
    {
        let state = state.clone();
        tauri::async_runtime::spawn(async move { ranked_monitor(state, generation).await });
    }
    std::thread::spawn(move || recenter_loop(app, state, generation));
}

type CamParams = (String, f64, f64, f64, f64, f64, f64);

fn read_params(state: &AppState) -> CamParams {
    let c = state.config.lock_safe();
    (
        c.camera.camera_hold_key.clone(),
        c.camera.recenter_hold_sec,
        c.camera.recenter_cooldown_sec,
        c.camera.lost_recenter_sec,
        c.camera.center_radius_px as f64,
        c.camera.vision_interval,
        c.camera.tick_sec,
    )
}

fn recenter_loop(app: AppHandle, state: Arc<AppState>, generation: u64) {
    let (mut key, mut hold_sec, mut cooldown_sec, mut lost_sec, mut radius, mut vis_interval, mut tick) =
        read_params(&state);
    let mut cfg_seen = state.config_gen.load(Ordering::SeqCst);
    let mut injector = match Injector::new(&key) {
        Some(i) => i,
        None => {
            state.camera_running.store(false, Ordering::SeqCst);
            emit_state(&app, &state);
            return;
        }
    };
    let mut last_seen = Instant::now() - Duration::from_secs(10);
    let mut next_recenter = Instant::now();
    let mut tracked: Option<(i32, i32)> = None;
    let mut capturer = vision::Capturer::new();

    while state.camera_running.load(Ordering::SeqCst)
        && state.camera_gen.load(Ordering::SeqCst) == generation
    {
        // Live-reload config when it changes; rebuild injector only on key change.
        let cfg_now = state.config_gen.load(Ordering::SeqCst);
        if cfg_now != cfg_seen {
            cfg_seen = cfg_now;
            let (k, h, c, l, r, v, t) = read_params(&state);
            hold_sec = h; cooldown_sec = c; lost_sec = l; radius = r; vis_interval = v; tick = t;
            if k != key {
                injector.release();
                if let Some(i) = Injector::new(&k) {
                    injector = i;
                    key = k;
                }
            }
        }

        let focused = winutil::lol_game_focused();
        state.game_focused.store(focused, Ordering::SeqCst); // publish for the chat hook
        if !focused {
            state.chat_open.store(false, Ordering::SeqCst);
        }
        let gated = !focused
            || state.injection_blocked.load(Ordering::SeqCst)
            || state.chat_open.load(Ordering::SeqCst);

        if gated {
            injector.release();
            // Idle back-off when not in-game; responsive tick when gated mid-game.
            let sleep = if focused { tick.max(0.01) } else { 0.25 };
            std::thread::sleep(Duration::from_secs_f64(sleep));
            continue;
        }

        if let Some(frame) = capturer.capture() {
            let (w, h) = (frame.width() as i32, frame.height() as i32);
            let center = vision::expected_player_anchor(w, h);
            let tracking_anchor = match tracked {
                Some(a) if last_seen.elapsed().as_secs_f64() <= vision::TARGET_TRACK_SEC => a,
                _ => center,
            };
            let candidates = vision::detect_player_candidates(&frame);
            let should_recenter = match vision::choose_player(&candidates, w, h, tracking_anchor) {
                Some(c) => {
                    tracked = Some(c.player_anchor);
                    last_seen = Instant::now();
                    vision::distance(c.player_anchor, center) > radius
                }
                None => last_seen.elapsed().as_secs_f64() > lost_sec,
            };
            if should_recenter && Instant::now() >= next_recenter {
                // Pulse the center-camera key.
                injector.press();
                std::thread::sleep(Duration::from_secs_f64(hold_sec.max(0.03)));
                injector.release();
                next_recenter = Instant::now() + Duration::from_secs_f64(cooldown_sec.max(0.05));
            }
        }
        std::thread::sleep(Duration::from_secs_f64(vis_interval.max(0.02)));
    }
    // injector Drop releases the key.
    state.game_focused.store(false, Ordering::SeqCst);
    emit_state(&app, &state);
}

async fn ranked_monitor(state: Arc<AppState>, generation: u64) {
    let (block_enabled, timeout, interval) = {
        let c = state.config.lock_safe();
        (c.safety.block_in_ranked, c.lcu.request_timeout, c.safety.check_interval)
    };
    if !block_enabled {
        return;
    }
    let client = lcu::build_client(timeout);
    while state.camera_running.load(Ordering::SeqCst)
        && state.camera_gen.load(Ordering::SeqCst) == generation
    {
        let block = match lcu::cached_auth() {
            Some(auth) => match lcu::gameflow_session(&client, &auth).await {
                Some(session) => safety::should_block(&session),
                None => {
                    lcu::invalidate_auth();
                    false
                }
            },
            None => false,
        };
        state.injection_blocked.store(block, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs_f64(interval.max(1.0))).await;
    }
    // Only clear if Auto-Range isn't still relying on the kill-switch.
    if !state.auto_range_running.load(Ordering::SeqCst) {
        state.injection_blocked.store(false, Ordering::SeqCst);
    }
}
