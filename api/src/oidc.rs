//! R1 I-3 CP5c: OpenID Connect building blocks for company SSO — discovery and JWKS fetched
//! through the outbound guard of [`crate::providers`], and verification of the ID token returned
//! by the token endpoint.
//!
//! - Every endpoint must sit on the issuer host or on an exact extra host of the IdP settings,
//!   https on port 443 (lab: http and any port, UAT only). Private addresses are refused by the
//!   guard, redirects are never followed, bodies are capped at 1 MB.
//! - Only RS256 and ES256 signatures are accepted (`ring`); `none`, HS* and every other algorithm,
//!   and any `crit` header, are refused before a key is looked up.
//! - Nothing here logs or returns a token, a claim value or a key in an error.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use reqwest::Url;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::providers::{CallError, Target};

/// Allowed clock difference between the IdP and this server, in seconds.
pub const CLOCK_SKEW_SECS: i64 = 60;
/// Path appended to the issuer to find its discovery document.
pub const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";
/// An ID token larger than this is refused before it is parsed.
const MAX_ID_TOKEN: usize = 16 * 1024;
/// A JWKS with more keys than this is refused.
const MAX_KEYS: usize = 50;
/// RS256 keys below 2048 bits are ignored (ring refuses them as well).
const MIN_RSA_MODULUS: usize = 256;

/// Why talking to the IdP (discovery or JWKS) failed. Carries no IdP text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OidcError {
    /// The address is not allowed (host policy or SSRF guard). Nothing was sent.
    #[error("refused")]
    Refused,
    /// DNS or TCP/TLS connection failed.
    #[error("unreachable")]
    Unreachable,
    /// The time budget ran out.
    #[error("timeout")]
    Timeout,
    /// The IdP answered with this HTTP status (3xx included: redirects are not followed).
    #[error("http_{0}")]
    Status(u16),
    /// The answer was not a JSON document of the expected size.
    #[error("protocol")]
    Protocol,
    /// The discovery document is unusable; the label says which rule failed.
    #[error("discovery_{0}")]
    Discovery(&'static str),
    /// The JWKS holds no usable RS256/ES256 signing key.
    #[error("jwks")]
    Jwks,
}

impl From<CallError> for OidcError {
    fn from(e: CallError) -> Self {
        match e {
            CallError::Refused => Self::Refused,
            CallError::Connect => Self::Unreachable,
            CallError::Timeout => Self::Timeout,
            CallError::Status(s) => Self::Status(s),
            CallError::Api(_) | CallError::Protocol => Self::Protocol,
        }
    }
}

/// Where an IdP may make us send requests (and the browser to sign in): the issuer host plus the
/// exact extra hosts of the IdP settings.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    issuer_host: String,
    extra_hosts: Vec<String>,
    lab: bool,
}

/// One allowed endpoint split into the origin to pin and the path (with query) to request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub origin: String,
    pub path_and_query: String,
}

impl Endpoint {
    pub fn url(&self) -> String {
        format!("{}{}", self.origin, self.path_and_query)
    }
}

impl HostPolicy {
    /// `None` when the issuer itself is not an acceptable endpoint (scheme, port, user info).
    pub fn new(issuer: &str, extra_hosts: &[String], lab: bool) -> Option<Self> {
        let url = Url::parse(issuer).ok()?;
        let issuer_host = url.host_str()?.to_ascii_lowercase();
        let policy = Self {
            issuer_host,
            extra_hosts: extra_hosts.iter().map(|h| h.to_ascii_lowercase()).collect(),
            lab,
        };
        policy.endpoint(issuer)?;
        Some(policy)
    }

    pub fn lab(&self) -> bool {
        self.lab
    }

    /// Checks one URL against the policy: https on 443 (lab: http or https, any port), no user
    /// info or fragment, host equal to the issuer host or one of the extra hosts.
    pub fn endpoint(&self, raw: &str) -> Option<Endpoint> {
        let url = Url::parse(raw).ok()?;
        let scheme = url.scheme();
        let scheme_ok = scheme == "https" || (self.lab && scheme == "http");
        if !scheme_ok
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.cannot_be_a_base()
        {
            return None;
        }
        if !self.lab && url.port_or_known_default() != Some(443) {
            return None;
        }
        let host = url.host_str()?.to_ascii_lowercase();
        if host != self.issuer_host && !self.extra_hosts.contains(&host) {
            return None;
        }
        let origin = match url.port() {
            Some(p) => format!("{scheme}://{host}:{p}"),
            None => format!("{scheme}://{host}"),
        };
        let path_and_query = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_owned(),
        };
        Some(Endpoint {
            origin,
            path_and_query,
        })
    }

    /// The discovery document of `issuer` (`<issuer>/.well-known/openid-configuration`).
    pub fn discovery_endpoint(&self, issuer: &str) -> Option<Endpoint> {
        self.endpoint(&format!("{}{DISCOVERY_PATH}", issuer.trim_end_matches('/')))
    }
}

/// The parts of a discovery document the sign-in flow uses, each already checked against the
/// host policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub issuer: String,
    /// Full URL the browser is sent to (may carry its own query).
    pub authorization_endpoint: String,
    pub token_endpoint: Endpoint,
    pub jwks: Endpoint,
}

/// Validates a discovery document: its `issuer` must equal the configured issuer exactly (OIDC
/// Discovery §4.3), endpoints must pass the host policy, and advertised capabilities (when listed)
/// must include the authorization code flow, PKCE S256 and RS256 or ES256.
pub fn parse_discovery(
    doc: &Value,
    issuer: &str,
    policy: &HostPolicy,
) -> Result<Discovery, OidcError> {
    let doc = doc.as_object().ok_or(OidcError::Protocol)?;
    if doc.get("issuer").and_then(Value::as_str) != Some(issuer) {
        return Err(OidcError::Discovery("issuer"));
    }
    let endpoint = |name: &'static str| {
        let raw = doc
            .get(name)
            .and_then(Value::as_str)
            .ok_or(OidcError::Discovery(name))?;
        policy.endpoint(raw).ok_or(OidcError::Discovery(name))
    };
    let authorization_endpoint = endpoint("authorization_endpoint")?.url();
    let token_endpoint = endpoint("token_endpoint")?;
    let jwks = endpoint("jwks_uri")?;
    let lists = |name: &str, wanted: &[&str]| match doc.get(name) {
        None => true,
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .any(|v| wanted.contains(&v)),
        Some(_) => false,
    };
    if !lists("response_types_supported", &["code"]) {
        return Err(OidcError::Discovery("response_types_supported"));
    }
    if !lists("code_challenge_methods_supported", &["S256"]) {
        return Err(OidcError::Discovery("code_challenge_methods_supported"));
    }
    if !lists("id_token_signing_alg_values_supported", &["RS256", "ES256"]) {
        return Err(OidcError::Discovery(
            "id_token_signing_alg_values_supported",
        ));
    }
    Ok(Discovery {
        issuer: issuer.to_owned(),
        authorization_endpoint,
        token_endpoint,
        jwks,
    })
}

/// Fetches and validates the discovery document of `issuer` within `timeout` (DNS included).
pub async fn fetch_discovery(
    issuer: &str,
    policy: &HostPolicy,
    timeout: Duration,
) -> Result<Discovery, OidcError> {
    let ep = policy
        .discovery_endpoint(issuer)
        .ok_or(OidcError::Refused)?;
    let doc = get_json(&ep, policy.lab, timeout).await?;
    parse_discovery(&doc, issuer, policy)
}

/// Fetches the signing keys named by a validated discovery document.
pub async fn fetch_jwks(
    discovery: &Discovery,
    policy: &HostPolicy,
    timeout: Duration,
) -> Result<Jwks, OidcError> {
    let doc = get_json(&discovery.jwks, policy.lab, timeout).await?;
    parse_jwks(&doc)
}

async fn get_json(ep: &Endpoint, lab: bool, timeout: Duration) -> Result<Value, OidcError> {
    let call = async {
        let target = Target::prepare_origin(ep.origin.clone(), lab, timeout).await?;
        target.get_json(&ep.path_and_query).await
    };
    match tokio::time::timeout(timeout, call).await {
        Ok(r) => r.map_err(OidcError::from),
        Err(_) => Err(OidcError::Timeout),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Alg {
    Rs256,
    Es256,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PublicKey {
    /// Modulus and exponent, big-endian without leading zeros.
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// Uncompressed P-256 point `0x04 || x || y`.
    P256(Vec<u8>),
}

impl PublicKey {
    fn alg(&self) -> Alg {
        match self {
            Self::Rsa { .. } => Alg::Rs256,
            Self::P256(_) => Alg::Es256,
        }
    }

    fn verify(&self, message: &[u8], sig: &[u8]) -> bool {
        match self {
            Self::Rsa { n, e } => RsaPublicKeyComponents { n, e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, sig)
                .is_ok(),
            Self::P256(point) => {
                sig.len() == 64
                    && UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                        .verify(message, sig)
                        .is_ok()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Jwk {
    kid: Option<String>,
    key: PublicKey,
}

/// The usable signing keys of an IdP (RSA ≥ 2048 bits and P-256; encryption keys and other
/// types are skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jwks {
    keys: Vec<Jwk>,
}

impl Jwks {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

fn b64(v: Option<&Value>) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(v?.as_str()?).ok()
}

fn strip_zeros(mut v: Vec<u8>) -> Vec<u8> {
    let lead = v.iter().take_while(|b| **b == 0).count();
    v.drain(..lead);
    v
}

fn parse_jwk(k: &Map<String, Value>) -> Option<Jwk> {
    if k.get("use").is_some_and(|u| u.as_str() != Some("sig")) {
        return None;
    }
    let declared = k.get("alg").map(|a| a.as_str().unwrap_or_default());
    let key = match k.get("kty").and_then(Value::as_str)? {
        "RSA" => {
            let n = strip_zeros(b64(k.get("n"))?);
            let e = strip_zeros(b64(k.get("e"))?);
            if n.len() < MIN_RSA_MODULUS || e.is_empty() {
                return None;
            }
            PublicKey::Rsa { n, e }
        }
        "EC" if k.get("crv").and_then(Value::as_str) == Some("P-256") => {
            let x = b64(k.get("x"))?;
            let y = b64(k.get("y"))?;
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            let mut point = Vec::with_capacity(65);
            point.push(0x04);
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            PublicKey::P256(point)
        }
        _ => return None,
    };
    let expected = match key.alg() {
        Alg::Rs256 => "RS256",
        Alg::Es256 => "ES256",
    };
    if declared.is_some_and(|a| a != expected) {
        return None;
    }
    let kid = match k.get("kid") {
        None => None,
        Some(v) => Some(v.as_str()?.to_owned()),
    };
    Some(Jwk { kid, key })
}

/// Parses a JWKS document, keeping only usable signing keys; none usable is an error.
pub fn parse_jwks(doc: &Value) -> Result<Jwks, OidcError> {
    let keys = doc
        .get("keys")
        .and_then(Value::as_array)
        .ok_or(OidcError::Jwks)?;
    if keys.len() > MAX_KEYS {
        return Err(OidcError::Jwks);
    }
    let keys: Vec<Jwk> = keys
        .iter()
        .filter_map(Value::as_object)
        .filter_map(parse_jwk)
        .collect();
    if keys.is_empty() {
        return Err(OidcError::Jwks);
    }
    Ok(Jwks { keys })
}

/// Why an ID token was rejected. `UnknownKey` means "refresh the JWKS once and retry".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed")]
    Malformed,
    #[error("algorithm")]
    Algorithm,
    #[error("unknown_key")]
    UnknownKey,
    #[error("signature")]
    Signature,
    #[error("issuer")]
    Issuer,
    #[error("audience")]
    Audience,
    #[error("expired")]
    Expired,
    #[error("not_yet_valid")]
    NotYetValid,
    #[error("issued_in_future")]
    IssuedInFuture,
    #[error("nonce")]
    Nonce,
    #[error("subject")]
    Subject,
}

/// What the ID token must match.
#[derive(Debug, Clone, Copy)]
pub struct Expected<'a> {
    /// Issuer of the validated discovery document (compared exactly).
    pub issuer: &'a str,
    pub client_id: &'a str,
    /// SHA-256 of the nonce sent in the authorization request.
    pub nonce_hash: &'a [u8; 32],
    /// Current time, Unix seconds.
    pub now: i64,
}

/// A verified ID token: its subject and the remaining claims (read only through the helpers).
#[derive(Debug, Clone)]
pub struct IdToken {
    pub subject: String,
    claims: Map<String, Value>,
}

impl IdToken {
    pub fn email(&self) -> Option<&str> {
        self.claims.get("email").and_then(Value::as_str)
    }

    /// `email_verified` is true only when the IdP says so explicitly (boolean or "true").
    pub fn email_verified(&self) -> bool {
        match self.claims.get("email_verified") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s == "true",
            _ => false,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.claims.get("name").and_then(Value::as_str)
    }

    /// The string values of `claim`: a single string or the strings of an array.
    pub fn strings(&self, claim: &str) -> Vec<String> {
        match self.claims.get(claim) {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        }
    }
}

pub fn sha256(v: &[u8]) -> [u8; 32] {
    Sha256::digest(v).into()
}

fn segment(raw: &str) -> Result<Vec<u8>, TokenError> {
    URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| TokenError::Malformed)
}

fn object(raw: &str) -> Result<Map<String, Value>, TokenError> {
    match serde_json::from_slice(&segment(raw)?) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(TokenError::Malformed),
    }
}

fn seconds(claims: &Map<String, Value>, name: &str) -> Result<Option<i64>, TokenError> {
    match claims.get(name) {
        None => Ok(None),
        Some(v) => match (v.as_i64(), v.as_f64()) {
            (Some(i), _) => Ok(Some(i)),
            (None, Some(f)) if f.is_finite() && f.abs() < 1e15 => Ok(Some(f.floor() as i64)),
            _ => Err(TokenError::Malformed),
        },
    }
}

/// Verifies the compact JWS `token` against `jwks` and the expected values (OIDC Core §3.1.3.7):
/// algorithm, key, signature, then `iss`, `aud`/`azp`, `exp`/`nbf`/`iat` (±60 s), `nonce`, `sub`.
pub fn verify_id_token(
    token: &str,
    jwks: &Jwks,
    expected: &Expected<'_>,
) -> Result<IdToken, TokenError> {
    if token.len() > MAX_ID_TOKEN {
        return Err(TokenError::Malformed);
    }
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let header = object(h)?;
    let alg = match header.get("alg").and_then(Value::as_str) {
        Some("RS256") => Alg::Rs256,
        Some("ES256") => Alg::Es256,
        Some(_) => return Err(TokenError::Algorithm),
        None => return Err(TokenError::Malformed),
    };
    if header.contains_key("crit") {
        return Err(TokenError::Algorithm);
    }
    let kid = match header.get("kid") {
        None => None,
        Some(Value::String(k)) => Some(k.as_str()),
        Some(_) => return Err(TokenError::Malformed),
    };
    let candidates: Vec<&PublicKey> = jwks
        .keys
        .iter()
        .filter(|k| k.key.alg() == alg && (kid.is_none() || k.kid.as_deref() == kid))
        .map(|k| &k.key)
        .collect();
    if candidates.is_empty() {
        return Err(TokenError::UnknownKey);
    }
    let sig = segment(s)?;
    let signed = &token.as_bytes()[..h.len() + 1 + p.len()];
    if !candidates.iter().any(|k| k.verify(signed, &sig)) {
        return Err(TokenError::Signature);
    }

    let claims = object(p)?;
    if claims.get("iss").and_then(Value::as_str) != Some(expected.issuer) {
        return Err(TokenError::Issuer);
    }
    let audiences: Vec<&str> = match claims.get("aud") {
        Some(Value::String(a)) => vec![a.as_str()],
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().ok_or(TokenError::Audience))
            .collect::<Result<_, _>>()?,
        _ => return Err(TokenError::Audience),
    };
    if !audiences.contains(&expected.client_id) {
        return Err(TokenError::Audience);
    }
    match claims.get("azp") {
        Some(v) if v.as_str() != Some(expected.client_id) => return Err(TokenError::Audience),
        None if audiences.len() > 1 => return Err(TokenError::Audience),
        _ => {}
    }
    let now = expected.now;
    let exp = seconds(&claims, "exp")?.ok_or(TokenError::Malformed)?;
    if now - CLOCK_SKEW_SECS >= exp {
        return Err(TokenError::Expired);
    }
    if seconds(&claims, "nbf")?.is_some_and(|nbf| nbf > now + CLOCK_SKEW_SECS) {
        return Err(TokenError::NotYetValid);
    }
    let iat = seconds(&claims, "iat")?.ok_or(TokenError::Malformed)?;
    if iat > now + CLOCK_SKEW_SECS {
        return Err(TokenError::IssuedInFuture);
    }
    let nonce = claims
        .get("nonce")
        .and_then(Value::as_str)
        .ok_or(TokenError::Nonce)?;
    if !bool::from(sha256(nonce.as_bytes()).ct_eq(expected.nonce_hash)) {
        return Err(TokenError::Nonce);
    }
    let subject = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && s.len() <= 255)
        .ok_or(TokenError::Subject)?
        .to_owned();
    Ok(IdToken { subject, claims })
}
