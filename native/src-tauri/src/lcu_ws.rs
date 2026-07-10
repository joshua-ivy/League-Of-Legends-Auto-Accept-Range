//! LCU WebSocket event feed. The client exposes a WAMP-style event socket on
//! the same loopback port as the REST API; subscribing to the gameflow-phase
//! event delivers ready-check notification the instant it happens, instead of
//! waiting for the next poll tick. The polling loop in `auto_accept` stays as
//! the fallback (and covers the state already in effect at connect time).
//!
//! Lifecycle: `auto_accept::run` owns a spawn slot (`AppState::ws_active`).
//! This task clears the slot when it returns, so the poller respawns it on its
//! next tick — connection drops and client restarts self-heal at poll cadence.
//! It also carries the poller's `generation`: a superseded task (rapid
//! off→on toggle bumped `auto_accept_gen` past it) exits on its next loop
//! check instead of running on — and possibly racing its replacement — with
//! stale auth.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tauri::AppHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::Connector;

use crate::{emit_state, lcu, AppState, LockExt};

/// WAMP subscribe opcode 5; event frames arrive as opcode 8.
const PHASE_EVENT: &str = "OnJsonApiEvent_lol-gameflow_v1_gameflow-phase";

pub async fn run(app: AppHandle, state: Arc<AppState>, auth: lcu::Auth, generation: u64) {
    stream_events(&app, &state, &auth, generation).await;
}

async fn stream_events(app: &AppHandle, state: &Arc<AppState>, auth: &lcu::Auth, generation: u64) -> Option<()> {
    let url = auth.base_url.replacen("https", "wss", 1);
    let mut request = url.into_client_request().ok()?;
    request
        .headers_mut()
        .insert(AUTHORIZATION, auth.header.parse().ok()?);

    // Same self-signed-cert situation as the REST client: scoped to loopback.
    let tls = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .ok()?;

    let (mut ws, _) = tokio_tungstenite::connect_async_tls_with_config(
        request,
        None,
        false,
        Some(Connector::NativeTls(tls)),
    )
    .await
    .ok()?;

    ws.send(Message::Text(format!("[5, \"{PHASE_EVENT}\"]")))
        .await
        .ok()?;

    let timeout = state.config.lock_safe().lcu.request_timeout;
    let client = lcu::build_client(timeout);

    while state.running.load(Ordering::SeqCst) && state.auto_accept_gen.load(Ordering::SeqCst) == generation {
        // Bounded wait so a "stop" toggle is honored within ~1s even when the
        // socket is silent (which is most of the time).
        let msg = match tokio::time::timeout(Duration::from_secs(1), ws.next()).await {
            Err(_) => continue,           // no event yet — re-check running flag
            Ok(None) => break,            // socket closed (client shut down)
            Ok(Some(Err(_))) => break,    // socket error — poller respawns us
            Ok(Some(Ok(m))) => m,
        };
        let Message::Text(text) = msg else { continue };
        // Event frame: [8, "<event>", {"data": "<phase>", ...}]
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(phase) = value
            .get(2)
            .and_then(|p| p.get("data"))
            .and_then(|d| d.as_str())
        else {
            continue;
        };

        *state.phase.lock_safe() = phase.to_string();
        if phase == "ReadyCheck" {
            if !state.readycheck_handled.load(Ordering::SeqCst)
                && lcu::accept_match(&client, auth).await
                && !state.readycheck_handled.swap(true, Ordering::SeqCst)
            {
                state.stats.lock_safe().record_accept();
            }
        } else {
            state.readycheck_handled.store(false, Ordering::SeqCst);
        }
        emit_state(app, state);
    }
    Some(())
}
