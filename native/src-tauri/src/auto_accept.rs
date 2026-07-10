//! Auto-Accept: poll the LCU gameflow phase and accept ready checks. Runs as a
//! tokio task controlled by `AppState::running`; pushes UI updates via events.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;

use crate::{emit_state, lcu, lcu_ws, AppState, LockExt};

/// Exponential backoff: base * 2^min(errors,6), capped.
fn backoff(base: f64, errors: u32, cap: f64) -> f64 {
    let factor = 2f64.powi(errors.min(6) as i32);
    (base * factor).min(cap)
}

/// True while this loop instance is the current one: still toggled on AND not
/// superseded by a newer arm (generation bump).
fn current(state: &AppState, generation: u64) -> bool {
    state.running.load(Ordering::SeqCst)
        && state.auto_accept_gen.load(Ordering::SeqCst) == generation
}

/// Sleep in small chunks so a stop toggle (or a superseding re-arm) takes
/// effect within ~200ms even during a long backoff.
async fn sleep_interruptible(state: &AppState, generation: u64, secs: f64) {
    let mut remaining = secs;
    while remaining > 0.0 && current(state, generation) {
        let chunk = remaining.min(0.2);
        tokio::time::sleep(Duration::from_secs_f64(chunk)).await;
        remaining -= chunk;
    }
}

pub async fn run(app: AppHandle, state: Arc<AppState>, generation: u64) {
    let (check_interval, retry_delay, max_backoff, timeout) = {
        let c = state.config.lock_safe();
        (
            c.auto_accept.check_interval,
            c.auto_accept.retry_delay,
            c.auto_accept.max_backoff,
            c.lcu.request_timeout,
        )
    };
    let client = lcu::build_client(timeout);
    let mut auth: Option<lcu::Auth> = None;
    let mut errors: u32 = 0;
    state.readycheck_handled.store(false, Ordering::SeqCst);

    while current(&state, generation) {
        if auth.is_none() {
            auth = lcu::find_auth();
        }
        match auth.clone() {
            None => {
                state.client_online.store(false, Ordering::SeqCst);
                emit_state(&app, &state);
                errors = errors.saturating_add(1);
                sleep_interruptible(&state, generation, backoff(retry_delay, errors, max_backoff)).await;
            }
            Some(a) => match lcu::get_phase(&client, &a).await {
                Some(phase) => {
                    errors = 0;
                    state.client_online.store(true, Ordering::SeqCst);
                    *state.phase.lock_safe() = phase.clone();

                    // Keep the websocket event task alive: it accepts the
                    // instant a ready check fires, instead of at poll cadence.
                    // The spawn slot prevents duplicates; the task clears it
                    // when its socket drops, and we respawn on the next tick.
                    if !state.ws_active.swap(true, Ordering::SeqCst) {
                        let ws_app = app.clone();
                        let ws_state = state.clone();
                        let ws_auth = a.clone();
                        tauri::async_runtime::spawn(async move {
                            lcu_ws::run(ws_app, ws_state.clone(), ws_auth, generation).await;
                            ws_state.ws_active.store(false, Ordering::SeqCst);
                        });
                    }

                    if phase == "ReadyCheck" {
                        if !state.readycheck_handled.load(Ordering::SeqCst)
                            && lcu::accept_match(&client, &a).await
                            && !state.readycheck_handled.swap(true, Ordering::SeqCst)
                        {
                            state.stats.lock_safe().record_accept();
                        }
                    } else {
                        state.readycheck_handled.store(false, Ordering::SeqCst);
                    }
                    emit_state(&app, &state);
                    sleep_interruptible(&state, generation, check_interval.max(0.2)).await;
                }
                None => {
                    // Request failed — client likely closed; drop cached auth.
                    auth = None;
                    state.client_online.store(false, Ordering::SeqCst);
                    emit_state(&app, &state);
                    errors = errors.saturating_add(1);
                    sleep_interruptible(&state, generation, backoff(retry_delay, errors, max_backoff)).await;
                }
            },
        }
    }

    // Stopped by the user — but only publish "offline" if we are still the
    // current loop; a superseded loop must not clobber its replacement's state.
    if state.auto_accept_gen.load(Ordering::SeqCst) == generation {
        state.client_online.store(false, Ordering::SeqCst);
        emit_state(&app, &state);
    }
}
