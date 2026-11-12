//! CP5c: OIDC discovery, JWKS and ID token verification vectors. No database and no real IdP:
//! tokens are signed in-process, and discovery/JWKS come from a local stub on 127.0.0.1 (lab).
//!
//! SYNTHETIC TEST KEY: `RSA_TEST_PKCS8` below is an RSA-2048 key generated only for these tests
//! (openssl genpkey, 2026-11-13). It signs nothing outside this file and is not a secret.

use std::fmt::Display;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use itsm_api::oidc::{
    self, Expected, HostPolicy, Jwks, OidcError, TokenError, parse_discovery, parse_jwks,
    verify_id_token,
};
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair, RsaKeyPair, RsaPublicKeyComponents};
use serde_json::{Value, json};

const RSA_TEST_PKCS8: &str = "MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCs6eBki68VvmFBxm+3kHICpfzdYGtOIJN+N/i5wFaSQEUT/wFN2KKsrQlcZXEMZDNGRnl4h5jcvy4Tlk7+mimM6IsmYr6DWRTDKXKmBYbm+BYeTPE9b1dWuPHbfav8LED3kRPtWUAdDJuWpTT9Q9cqFIjShFCDm6wlDe7quceu522B5Us0oOlGryW8HWrvnN/5+d4N7didTwzsU8PusHzj2QU1nU6O2msTTZVIgj7SU7r2XrJ3/Hq4e3cvs92s0b1muerxVM7iuKKgrw9Kh1oONOsQjxG67Qb4YFkOM7rVUmPCRXgvkQmCnX0UJNeUYobvywpmjp7dbkWFDtAV/jVHAgMBAAECggEACHC2GlJTLo2Hc8164Iv1dLmjeS7ergabDHsw8eg4kZT/ijVVW/ZqImGj5lWxSfiOJC0GeuKrjILsfdpFatodkxo6yJnjUMgFYm1ggkgcyKNtePcqg2Qu8DXA4ITm4z1JwovbXCeSIJjf81+HEaHBG/W6pAP/Zh0wLvLaKaz92USwIf61j0pY7OMoeaiTfE4A7tb7E275Bv9R7vzwEz1J22nuN2Q0XAB1Syy+G0uEHJ5YoQb3smHYUFICxy1CSycvvkw0ISUjWFTRpx45MFcRH1X3slTKgujmyf/gJOqpm4tP8quRDxxZIPUTPXzR8Yh7iVLrZhXyGJellputpWmMyQKBgQDVWU4hg8UUpvqMuNOaYHa3Eoblh9UzGHeM7Ez5ULokhoe3MGh8xJjvRiYRywidyQls0v3xiHdaPaZM6xaE5RbZ/qmXQ3fHQACaDKBM0Md3t/qa0ajUETAA3cfDF1TgmKsKeGP95bAPP4b/fS8HvrDjszNyZGo+BTw7dqNtWUCq0wKBgQDPezBdyYOLs8j5niHzYSzxYRgRstOMUDzRM2myg+zQgYGCaTQzAQHri7Dk4KgC0Op+1/JXWspK9bmg7hrJ0pM0ReW4KaQDRtZIw3E+8wwH6pZCdR4H6QFPLLViH/BWBOH3vVDQa9tbTtXQh9Sloh+qnU5QJT7aHo7alMKXtOHbPQKBgEyM9Z7K4/0MCNxTOQGsWQxJJoPXICVsQwEajGfskk2mw610Phl+hQ1dky1RN28+Aha9AgcXGxQTCMeSlorqlC91064FtEWW7mBX5XCKfPe02MOSNMyauimlahbTGU5ZGQ3xspcYQfrGpY6ogSZ7QocA9pW19/T29ey0HeGPY8enAoGAcI8iRB8osEeOxmzVelXDrgfAqQlc3whAljXQJ7qOZOd9PSSZ3vmy8vhLtT9zpmN0c+/vVxi4GkaIuBe0jL0igv9QisJ8ahKp+bud2kq8wiav8cnbyurLfQqCNIOjPOdVN7OK/iclT7nLIWAvKtRzXXoNnEk4ediOI2ME1oyY4vkCgYAYakFIf/BkAeth6+Mlajd3asHkX9U8vZZBFF0QrNA0EXGAswI8MijSdSu1OCTpUAkaST4FmsijnGVxs5BHcX37/+2afKmtAIus6IZXOZkBC/WZBD+27YjN9khF27jUMQFm98+W6zQoxlCUtdQPZ//0j5HnZ3BSVmLytDPviwnUGg==";

const ISSUER: &str = "https://login.example.test/tenant-a/v2.0";
const CLIENT: &str = "client-a";
const NONCE: &str = "n-0S6_WzA2Mj";
const NOW: i64 = 1_800_000_000;

fn ok<T, E: Display>(r: Result<T, E>, what: &str) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("{what}: {e}"),
    }
}

fn b64(v: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(v)
}

enum Signer {
    Rsa(RsaKeyPair),
    Ec(EcdsaKeyPair),
}

impl Signer {
    fn rsa() -> Self {
        let der = ok(STANDARD.decode(RSA_TEST_PKCS8), "key b64");
        Self::Rsa(ok(RsaKeyPair::from_pkcs8(&der), "rsa key"))
    }

    fn ec() -> Self {
        let rng = SystemRandom::new();
        let alg = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let der = ok(EcdsaKeyPair::generate_pkcs8(alg, &rng), "ec gen");
        Self::Ec(ok(
            EcdsaKeyPair::from_pkcs8(alg, der.as_ref(), &rng),
            "ec key",
        ))
    }

    fn alg(&self) -> &'static str {
        match self {
            Self::Rsa(_) => "RS256",
            Self::Ec(_) => "ES256",
        }
    }

    fn jwk(&self, kid: &str) -> Value {
        match self {
            Self::Rsa(k) => {
                let c = RsaPublicKeyComponents::<Vec<u8>>::from(k.public());
                json!({"kty": "RSA", "kid": kid, "use": "sig", "alg": "RS256",
                       "n": b64(&c.n), "e": b64(&c.e)})
            }
            Self::Ec(k) => {
                let p = k.public_key().as_ref();
                json!({"kty": "EC", "kid": kid, "crv": "P-256",
                       "x": b64(&p[1..33]), "y": b64(&p[33..65])})
            }
        }
    }

    fn sign(&self, header: &Value, claims: &Value) -> String {
        let input = format!(
            "{}.{}",
            b64(header.to_string().as_bytes()),
            b64(claims.to_string().as_bytes())
        );
        let rng = SystemRandom::new();
        let sig = match self {
            Self::Rsa(k) => {
                let mut sig = vec![0; k.public().modulus_len()];
                ok(
                    k.sign(
                        &signature::RSA_PKCS1_SHA256,
                        &rng,
                        input.as_bytes(),
                        &mut sig,
                    ),
                    "rsa sign",
                );
                sig
            }
            Self::Ec(k) => ok(k.sign(&rng, input.as_bytes()), "ec sign")
                .as_ref()
                .to_vec(),
        };
        format!("{input}.{}", b64(&sig))
    }

    fn token(&self, kid: &str, claims: &Value) -> String {
        self.sign(
            &json!({"alg": self.alg(), "kid": kid, "typ": "JWT"}),
            claims,
        )
    }
}

fn claims() -> Value {
    json!({
        "iss": ISSUER, "aud": CLIENT, "sub": "248289761001", "nonce": NONCE,
        "iat": NOW - 5, "exp": NOW + 300, "email": "budi@alfa.example.test",
        "email_verified": true, "name": "Budi", "groups": ["itsm-cs", "other"],
    })
}

fn with(mut base: Value, changes: Value) -> Value {
    for (k, v) in ok(changes.as_object().ok_or("object"), "changes") {
        if v.is_null() {
            base.as_object_mut().map(|m| m.remove(k));
        } else {
            base[k] = v.clone();
        }
    }
    base
}

fn jwks(keys: &[Value]) -> Jwks {
    ok(parse_jwks(&json!({ "keys": keys })), "jwks")
}

fn verify(token: &str, keys: &Jwks) -> Result<oidc::IdToken, TokenError> {
    let nonce_hash = oidc::sha256(NONCE.as_bytes());
    verify_id_token(
        token,
        keys,
        &Expected {
            issuer: ISSUER,
            client_id: CLIENT,
            nonce_hash: &nonce_hash,
            now: NOW,
        },
    )
}

#[test]
fn valid_rs256_and_es256_tokens_verify() {
    let rsa = Signer::rsa();
    let ec = Signer::ec();
    let keys = jwks(&[rsa.jwk("k1"), ec.jwk("e1")]);
    assert_eq!(keys.len(), 2);

    let t = ok(verify(&rsa.token("k1", &claims()), &keys), "rs256");
    assert_eq!(t.subject, "248289761001");
    assert_eq!(t.email(), Some("budi@alfa.example.test"));
    assert!(t.email_verified());
    assert_eq!(t.name(), Some("Budi"));
    assert_eq!(t.strings("groups"), vec!["itsm-cs", "other"]);
    assert!(t.strings("roles").is_empty());

    let t = ok(verify(&ec.token("e1", &claims()), &keys), "es256");
    assert_eq!(t.subject, "248289761001");

    // No kid in the header: any key of the right type may verify it.
    let no_kid = rsa.sign(&json!({"alg": "RS256"}), &claims());
    ok(verify(&no_kid, &keys), "no kid");
    // email_verified missing or false is not verified.
    let t = ok(
        verify(
            &rsa.token("k1", &with(claims(), json!({"email_verified": null}))),
            &keys,
        ),
        "unverified",
    );
    assert!(!t.email_verified());
}

#[test]
fn wrong_signatures_are_rejected() {
    let rsa = Signer::rsa();
    let ec = Signer::ec();
    let other_ec = Signer::ec();
    // Same kid, different key.
    let keys = jwks(&[other_ec.jwk("e1"), rsa.jwk("k1")]);
    assert_eq!(
        verify(&ec.token("e1", &claims()), &keys).err(),
        Some(TokenError::Signature)
    );
    // Payload changed after signing.
    let good = rsa.token("k1", &claims());
    let mut parts: Vec<&str> = good.split('.').collect();
    let forged = b64(with(claims(), json!({"sub": "admin"}))
        .to_string()
        .as_bytes());
    parts[1] = &forged;
    assert_eq!(
        verify(&parts.join("."), &keys).err(),
        Some(TokenError::Signature)
    );
    // Truncated or empty signature.
    let cut = &good[..good.len() - 4];
    assert!(verify(cut, &keys).is_err());
    let (head, _) = good.rsplit_once('.').unwrap_or_default();
    assert_eq!(
        verify(&format!("{head}."), &keys).err(),
        Some(TokenError::Signature)
    );
}

#[test]
fn none_hs256_and_other_algorithms_are_refused() {
    let rsa = Signer::rsa();
    let keys = jwks(&[rsa.jwk("k1")]);
    let body = b64(claims().to_string().as_bytes());
    for alg in [
        "none", "None", "HS256", "HS512", "RS512", "PS256", "ES384", "EdDSA",
    ] {
        let header = b64(json!({"alg": alg, "kid": "k1"}).to_string().as_bytes());
        let token = format!("{header}.{body}.{}", b64(b"x"));
        assert_eq!(
            verify(&token, &keys).err(),
            Some(TokenError::Algorithm),
            "{alg}"
        );
        let unsigned = format!("{header}.{body}.");
        assert_eq!(
            verify(&unsigned, &keys).err(),
            Some(TokenError::Algorithm),
            "{alg}"
        );
    }
    // HS256 "signed" with the public modulus (key confusion) is still refused.
    let header = b64(json!({"alg": "HS256", "kid": "k1"}).to_string().as_bytes());
    let token = format!("{header}.{body}.{}", b64(&[0u8; 32]));
    assert_eq!(verify(&token, &keys).err(), Some(TokenError::Algorithm));
    // A crit header is never understood.
    let crit = rsa.sign(
        &json!({"alg": "RS256", "kid": "k1", "crit": ["exp"], "exp": 1}),
        &claims(),
    );
    assert_eq!(verify(&crit, &keys).err(), Some(TokenError::Algorithm));
    // An RS256 header cannot use an EC key and vice versa.
    let ec = Signer::ec();
    let ec_only = jwks(&[ec.jwk("k1")]);
    assert_eq!(
        verify(&rsa.token("k1", &claims()), &ec_only).err(),
        Some(TokenError::UnknownKey)
    );
}

#[test]
fn claims_must_match_issuer_audience_time_and_nonce() {
    let rsa = Signer::rsa();
    let keys = jwks(&[rsa.jwk("k1")]);
    let check = |changes: Value| verify(&rsa.token("k1", &with(claims(), changes)), &keys).err();

    assert_eq!(
        check(json!({"iss": "https://evil.example.test"})),
        Some(TokenError::Issuer)
    );
    assert_eq!(
        check(json!({"iss": format!("{ISSUER}/")})),
        Some(TokenError::Issuer)
    );
    assert_eq!(check(json!({"iss": null})), Some(TokenError::Issuer));
    assert_eq!(
        check(json!({"aud": "client-b"})),
        Some(TokenError::Audience)
    );
    assert_eq!(check(json!({"aud": null})), Some(TokenError::Audience));
    assert_eq!(
        check(json!({"aud": [CLIENT, 7]})),
        Some(TokenError::Audience)
    );
    // Several audiences need azp = our client.
    assert_eq!(
        check(json!({"aud": [CLIENT, "api-x"]})),
        Some(TokenError::Audience)
    );
    assert_eq!(
        check(json!({"aud": [CLIENT, "api-x"], "azp": CLIENT})),
        None
    );
    assert_eq!(
        check(json!({"azp": "client-b"})),
        Some(TokenError::Audience)
    );
    assert_eq!(check(json!({"aud": [CLIENT]})), None);

    // exp/nbf/iat with 60 s of skew.
    assert_eq!(check(json!({"exp": NOW - 61})), Some(TokenError::Expired));
    assert_eq!(check(json!({"exp": NOW - 60})), Some(TokenError::Expired));
    assert_eq!(check(json!({"exp": NOW - 30})), None);
    assert_eq!(check(json!({"exp": null})), Some(TokenError::Malformed));
    assert_eq!(check(json!({"exp": "soon"})), Some(TokenError::Malformed));
    assert_eq!(
        check(json!({"nbf": NOW + 120})),
        Some(TokenError::NotYetValid)
    );
    assert_eq!(check(json!({"nbf": NOW + 30})), None);
    assert_eq!(
        check(json!({"iat": NOW + 120})),
        Some(TokenError::IssuedInFuture)
    );
    assert_eq!(check(json!({"iat": null})), Some(TokenError::Malformed));
    assert_eq!(check(json!({"iat": (NOW - 5) as f64 + 0.5})), None);

    assert_eq!(check(json!({"nonce": "n-other"})), Some(TokenError::Nonce));
    assert_eq!(check(json!({"nonce": null})), Some(TokenError::Nonce));
    assert_eq!(check(json!({"nonce": 1})), Some(TokenError::Nonce));
    assert_eq!(check(json!({"sub": ""})), Some(TokenError::Subject));
    assert_eq!(
        check(json!({"sub": "x".repeat(256)})),
        Some(TokenError::Subject)
    );
    assert_eq!(check(json!({"sub": 42})), Some(TokenError::Subject));
}

#[test]
fn unknown_kid_asks_for_one_jwks_refresh() {
    let old = Signer::ec();
    let new = Signer::rsa();
    let before = jwks(&[old.jwk("2026-10")]);
    let token = new.token("2026-11", &claims());
    assert_eq!(verify(&token, &before).err(), Some(TokenError::UnknownKey));
    // After the IdP rotates, the refreshed set has both keys and the token verifies.
    let after = jwks(&[old.jwk("2026-10"), new.jwk("2026-11")]);
    ok(verify(&token, &after), "after rotation");
    // A known kid with a key of another type is still unknown.
    let wrong_type = jwks(&[old.jwk("2026-11")]);
    assert_eq!(
        verify(&token, &wrong_type).err(),
        Some(TokenError::UnknownKey)
    );
}

#[test]
fn malformed_tokens_are_rejected_before_any_key_lookup() {
    let rsa = Signer::rsa();
    let keys = jwks(&[rsa.jwk("k1")]);
    let good = rsa.token("k1", &claims());
    let padded = {
        let (h, rest) = good.split_once('.').unwrap_or_default();
        format!("{h}=.{rest}")
    };
    for bad in [
        String::new(),
        "a.b".to_owned(),
        format!("{good}.x"),
        padded,
        format!("{}.{}.x", b64(b"not json"), b64(b"{}")),
        format!("{}.{}.x", b64(b"[1]"), b64(b"{}")),
        format!("{}.{}.x", b64(br#"{"kid":"k1"}"#), b64(b"{}")),
        format!("{}.{}.x", b64(br#"{"alg":"RS256","kid":1}"#), b64(b"{}")),
        "x".repeat(16 * 1024 + 1),
    ] {
        assert_eq!(
            verify(&bad, &keys).err(),
            Some(TokenError::Malformed),
            "{bad:.40}"
        );
    }
}

#[test]
fn jwks_keeps_only_usable_signing_keys() {
    let rsa = Signer::rsa();
    let ec = Signer::ec();
    let mut enc = rsa.jwk("enc");
    enc["use"] = json!("enc");
    let mut mislabeled = ec.jwk("ml");
    mislabeled["alg"] = json!("RS256");
    let weak = json!({"kty": "RSA", "kid": "weak", "n": b64(&[0xC5; 128]), "e": "AQAB"});
    let p384 =
        json!({"kty": "EC", "kid": "p384", "crv": "P-384", "x": b64(&[1; 48]), "y": b64(&[2; 48])});
    let hmac = json!({"kty": "oct", "kid": "h", "k": b64(b"shared")});
    let keys = jwks(&[
        enc,
        mislabeled,
        weak,
        p384,
        hmac,
        rsa.jwk("k1"),
        ec.jwk("e1"),
    ]);
    assert_eq!(keys.len(), 2);
    // Only unusable keys, no key array, or too many keys: refused.
    assert_eq!(
        parse_jwks(&json!({"keys": [{"kty": "oct", "k": "AA"}]})).err(),
        Some(OidcError::Jwks)
    );
    assert_eq!(parse_jwks(&json!({"key": []})).err(), Some(OidcError::Jwks));
    let many: Vec<Value> = (0..51).map(|i| ec.jwk(&format!("k{i}"))).collect();
    assert_eq!(
        parse_jwks(&json!({ "keys": many })).err(),
        Some(OidcError::Jwks)
    );
    // The encryption key cannot verify a token signed by the same RSA key under its kid.
    assert_eq!(
        verify(&rsa.token("enc", &claims()), &keys).err(),
        Some(TokenError::UnknownKey)
    );
}

fn discovery_doc(issuer: &str, jwks_uri: &str) -> Value {
    json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize?p=1"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": jwks_uri,
        "response_types_supported": ["code", "id_token"],
        "code_challenge_methods_supported": ["plain", "S256"],
        "id_token_signing_alg_values_supported": ["RS256"],
    })
}

#[test]
fn discovery_endpoints_stay_on_the_issuer_or_extra_hosts() {
    let google = "https://accounts.google.com";
    let none: Vec<String> = Vec::new();
    let extra = vec![
        "www.googleapis.com".to_owned(),
        "OAuth2.googleapis.com".to_owned(),
    ];
    let strict = ok(
        HostPolicy::new(google, &none, false).ok_or("policy"),
        "strict",
    );
    let wide = ok(
        HostPolicy::new(google, &extra, false).ok_or("policy"),
        "wide",
    );

    let doc = with(
        discovery_doc(google, "https://www.googleapis.com/oauth2/v3/certs"),
        json!({"token_endpoint": "https://oauth2.googleapis.com/token"}),
    );
    assert_eq!(
        parse_discovery(&doc, google, &strict).err(),
        Some(OidcError::Discovery("token_endpoint"))
    );
    let d = ok(parse_discovery(&doc, google, &wide), "google");
    assert_eq!(
        d.authorization_endpoint,
        "https://accounts.google.com/authorize?p=1"
    );
    assert_eq!(d.token_endpoint.origin, "https://oauth2.googleapis.com");
    assert_eq!(d.jwks.path_and_query, "/oauth2/v3/certs");

    let entra = "https://login.microsoftonline.com/tenant-a/v2.0";
    let p = ok(
        HostPolicy::new(entra, &none, false).ok_or("policy"),
        "entra",
    );
    let base = discovery_doc(
        entra,
        "https://login.microsoftonline.com/tenant-a/discovery/v2.0/keys",
    );
    ok(parse_discovery(&base, entra, &p), "entra");
    let check = |changes: Value| parse_discovery(&with(base.clone(), changes), entra, &p).err();
    // The document must name exactly the configured issuer.
    assert_eq!(
        check(json!({"issuer": format!("{entra}/")})),
        Some(OidcError::Discovery("issuer"))
    );
    assert_eq!(
        check(json!({"issuer": "https://login.microsoftonline.com/{tenantid}/v2.0"})),
        Some(OidcError::Discovery("issuer"))
    );
    for bad in [
        "http://login.microsoftonline.com/k",
        "https://login.microsoftonline.com:8443/k",
        "https://user@login.microsoftonline.com/k",
        "https://login.microsoftonline.com/k#f",
        "https://evil.example.test/k",
        "https://login.microsoftonline.com.evil.example.test/k",
        "https://169.254.169.254/latest/meta-data",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert_eq!(
            check(json!({"jwks_uri": bad})),
            Some(OidcError::Discovery("jwks_uri")),
            "{bad}"
        );
    }
    assert_eq!(
        check(json!({"token_endpoint": null})),
        Some(OidcError::Discovery("token_endpoint"))
    );
    assert_eq!(
        check(json!({"code_challenge_methods_supported": ["plain"]})),
        Some(OidcError::Discovery("code_challenge_methods_supported"))
    );
    assert_eq!(
        check(json!({"code_challenge_methods_supported": null})),
        None
    );
    assert_eq!(
        check(json!({"response_types_supported": ["id_token"]})),
        Some(OidcError::Discovery("response_types_supported"))
    );
    assert_eq!(
        check(json!({"id_token_signing_alg_values_supported": ["HS256", "none"]})),
        Some(OidcError::Discovery(
            "id_token_signing_alg_values_supported"
        ))
    );
    assert_eq!(
        parse_discovery(&json!([]), entra, &p).err(),
        Some(OidcError::Protocol)
    );

    // The issuer itself must be https on 443 outside lab mode.
    for bad in [
        "http://idp.example.test",
        "https://idp.example.test:8443",
        "https://u@idp.example.test",
    ] {
        assert!(HostPolicy::new(bad, &none, false).is_none(), "{bad}");
    }
    assert!(HostPolicy::new("http://127.0.0.1:9", &none, true).is_some());
}

async fn stub(app: Router) -> String {
    let l = ok(tokio::net::TcpListener::bind("127.0.0.1:0").await, "bind");
    let addr = ok(l.local_addr(), "addr");
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn discovery_and_jwks_are_fetched_through_the_guard() {
    let rsa = Signer::rsa();
    let jwk = rsa.jwk("k1");
    let issuer_slot = std::sync::Arc::new(std::sync::OnceLock::<String>::new());
    let slot = issuer_slot.clone();
    let app = Router::new()
        .route(
            "/realms/alfa/.well-known/openid-configuration",
            get(move || {
                let issuer = slot.get().cloned().unwrap_or_default();
                async move {
                    axum::Json(discovery_doc(
                        &issuer,
                        &format!("{issuer}/protocol/openid-connect/certs"),
                    ))
                }
            }),
        )
        .route(
            "/realms/alfa/protocol/openid-connect/certs",
            get(move || {
                let jwk = jwk.clone();
                async move { axum::Json(json!({ "keys": [jwk] })) }
            }),
        )
        .route(
            "/realms/moved/.well-known/openid-configuration",
            get(|| async {
                (
                    StatusCode::FOUND,
                    [("location", "https://evil.example.test/")],
                )
            }),
        )
        .route(
            "/realms/big/.well-known/openid-configuration",
            get(|| async { "x".repeat(1024 * 1024 + 1) }),
        );
    let base = stub(app).await;
    let issuer = format!("{base}/realms/alfa");
    let _ = issuer_slot.set(issuer.clone());
    let none: Vec<String> = Vec::new();
    let t = Duration::from_secs(5);

    let lab = ok(HostPolicy::new(&issuer, &none, true).ok_or("policy"), "lab");
    let d = ok(oidc::fetch_discovery(&issuer, &lab, t).await, "discovery");
    assert_eq!(d.issuer, issuer);
    let keys = ok(oidc::fetch_jwks(&d, &lab, t).await, "jwks");
    let token = rsa.token("k1", &with(claims(), json!({"iss": issuer})));
    let nonce_hash = oidc::sha256(NONCE.as_bytes());
    let exp = Expected {
        issuer: &d.issuer,
        client_id: CLIENT,
        nonce_hash: &nonce_hash,
        now: NOW,
    };
    ok(verify_id_token(&token, &keys, &exp), "verify");

    // Redirects are not followed; oversized bodies are cut off.
    let moved = format!("{base}/realms/moved");
    let p = ok(
        HostPolicy::new(&moved, &none, true).ok_or("policy"),
        "moved",
    );
    assert_eq!(
        oidc::fetch_discovery(&moved, &p, t).await.err(),
        Some(OidcError::Status(302))
    );
    let big = format!("{base}/realms/big");
    let p = ok(HostPolicy::new(&big, &none, true).ok_or("policy"), "big");
    assert_eq!(
        oidc::fetch_discovery(&big, &p, t).await.err(),
        Some(OidcError::Protocol)
    );

    // Outside lab mode the same local IdP is refused before anything is sent: http is not
    // allowed, and https on 127.0.0.1 is a private address.
    assert!(HostPolicy::new(&issuer, &none, false).is_none());
    let https_local = "https://127.0.0.1/realms/alfa";
    let strict = ok(
        HostPolicy::new(https_local, &none, false).ok_or("policy"),
        "strict",
    );
    assert_eq!(
        oidc::fetch_discovery(https_local, &strict, t).await.err(),
        Some(OidcError::Refused)
    );
}
