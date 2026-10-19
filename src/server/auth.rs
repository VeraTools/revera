use super::{Server, assets};
use axum::{
    Json,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use base64::{Engine, prelude::BASE64_URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{path::Path, sync::Arc};

pub const SESSION: &str = "revera_session";
pub const SETUP: &str = "revera_setup";
const STATE: &str = "revera_oauth";

#[derive(Clone, Serialize, Deserialize)]
pub struct User {
    pub login: String,
    pub avatar_url: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub user: User,
    pub csrf: String,
    pub exp: i64,
    pub kind: String,
}

pub fn key(data: &Path) -> anyhow::Result<[u8; 32]> {
    let path = data.join("session-key");
    crate::fsutil::check_regular_target(&path)?;
    if path.exists() {
        return std::fs::read(path)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("session-key must contain exactly 32 bytes"));
    }
    let mut key = [0; 32];
    getrandom::fill(&mut key).map_err(|e| anyhow::anyhow!("generate session key: {e}"))?;
    crate::fsutil::write_secret_atomic(data, "session-key", &key)?;
    Ok(key)
}

pub fn sign(key: &[u8], payload: &impl Serialize) -> String {
    let encoded = BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(encoded.as_bytes());
    format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
}

pub fn verify<T: serde::de::DeserializeOwned>(key: &[u8], cookie: &str) -> Option<T> {
    let (encoded, signature) = cookie.split_once('.')?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).ok()?;
    mac.update(encoded.as_bytes());
    mac.verify_slice(&hex::decode(signature).ok()?).ok()?;
    serde_json::from_slice(&BASE64_URL_SAFE_NO_PAD.decode(encoded).ok()?).ok()
}

pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .flat_map(|h| h.split(';'))
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

pub fn set_cookie(name: &str, value: &str, seconds: u64) -> String {
    format!("{name}={value}; Path=/; Max-Age={seconds}; HttpOnly; Secure; SameSite=Lax")
}

fn purpose(server: &Server, headers: &HeaderMap, cookie_name: &str, kind: &str) -> Option<Value> {
    let payload: Value = verify(&server.session_key, cookie(headers, cookie_name)?)?;
    (payload["kind"].as_str() == Some(kind)
        && payload["exp"].as_i64()? > chrono::Utc::now().timestamp())
    .then_some(payload)
}

pub fn bootstrap(server: &Server, headers: &HeaderMap) -> bool {
    purpose(server, headers, SETUP, "setup").is_some()
}

pub async fn session(server: &Server, headers: &HeaderMap) -> Option<Session> {
    let session: Session = verify(&server.session_key, cookie(headers, SESSION)?)?;
    if session.kind != "session" || session.exp <= chrono::Utc::now().timestamp() {
        return None;
    }
    let dashboard = server.dashboard.lock().await;
    dashboard
        .preferences
        .admin(&session.user.login)
        .then_some(session)
}

pub async fn guard(
    State(server): State<Arc<Server>>,
    mut request: Request,
    next: Next,
) -> Response {
    if matches!(request.uri().path(), "/api/session" | "/api/setup/manifest") {
        return next.run(request).await;
    }
    if server.setup.lock().await.app.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(session) = session(&server, request.headers()).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    ) && !request
        .headers()
        .get("X-Revera-CSRF")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|value| super::app::token_matches(&session.csrf, value))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    request.extensions_mut().insert(session);
    next.run(request).await
}

pub async fn setup(
    State(server): State<Arc<Server>>,
    Query(query): Query<super::SetupQuery>,
) -> Response {
    let setup = server.setup.lock().await;
    if setup.app.is_some() {
        // Reloading the wizard must work without reopening bootstrap authorization.
        return assets::page("/setup");
    }
    if !setup
        .token
        .as_ref()
        .is_some_and(|t| super::app::token_matches(t, &query.token))
    {
        // The SPA explains where the setup link comes from.
        let mut response = assets::page("/setup");
        *response.status_mut() = StatusCode::UNAUTHORIZED;
        return response;
    }
    let value = sign(
        &server.session_key,
        &json!({
            "kind": "setup", "exp": chrono::Utc::now().timestamp() + 3600,
            "nonce": uuid::Uuid::new_v4().simple().to_string()
        }),
    );
    let mut response = assets::page("/setup");
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(SETUP, &value, 3600).parse().unwrap(),
    );
    response
}

pub async fn login(State(server): State<Arc<Server>>) -> Response {
    let Some(app) = server.setup.lock().await.app.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if app.client_secret.is_empty() {
        return (
            StatusCode::CONFLICT,
            "app.json has no client_secret; add the app's OAuth client secret",
        )
            .into_response();
    }
    let state = uuid::Uuid::new_v4().simple().to_string();
    let mut url = url::Url::parse("https://github.com/login/oauth/authorize").unwrap();
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair(
            "redirect_uri",
            &format!("{}/auth/callback", server.settings.public_url),
        )
        .append_pair("state", &state);
    let value = sign(
        &server.session_key,
        &json!({
            "kind": "oauth", "nonce": state, "exp": chrono::Utc::now().timestamp() + 600
        }),
    );
    let mut response = Redirect::to(url.as_str()).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(STATE, &value, 600).parse().unwrap(),
    );
    response
}

pub async fn callback(
    State(server): State<Arc<Server>>,
    headers: HeaderMap,
    Query(query): Query<super::SetupQuery>,
) -> Response {
    let Some(state) = purpose(&server, &headers, STATE, "oauth") else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !state["nonce"]
        .as_str()
        .is_some_and(|s| super::app::token_matches(s, &query.state))
        || query.code.is_empty()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(app) = server.setup.lock().await.app.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let result: anyhow::Result<User> = async {
        let redirect = format!("{}/auth/callback", server.settings.public_url);
        let token = server.api.oauth_token(&app, &query.code, &redirect).await?;
        let _redaction = crate::redact::ScopedSecret::new(&token);
        let value = server
            .api
            .request(reqwest::Method::GET, "/user", &token, None)
            .await?;
        let user: User = serde_json::from_value(value)?;
        // This lock makes first-admin bootstrap a single atomic decision.
        server.dashboard.lock().await.authorize(
            &user.login,
            bootstrap(&server, &headers),
            &server.settings.data_dir,
        )?;
        Ok(user)
    }
    .await;
    let user = match result {
        Ok(user) => user,
        Err(e) => {
            tracing::warn!(
                "sign-in rejected: {}",
                crate::redact::diagnostic(&format!("{e:#}"))
            );
            return StatusCode::FORBIDDEN.into_response();
        }
    };
    let session = Session {
        user,
        csrf: uuid::Uuid::new_v4().simple().to_string(),
        exp: chrono::Utc::now().timestamp() + 14 * 86400,
        kind: "session".into(),
    };
    let _ = super::reconcile::once(&server).await;
    let redirect = if server.dashboard.lock().await.installations == 0 {
        format!("https://github.com/apps/{}/installations/new", app.slug)
    } else {
        format!("{}/", server.settings.public_url)
    };
    let mut response = Redirect::to(&redirect).into_response();
    for (name, value, seconds) in [
        (SESSION, sign(&server.session_key, &session), 14 * 86400),
        (STATE, String::new(), 0),
        (SETUP, String::new(), 0),
    ] {
        response.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(name, &value, seconds).parse().unwrap(),
        );
    }
    response
}

pub async fn logout() -> Response {
    let mut response = Json(json!({"ok": true})).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(SESSION, "", 0).parse().unwrap(),
    );
    response
}
