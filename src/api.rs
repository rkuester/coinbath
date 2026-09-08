//! The HTTP JSON API.
//!
//! `GET /api/v0/state` returns the snapshot. `PUT /api/v0/setpoint`
//! takes a bare JSON number in degrees Celsius and returns the
//! snapshot after the change, or 400 with the reason it was refused.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::extract::State as Shared;
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};

use crate::state::{Client, Rejected, State};

/// Builds the router over a state client.
pub fn router(client: Client) -> Router {
    Router::new()
        .route("/api/v0/state", get(get_state))
        .route("/api/v0/setpoint", put(put_setpoint))
        .with_state(client)
}

/// Serves the API on `listen` until the task is cancelled.
pub async fn serve(client: Client, listen: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind API listener on {listen}"))?;
    tracing::info!(%listen, "API listening");
    axum::serve(listener, router(client))
        .await
        .context("serve API")
}

async fn get_state(Shared(client): Shared<Client>) -> Json<State> {
    Json(client.state())
}

async fn put_setpoint(
    Shared(client): Shared<Client>,
    Json(celsius): Json<f64>,
) -> Result<Json<State>, (StatusCode, String)> {
    match client.set_setpoint(celsius).await {
        Ok(()) => Ok(Json(client.state())),
        Err(e) if e.downcast_ref::<Rejected>().is_some() => {
            Err((StatusCode::BAD_REQUEST, e.to_string()))
        }
        Err(e) => {
            tracing::error!("setpoint change failed: {e:#}");
            Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state;

    /// Serves the router on an ephemeral port and returns its base URL.
    async fn serve_ephemeral(client: Client) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(client)).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into()
    }

    fn get_state(base: &str) -> State {
        agent()
            .get(format!("{base}/api/v0/state"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap()
    }

    fn put_setpoint(base: &str, celsius: f64) -> (u16, String) {
        let mut response = agent()
            .put(format!("{base}/api/v0/setpoint"))
            .send_json(celsius)
            .unwrap();
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().unwrap();
        (status, body)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn state_round_trips_and_setpoint_changes() {
        let (client, _task) = state::spawn(State::new(50.0, &["bath"]));
        let base = serve_ephemeral(client.clone()).await;

        let before = tokio::task::spawn_blocking({
            let base = base.clone();
            move || get_state(&base)
        })
        .await
        .unwrap();
        assert_eq!(before.setpoint_c, 50.0);
        assert_eq!(before.probes[0].name, "bath");

        let (status, body) = tokio::task::spawn_blocking({
            let base = base.clone();
            move || put_setpoint(&base, 60.0)
        })
        .await
        .unwrap();
        assert_eq!(status, 200);
        let after: State = serde_json::from_str(&body).unwrap();
        assert_eq!(after.setpoint_c, 60.0);
        assert_eq!(client.state().setpoint_c, 60.0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_setpoint_is_a_400_and_changes_nothing() {
        let (client, _task) = state::spawn(State::new(50.0, &["bath"]));
        let base = serve_ephemeral(client.clone()).await;

        let (status, body) = tokio::task::spawn_blocking(move || put_setpoint(&base, 150.0))
            .await
            .unwrap();
        assert_eq!(status, 400);
        assert!(body.contains("150"), "body names the value: {body}");
        assert_eq!(client.state().setpoint_c, 50.0);
    }
}
