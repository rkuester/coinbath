//! The HTTP JSON API.
//!
//! `GET /api/v0/state` returns the snapshot. `PUT /api/v0/setpoint`
//! takes a bare JSON number in degrees Celsius, and
//! `PUT /api/v0/power_fraction` a bare JSON number from 0.0 to 1.0,
//! which also puts the state in manual mode. `PUT /api/v0/mode`
//! takes `"auto"` or `"manual"`. Each returns the snapshot after
//! the change, or 400 with the reason it was refused.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::extract::State as Shared;
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};

use crate::state::{Client, Mode, Rejected, State};

/// Builds the router over a state client.
pub fn router(client: Client) -> Router {
    Router::new()
        .route("/api/v0/state", get(get_state))
        .route("/api/v0/setpoint", put(put_setpoint))
        .route("/api/v0/power_fraction", put(put_power_fraction))
        .route("/api/v0/mode", put(put_mode))
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
    reply(&client, "setpoint", client.set_setpoint(celsius).await)
}

async fn put_power_fraction(
    Shared(client): Shared<Client>,
    Json(fraction): Json<f64>,
) -> Result<Json<State>, (StatusCode, String)> {
    reply(
        &client,
        "power fraction",
        client.set_power_fraction_by_hand(fraction).await,
    )
}

async fn put_mode(
    Shared(client): Shared<Client>,
    Json(mode): Json<Mode>,
) -> Result<Json<State>, (StatusCode, String)> {
    reply(&client, "mode", client.set_mode(mode).await)
}

/// Answers a change with the snapshot after it, or with the reason
/// the state task refused it.
fn reply(
    client: &Client,
    what: &str,
    result: Result<()>,
) -> Result<Json<State>, (StatusCode, String)> {
    match result {
        Ok(()) => Ok(Json(client.state())),
        Err(e) if e.downcast_ref::<Rejected>().is_some() => {
            Err((StatusCode::BAD_REQUEST, e.to_string()))
        }
        Err(e) => {
            tracing::error!("{what} change failed: {e:#}");
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
        put_number(&format!("{base}/api/v0/setpoint"), celsius)
    }

    fn put_number(url: &str, value: f64) -> (u16, String) {
        let mut response = agent().put(url).send_json(value).unwrap();
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
    async fn power_fraction_is_set_and_refused_the_same_way() {
        let (client, _task) = state::spawn(State::new(50.0, &["bath"]));
        let base = serve_ephemeral(client.clone()).await;
        let url = format!("{base}/api/v0/power_fraction");

        let (status, body) = tokio::task::spawn_blocking({
            let url = url.clone();
            move || put_number(&url, 0.3)
        })
        .await
        .unwrap();
        assert_eq!(status, 200);
        let after: State = serde_json::from_str(&body).unwrap();
        assert_eq!(after.power_fraction, Some(0.3));
        assert_eq!(after.mode, Mode::Manual);

        let (status, body) = tokio::task::spawn_blocking(move || put_number(&url, 2.0))
            .await
            .unwrap();
        assert_eq!(status, 400);
        assert!(body.contains("2"), "body names the value: {body}");
        assert_eq!(client.state().power_fraction, Some(0.3));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn mode_is_a_json_string() {
        let (client, _task) = state::spawn(State::new(50.0, &["bath"]));
        let base = serve_ephemeral(client.clone()).await;

        let (status, body) = tokio::task::spawn_blocking(move || {
            let mut response = agent()
                .put(format!("{base}/api/v0/mode"))
                .send_json("manual")
                .unwrap();
            let status = response.status().as_u16();
            (status, response.body_mut().read_to_string().unwrap())
        })
        .await
        .unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("\"mode\":\"manual\""), "{body}");
        assert_eq!(client.state().mode, Mode::Manual);
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
