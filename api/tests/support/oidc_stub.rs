//! Shared by the CP5c OIDC test binaries: a local stub IdP (axum on 127.0.0.1, ITSM_OIDC_LAB=on)
//! with a per-run ES256 key, and the app configuration those tests use. Everything is synthetic.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use itsm_api::config::Config;
use itsm_api::{AppState, oidc};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};
use sqlx::PgPool;

pub const CLIENT: &str = "client-alfa";
pub const SECRET: &str = "synthetic-client-secret";
pub const PUBLIC_URL: &str = "https://itsm.example.test";
pub const DOMAIN: &str = "alfa.example.test";

pub fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

pub fn b64(v: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(v)
}

pub fn now() -> i64 {
    let d = ok(
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH),
        "clock",
    );
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

pub struct Key {
    pub pair: EcdsaKeyPair,
    pub kid: String,
}

impl Key {
    pub fn new(kid: &str) -> Self {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let der = ok(EcdsaKeyPair::generate_pkcs8(alg, &rng), "ec gen");
        Self {
            pair: ok(EcdsaKeyPair::from_pkcs8(alg, der.as_ref(), &rng), "ec key"),
            kid: kid.to_owned(),
        }
    }

    pub fn jwk(&self) -> Value {
        let p = self.pair.public_key().as_ref();
        json!({"kty": "EC", "kid": self.kid, "crv": "P-256", "use": "sig",
               "x": b64(&p[1..33]), "y": b64(&p[33..65])})
    }

    pub fn sign(&self, claims: &Value) -> String {
        let header = json!({"alg": "ES256", "kid": self.kid, "typ": "JWT"});
        let input = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(claims.to_string().as_bytes())
        );
        let sig = ok(
            self.pair.sign(&SystemRandom::new(), input.as_bytes()),
            "sign",
        );
        format!("{input}.{}", b64(sig.as_ref()))
    }
}

/// What the "user" does at the IdP for one authorization: who signs in and how the IdP answers.
#[derive(Clone)]
pub struct Grant {
    pub challenge: String,
    pub nonce: String,
    pub claims: Value,
    /// Token endpoint answers with this status instead of a token.
    pub status: Option<u16>,
    /// Signs with this key instead of the published one.
    pub rogue: bool,
}

pub struct Idp {
    pub issuer: String,
    pub published: Key,
    pub rogue: Key,
    pub grants: HashMap<String, Grant>,
    pub token_calls: usize,
    pub jwks_calls: usize,
    /// Discovery answers 503 (an IdP outage).
    pub down: bool,
}

pub type Shared = Arc<Mutex<Idp>>;

pub fn lock(s: &Shared) -> std::sync::MutexGuard<'_, Idp> {
    s.lock().unwrap_or_else(|p| p.into_inner())
}

pub async fn discovery(State(s): State<Shared>) -> Response {
    let (iss, down) = {
        let idp = lock(&s);
        (idp.issuer.clone(), idp.down)
    };
    if down {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    axum::Json(json!({
        "issuer": iss,
        "authorization_endpoint": format!("{iss}/authorize?prompt=login"),
        "token_endpoint": format!("{iss}/token"),
        "jwks_uri": format!("{iss}/jwks"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "id_token_signing_alg_values_supported": ["ES256"],
    }))
    .into_response()
}

pub async fn jwks(State(s): State<Shared>) -> Response {
    let mut idp = lock(&s);
    idp.jwks_calls += 1;
    axum::Json(json!({"keys": [idp.published.jwk()]})).into_response()
}

pub fn form(body: &str) -> HashMap<String, String> {
    // `reqwest::Url` (the url crate) decodes application/x-www-form-urlencoded pairs.
    let u = ok(
        reqwest::Url::parse(&format!("http://form.invalid/?{body}")),
        "form",
    );
    u.query_pairs().into_owned().collect()
}

pub async fn token(State(s): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut idp = lock(&s);
    idp.token_calls += 1;
    let basic = format!("Basic {}", STANDARD.encode(format!("{CLIENT}:{SECRET}")));
    let f = form(&body);
    let auth_ok = headers
        .get(header::AUTHORIZATION)
        .is_some_and(|v| v.as_bytes() == basic.as_bytes());
    let redirect_ok = f.get("redirect_uri").map(String::as_str)
        == Some(&format!("{PUBLIC_URL}/api/v1/auth/oidc/callback"))
        && f.get("grant_type").map(String::as_str) == Some("authorization_code");
    // A code is single use at the IdP too.
    let grant = f.get("code").and_then(|c| idp.grants.remove(c));
    let Some(g) = grant.filter(|_| auth_ok && redirect_ok) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    let verifier = f.get("code_verifier").cloned().unwrap_or_default();
    if b64(&oidc::sha256(verifier.as_bytes())) != g.challenge {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    if let Some(code) = g.status {
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY);
        return (status, axum::Json(json!({"error": "server_error"}))).into_response();
    }
    let t = now();
    let mut claims = json!({"iss": idp.issuer, "aud": CLIENT, "iat": t, "exp": t + 300,
                            "nonce": g.nonce});
    if let (Some(c), Some(extra)) = (claims.as_object_mut(), g.claims.as_object()) {
        c.extend(extra.clone());
    }
    let key = if g.rogue { &idp.rogue } else { &idp.published };
    let id_token = key.sign(&claims);
    axum::Json(json!({"access_token": "synthetic", "token_type": "Bearer", "id_token": id_token}))
        .into_response()
}

pub async fn stub() -> (Shared, String) {
    let l = ok(tokio::net::TcpListener::bind("127.0.0.1:0").await, "bind");
    let base = format!("http://{}", ok(l.local_addr(), "addr"));
    let shared = Arc::new(Mutex::new(Idp {
        issuer: base.clone(),
        published: Key::new("k-pub"),
        rogue: Key::new("k-rogue"),
        grants: HashMap::new(),
        token_calls: 0,
        jwks_calls: 0,
        down: false,
    }));
    let router = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .with_state(shared.clone());
    tokio::spawn(async move {
        let _ = axum::serve(l, router).await;
    });
    (shared, base)
}

pub fn state(pool: &PgPool, auth: &str, keyring: bool) -> AppState {
    let mut vars: Vec<(&str, String)> = vec![
        ("ITSM_ENV", "uat".into()),
        ("ITSM_ACCESS_MODE", "password".into()),
        ("ITSM_COOKIE_SECURE", "true".into()),
        ("DATABASE_URL", "postgres://unused-in-tests".into()),
        ("ITSM_AUTH", auth.into()),
        ("ITSM_PUBLIC_URL", PUBLIC_URL.into()),
        ("ITSM_OIDC_LAB", "on".into()),
        ("ITSM_INTEGRATIONS_TIMEOUT_MS", "2000".into()),
    ];
    if keyring {
        vars.push(("ITSM_KEYRING", format!("k1:{}", STANDARD.encode([9u8; 32]))));
        vars.push(("ITSM_KEYRING_ACTIVE", "k1".into()));
    }
    let vars: HashMap<String, String> = vars.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
    AppState {
        config: Arc::new(ok(Config::from_map(&vars), "test config")),
        pool: pool.clone(),
    }
}
