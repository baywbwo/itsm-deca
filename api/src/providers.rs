//! CP5b provider clients: Jira Cloud REST v3 and Lark Open Platform (custom app).
//!
//! Every request goes through [`Target`], the outbound guard:
//! - the base URL is `https://host[:port]` only (no user info, path, query or fragment); `http://`
//!   only with `ITSM_INTEGRATIONS_LAB=on`;
//! - the host is resolved once and every address must be public (no loopback, private, link-local,
//!   CGNAT, documentation, multicast or reserved ranges, IPv4-mapped forms included) unless lab
//!   mode is on; the connection is pinned to the checked addresses so a second DNS answer cannot
//!   swap in an internal one;
//! - redirects are never followed, environment proxies are ignored, bodies are capped.
//!
//! Errors are reduced to [`CallError`] kinds. Provider bodies, URLs and credentials never reach a
//! log line, an error message or a response; callers turn a kind into a fixed safe sentence.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Method, Url};
use serde::Deserialize;
use serde_json::{Value, json};

/// Largest provider answer read into memory.
const MAX_BODY: usize = 1024 * 1024;
/// Pages of the Lark chat list read at most (100 chats each).
const MAX_CHAT_PAGES: usize = 10;
/// Lark business code for "too many requests".
const LARK_RATE_LIMITED: i64 = 99_991_400;

/// Why a provider call did not produce a usable answer. Carries no provider text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    /// The target address is not allowed (SSRF guard) or the URL is malformed. Nothing was sent.
    Refused,
    /// DNS or TCP/TLS connection failed. Nothing was sent.
    Connect,
    /// The operation budget ran out; a write may or may not have reached the provider.
    Timeout,
    /// The provider answered with this HTTP status.
    Status(u16),
    /// Lark answered HTTP 200 with a business error code.
    Api(i64),
    /// The answer could not be read (too large, not JSON, unexpected shape, broken stream).
    Protocol,
}

impl CallError {
    /// Short stable label for logs and audit rows.
    pub fn kind(self) -> &'static str {
        match self {
            CallError::Refused => "refused",
            CallError::Connect => "connect",
            CallError::Timeout => "timeout",
            CallError::Status(_) => "status",
            CallError::Api(_) => "api",
            CallError::Protocol => "protocol",
        }
    }

    /// The provider certainly did not accept the request and asks to come back later.
    pub fn is_throttle(self) -> bool {
        matches!(self, CallError::Status(429 | 503)) || self == CallError::Api(LARK_RATE_LIMITED)
    }

    /// Fixed Indonesian sentence for notes and outbox errors (never provider text).
    pub fn note(self) -> String {
        match self {
            CallError::Refused => "Alamat penyedia ditolak: hanya https ke alamat resmi penyedia (Jira *.atlassian.net, Lark open.larksuite.com/open.feishu.cn), bukan jaringan privat.".to_owned(),
            CallError::Connect => "Penyedia tidak dapat dihubungi (DNS atau koneksi gagal).".to_owned(),
            CallError::Timeout => "Penyedia tidak menjawab dalam batas waktu.".to_owned(),
            CallError::Status(401 | 403) => {
                "Kredensial ditolak penyedia (HTTP 401/403). Periksa email, token atau izin akun.".to_owned()
            }
            CallError::Status(404) => "Data tidak ditemukan di penyedia atau akun tidak berhak (HTTP 404).".to_owned(),
            CallError::Status(429) => "Penyedia membatasi laju permintaan (HTTP 429).".to_owned(),
            CallError::Status(code) => format!("Penyedia menjawab HTTP {code}."),
            CallError::Api(code) => format!("Penyedia menolak permintaan (kode {code})."),
            CallError::Protocol => "Jawaban penyedia tidak dapat dibaca.".to_owned(),
        }
    }
}

/// Validates a provider base URL and returns its normal form `scheme://host[:port]`.
/// Syntax only; addresses are checked when a request is made ([`Target::prepare`]).
pub fn normalize_base_url(raw: &str, lab: bool) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 200 {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    let scheme_ok = url.scheme() == "https" || (lab && url.scheme() == "http");
    if !scheme_ok
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    let host = url.host_str()?;
    if host.is_empty() {
        return None;
    }
    // Literal private addresses are refused up front; names are checked again after DNS.
    if !lab {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        let private_ip = bare.parse::<IpAddr>().is_ok_and(|ip| !is_public(ip));
        if private_ip || host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
            return None;
        }
    }
    Some(match url.port() {
        Some(p) => format!("{}://{host}:{p}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// Hosts a provider may be called at outside lab mode (CP5B-02): Jira Cloud tenants
/// (`<name>.atlassian.net`, exactly one label) and the two Lark Open Platform regions.
pub fn allowed_host(provider: &str, host: &str) -> bool {
    match provider {
        "jira" => host.strip_suffix(".atlassian.net").is_some_and(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        }),
        "lark" => matches!(host, "open.larksuite.com" | "open.feishu.cn"),
        _ => false,
    }
}

/// [`normalize_base_url`] plus, outside lab mode, the provider allowlist on the default https
/// port only: a stored credential is never sent to any other host or port.
pub fn provider_base_url(provider: &str, raw: &str, lab: bool) -> Option<String> {
    let base = normalize_base_url(raw, lab)?;
    if lab {
        return Some(base);
    }
    let url = Url::parse(&base).ok()?;
    let ok = url.port().is_none() && url.host_str().is_some_and(|h| allowed_host(provider, h));
    ok.then_some(base)
}

fn v4_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn v6_public(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // IPv4-mapped (::ffff:a.b.c.d) and NAT64 (64:ff9b::a.b.c.d) carry an IPv4 target.
    if s[..5] == [0, 0, 0, 0, 0] && s[5] == 0xffff || s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return v4_public(v4);
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || (s[0] & 0xfe00) == 0xfc00
        || (s[0] & 0xffc0) == 0xfe80
        || (s[0] & 0xffc0) == 0xfec0
        || (s[0] & 0xff00) == 0xff00
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        // Remaining IPv4-compatible (deprecated) addresses.
        || s[..6] == [0, 0, 0, 0, 0, 0])
}

/// True when `ip` may be contacted outside lab mode.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_public(v4),
        IpAddr::V6(v6) => v6_public(v6),
    }
}

/// A checked provider endpoint with its pinned HTTP client.
pub struct Target {
    base: String,
    client: reqwest::Client,
}

impl Target {
    /// Validates the base URL against the provider allowlist (again, for settings stored before
    /// it existed), resolves it and checks every address, then builds a client pinned to those
    /// addresses. Call inside the operation's time budget (DNS can hang).
    pub async fn prepare(
        provider: &str,
        raw_base: &str,
        lab: bool,
        timeout: Duration,
    ) -> Result<Self, CallError> {
        let base = provider_base_url(provider, raw_base, lab).ok_or(CallError::Refused)?;
        Self::prepare_origin(base, lab, timeout).await
    }

    /// Resolves and pins an origin `scheme://host[:port]` that the caller already validated against
    /// its own allowlist (CP5c: OIDC endpoints of a configured IdP). Same address rules as above.
    pub async fn prepare_origin(
        base: String,
        lab: bool,
        timeout: Duration,
    ) -> Result<Self, CallError> {
        let url = Url::parse(&base).map_err(|_| CallError::Refused)?;
        let host = url.host_str().ok_or(CallError::Refused)?.to_owned();
        let port = url.port_or_known_default().ok_or(CallError::Refused)?;
        let lookup = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((lookup.as_str(), port))
            .await
            .map_err(|_| CallError::Connect)?
            .collect();
        if addrs.is_empty() {
            return Err(CallError::Connect);
        }
        if !lab && addrs.iter().any(|a| !is_public(a.ip())) {
            return Err(CallError::Refused);
        }
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .https_only(!lab)
            .timeout(timeout)
            .connect_timeout(timeout)
            .user_agent(concat!("itsm-deca/", env!("CARGO_PKG_VERSION")));
        if lookup.parse::<IpAddr>().is_err() {
            builder = builder.resolve_to_addrs(&host, &addrs);
        }
        let client = builder.build().map_err(|_| CallError::Protocol)?;
        Ok(Self { base, client })
    }

    /// GET returning the parsed JSON body of a 2xx answer (CP5c discovery and JWKS).
    pub async fn get_json(&self, path_and_query: &str) -> Result<Value, CallError> {
        self.call(Method::GET, path_and_query, None, None).await
    }

    /// POST of an `application/x-www-form-urlencoded` body (CP5c token endpoint).
    pub async fn post_form(
        &self,
        path_and_query: &str,
        auth: &HeaderValue,
        form: &[(&str, &str)],
    ) -> Result<Value, CallError> {
        let body = form
            .iter()
            .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
            .collect::<Vec<_>>()
            .join("&");
        self.send(
            Method::POST,
            path_and_query,
            Some(auth),
            Some(("application/x-www-form-urlencoded", body.into_bytes())),
        )
        .await
    }

    /// Sends one request and returns the parsed JSON body of a 2xx answer.
    async fn call(
        &self,
        method: Method,
        path_and_query: &str,
        auth: Option<&HeaderValue>,
        body: Option<&Value>,
    ) -> Result<Value, CallError> {
        let body = match body {
            Some(b) => Some((
                "application/json; charset=utf-8",
                serde_json::to_vec(b).map_err(|_| CallError::Protocol)?,
            )),
            None => None,
        };
        self.send(method, path_and_query, auth, body).await
    }

    async fn send(
        &self,
        method: Method,
        path_and_query: &str,
        auth: Option<&HeaderValue>,
        body: Option<(&'static str, Vec<u8>)>,
    ) -> Result<Value, CallError> {
        let mut req = self
            .client
            .request(method, format!("{}{path_and_query}", self.base))
            .header(ACCEPT, "application/json");
        if let Some(a) = auth {
            req = req.header(AUTHORIZATION, a.clone());
        }
        if let Some((content_type, b)) = body {
            req = req.header(CONTENT_TYPE, content_type).body(b);
        }
        let mut resp = req.send().await.map_err(classify)?;
        let status = resp.status();
        // 3xx is never followed (and never trusted): treat like any other non-success answer.
        if !status.is_success() {
            return Err(CallError::Status(status.as_u16()));
        }
        let mut buf = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(classify)? {
            if buf.len() + chunk.len() > MAX_BODY {
                return Err(CallError::Protocol);
            }
            buf.extend_from_slice(&chunk);
        }
        if buf.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&buf).map_err(|_| CallError::Protocol)
    }
}

fn classify(e: reqwest::Error) -> CallError {
    if e.is_timeout() {
        CallError::Timeout
    } else if e.is_connect() {
        CallError::Connect
    } else {
        CallError::Protocol
    }
}

pub(crate) fn sensitive(value: String) -> Result<HeaderValue, CallError> {
    let mut v = HeaderValue::from_str(&value).map_err(|_| CallError::Protocol)?;
    v.set_sensitive(true);
    Ok(v)
}

fn parse<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T, CallError> {
    serde_json::from_value(v).map_err(|_| CallError::Protocol)
}

/// Percent-encodes one path segment or query value.
pub(crate) fn enc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------- Jira Cloud ----------

/// Jira Cloud REST v3 with Basic auth (account email + API token).
pub struct Jira {
    target: Target,
    auth: HeaderValue,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct IdName {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraUser {
    pub account_id: String,
    pub display_name: String,
    #[serde(default = "yes")]
    pub active: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JiraProject {
    pub key: String,
    #[serde(default)]
    pub issue_types: Vec<IdName>,
}

/// An issue as stored locally: never more than id, key, status and a browse URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub id: String,
    pub key: String,
    pub status: Option<String>,
    /// `https://…/browse/KEY`, only for an https base URL.
    pub url: Option<String>,
}

#[derive(Deserialize)]
struct IssueBody {
    id: String,
    key: String,
    #[serde(default)]
    fields: Option<IssueFields>,
}

#[derive(Deserialize)]
struct IssueFields {
    status: Option<IdName>,
}

/// What a new Jira issue gets. No ticket chronology or customer data is sent.
pub struct NewIssue<'a> {
    pub project_key: &'a str,
    pub issue_type_id: &'a str,
    pub summary: &'a str,
    pub description: &'a str,
    pub priority_id: Option<&'a str>,
    pub assignee_account_id: Option<&'a str>,
    /// Labels; the first is the idempotency label searched before creating.
    pub labels: &'a [String],
}

impl Jira {
    pub fn new(target: Target, email: &str, token: &[u8]) -> Result<Self, CallError> {
        let mut raw = email.as_bytes().to_vec();
        raw.push(b':');
        raw.extend_from_slice(token);
        let auth = sensitive(format!("Basic {}", STANDARD.encode(&raw)))?;
        zeroize::Zeroize::zeroize(&mut raw);
        Ok(Self { target, auth })
    }

    async fn get(&self, path: &str) -> Result<Value, CallError> {
        self.target
            .call(Method::GET, path, Some(&self.auth), None)
            .await
    }

    pub async fn myself(&self) -> Result<JiraUser, CallError> {
        parse(self.get("/rest/api/3/myself").await?)
    }

    pub async fn project(&self, key: &str) -> Result<JiraProject, CallError> {
        parse(
            self.get(&format!("/rest/api/3/project/{}", enc(key)))
                .await?,
        )
    }

    pub async fn priorities(&self) -> Result<Vec<IdName>, CallError> {
        parse(self.get("/rest/api/3/priority").await?)
    }

    /// Distinct workflow statuses used by the project's issue types.
    pub async fn statuses(&self, project_key: &str) -> Result<Vec<IdName>, CallError> {
        #[derive(Deserialize)]
        struct PerType {
            #[serde(default)]
            statuses: Vec<IdName>,
        }
        let per: Vec<PerType> = parse(
            self.get(&format!(
                "/rest/api/3/project/{}/statuses",
                enc(project_key)
            ))
            .await?,
        )?;
        let mut out: Vec<IdName> = Vec::new();
        for s in per.into_iter().flat_map(|p| p.statuses) {
            if !out.iter().any(|o| o.id == s.id) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// Users that can be assigned issues in the project (Jira user search, max 20).
    pub async fn assignable(
        &self,
        project_key: &str,
        query: &str,
    ) -> Result<Vec<JiraUser>, CallError> {
        let users: Vec<JiraUser> = parse(
            self.get(&format!(
                "/rest/api/3/user/assignable/search?project={}&query={}&maxResults=20",
                enc(project_key),
                enc(query)
            ))
            .await?,
        )?;
        Ok(users.into_iter().filter(|u| u.active).collect())
    }

    fn issue_ref(&self, b: IssueBody) -> IssueRef {
        let url = self
            .target
            .base
            .starts_with("https://")
            .then(|| format!("{}/browse/{}", self.target.base, b.key));
        IssueRef {
            status: b.fields.and_then(|f| f.status).map(|s| s.name),
            id: b.id,
            key: b.key,
            url,
        }
    }

    pub async fn issue(&self, key: &str) -> Result<IssueRef, CallError> {
        let b: IssueBody = parse(
            self.get(&format!("/rest/api/3/issue/{}?fields=status", enc(key)))
                .await?,
        )?;
        Ok(self.issue_ref(b))
    }

    /// Finds an issue already created for this label (a previous attempt that did reach Jira).
    pub async fn find_by_label(&self, label: &str) -> Result<Option<IssueRef>, CallError> {
        #[derive(Deserialize)]
        struct Found {
            #[serde(default)]
            issues: Vec<IssueBody>,
        }
        let body = json!({
            "jql": format!("labels = \"{label}\""),
            "fields": ["status"],
            "maxResults": 1,
        });
        let found: Found = parse(
            self.target
                .call(
                    Method::POST,
                    "/rest/api/3/search/jql",
                    Some(&self.auth),
                    Some(&body),
                )
                .await?,
        )?;
        Ok(found.issues.into_iter().next().map(|b| self.issue_ref(b)))
    }

    pub async fn create_issue(&self, issue: &NewIssue<'_>) -> Result<IssueRef, CallError> {
        let mut fields = json!({
            "project": { "key": issue.project_key },
            "issuetype": { "id": issue.issue_type_id },
            "summary": issue.summary,
            "labels": issue.labels,
            "description": {
                "type": "doc",
                "version": 1,
                "content": [{
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": issue.description }]
                }]
            },
        });
        if let Some(p) = issue.priority_id {
            fields["priority"] = json!({ "id": p });
        }
        if let Some(a) = issue.assignee_account_id {
            fields["assignee"] = json!({ "accountId": a });
        }
        let created: IssueBody = parse(
            self.target
                .call(
                    Method::POST,
                    "/rest/api/3/issue",
                    Some(&self.auth),
                    Some(&json!({ "fields": fields })),
                )
                .await?,
        )?;
        Ok(self.issue_ref(created))
    }
}

// ---------- Lark ----------

/// Lark Open Platform with a custom app (app_id + app_secret → tenant_access_token).
pub struct Lark {
    target: Target,
    auth: HeaderValue,
}

#[derive(Deserialize)]
struct LarkEnvelope {
    code: i64,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    bot: Option<Value>,
    #[serde(default)]
    tenant_access_token: Option<String>,
}

fn lark_ok(v: Value) -> Result<LarkEnvelope, CallError> {
    let env: LarkEnvelope = parse(v)?;
    if env.code != 0 {
        return Err(CallError::Api(env.code));
    }
    Ok(env)
}

#[derive(Debug, Clone, Deserialize)]
pub struct LarkChat {
    pub chat_id: String,
    #[serde(default)]
    pub name: String,
}

impl Lark {
    /// Exchanges the app credentials for a tenant access token (valid ~2 h; fetched per operation).
    pub async fn connect(
        target: Target,
        app_id: &str,
        app_secret: &[u8],
    ) -> Result<Self, CallError> {
        let secret = std::str::from_utf8(app_secret).map_err(|_| CallError::Protocol)?;
        let env = lark_ok(
            target
                .call(
                    Method::POST,
                    "/open-apis/auth/v3/tenant_access_token/internal",
                    None,
                    Some(&json!({ "app_id": app_id, "app_secret": secret })),
                )
                .await?,
        )?;
        let token = env.tenant_access_token.ok_or(CallError::Protocol)?;
        let auth = sensitive(format!("Bearer {token}"))?;
        Ok(Self { target, auth })
    }

    /// Bot name of the app (proves the bot capability is enabled).
    pub async fn bot_name(&self) -> Result<String, CallError> {
        let env = lark_ok(
            self.target
                .call(
                    Method::GET,
                    "/open-apis/bot/v3/info",
                    Some(&self.auth),
                    None,
                )
                .await?,
        )?;
        env.bot
            .and_then(|b| b.get("app_name").and_then(Value::as_str).map(str::to_owned))
            .ok_or(CallError::Protocol)
    }

    /// Group chats the bot is a member of (at most 1000).
    pub async fn chats(&self) -> Result<Vec<LarkChat>, CallError> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            items: Vec<LarkChat>,
            #[serde(default)]
            has_more: bool,
            #[serde(default)]
            page_token: Option<String>,
        }
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_CHAT_PAGES {
            let path = match &token {
                Some(t) => format!("/open-apis/im/v1/chats?page_size=100&page_token={}", enc(t)),
                None => "/open-apis/im/v1/chats?page_size=100".to_owned(),
            };
            let env = lark_ok(
                self.target
                    .call(Method::GET, &path, Some(&self.auth), None)
                    .await?,
            )?;
            let page: Page = parse(env.data.ok_or(CallError::Protocol)?)?;
            out.extend(page.items.into_iter().filter(|c| !c.chat_id.is_empty()));
            match (page.has_more, page.page_token) {
                (true, Some(t)) if !t.is_empty() => token = Some(t),
                _ => break,
            }
        }
        Ok(out)
    }

    /// Sends a text message. `dedupe` (≤ 50 chars) makes Lark drop a repeat within one hour.
    pub async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        dedupe: &str,
    ) -> Result<(), CallError> {
        let content =
            serde_json::to_string(&json!({ "text": text })).map_err(|_| CallError::Protocol)?;
        lark_ok(
            self.target
                .call(
                    Method::POST,
                    "/open-apis/im/v1/messages?receive_id_type=chat_id",
                    Some(&self.auth),
                    Some(&json!({
                        "receive_id": chat_id,
                        "msg_type": "text",
                        "content": content,
                        "uuid": dedupe,
                    })),
                )
                .await?,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_https_origins_only() {
        assert_eq!(
            normalize_base_url(" https://acme.atlassian.net/ ", false).as_deref(),
            Some("https://acme.atlassian.net")
        );
        assert_eq!(
            normalize_base_url("https://open.larksuite.com:8443", false).as_deref(),
            Some("https://open.larksuite.com:8443")
        );
        for bad in [
            "http://acme.atlassian.net",
            "https://user:pw@acme.atlassian.net",
            "https://acme.atlassian.net/rest",
            "https://acme.atlassian.net?x=1",
            "https://acme.atlassian.net#f",
            "ftp://acme",
            "acme.atlassian.net",
            "",
        ] {
            assert_eq!(normalize_base_url(bad, false), None, "{bad}");
        }
        assert_eq!(
            normalize_base_url("http://127.0.0.1:9000", true).as_deref(),
            Some("http://127.0.0.1:9000")
        );
    }

    #[test]
    fn internal_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a9fe:a9fe",
        ] {
            let ip: IpAddr = ip.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
            assert!(!is_public(ip), "{ip}");
        }
        for ip in [
            "8.8.8.8",
            "104.192.141.1",
            "2606:4700::1111",
            "::ffff:8.8.8.8",
        ] {
            let ip: IpAddr = ip.parse().unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
            assert!(is_public(ip), "{ip}");
        }
    }

    #[tokio::test]
    async fn loopback_target_is_refused_outside_lab_before_any_request() {
        let r = Target::prepare("jira", "https://127.0.0.1:9", false, Duration::from_secs(1)).await;
        assert_eq!(r.err(), Some(CallError::Refused));
        let r = Target::prepare("jira", "https://localhost:9", false, Duration::from_secs(1)).await;
        assert_eq!(r.err(), Some(CallError::Refused));
        let r = Target::prepare("jira", "http://8.8.8.8", false, Duration::from_secs(1)).await;
        assert_eq!(r.err(), Some(CallError::Refused));
    }

    /// CP5B-02: outside lab mode only the provider's own hosts on the default https port pass,
    /// in `configure` and again before any request (no DNS lookup for other names).
    #[tokio::test]
    async fn cp5b02_only_provider_hosts_on_port_443_outside_lab() {
        for (provider, good) in [
            ("jira", "https://acme.atlassian.net"),
            ("jira", "https://ACME-ops2.atlassian.net/"),
            ("jira", "https://acme.atlassian.net:443"),
            ("lark", "https://open.larksuite.com"),
            ("lark", "https://open.feishu.cn"),
        ] {
            assert!(
                provider_base_url(provider, good, false).is_some(),
                "{provider} {good}"
            );
        }
        for (provider, bad) in [
            ("jira", "https://evil.example"),
            ("jira", "https://atlassian.net"),
            ("jira", "https://a.b.atlassian.net"),
            ("jira", "https://acme.atlassian.net.evil.example"),
            ("jira", "https://evilatlassian.net"),
            ("jira", "https://-acme.atlassian.net"),
            ("jira", "https://acme.atlassian.net."),
            ("jira", "https://acme.atlassian.net:8443"),
            ("jira", "http://acme.atlassian.net"),
            ("jira", "https://open.larksuite.com"),
            ("lark", "https://acme.atlassian.net"),
            ("lark", "https://open.larksuite.com:8443"),
            ("lark", "https://x.open.larksuite.com"),
            ("lark", "https://8.8.8.8"),
            ("other", "https://acme.atlassian.net"),
        ] {
            assert_eq!(
                provider_base_url(provider, bad, false),
                None,
                "{provider} {bad}"
            );
            let r = Target::prepare(provider, bad, false, Duration::from_secs(1)).await;
            assert_eq!(r.err(), Some(CallError::Refused), "{provider} {bad}");
        }
        // Lab mode (UAT only) keeps local stubs reachable.
        assert!(provider_base_url("jira", "http://127.0.0.1:9000", true).is_some());
    }

    #[test]
    fn notes_never_echo_provider_text() {
        assert!(CallError::Status(401).note().contains("401"));
        assert!(CallError::Status(429).is_throttle());
        assert!(CallError::Api(LARK_RATE_LIMITED).is_throttle());
        assert!(!CallError::Status(400).is_throttle());
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
    }
}
