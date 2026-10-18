//! Browser sign-in: the `OpenID` Connect authorization-code flow against
//! `server.auth.issuer`, done by walgit itself. `GET /_auth/login?next=/p`
//! redirects to the issuer's `authorization_endpoint` (from discovery),
//! `GET /_auth/callback` exchanges the code at the `token_endpoint`, verifies the
//! returned ID token (signature, issuer, audience = our client, domain policy)
//! and sets the HMAC-signed session cookie; `/_auth/logout` clears it;
//! `/_auth/me` reports the account. A sign-in with no account yet chooses a username at
//! `/_auth/welcome` (D51), which creates it on the account log.
//!
//! `/_auth/tokens` is where git gets its credential: a signed-in browser `POST`s
//! there and receives a walgit access token (`wgt_…`, `server.auth.access_token_ttl`)
//! to paste into the credential helper; `GET` renders the small page that does it.
//! Tokens are stateless — rotating `session_secret` revokes them all.

use std::sync::Arc;

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};

use crate::{AppState, auth::SESSION_COOKIE};

const STATE_TTL_SECS: u64 = 600;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/_auth/login", get(login).post(password_login))
        .route("/_auth/signup", get(signup).post(create_password_account))
        .route("/_auth/password", get(password_form).post(change_password))
        .route("/_auth/callback", get(callback))
        .route("/_auth/welcome", get(welcome).post(create_account))
        .route("/_auth/claimed", get(claimed))
        .route("/_auth/logout", get(logout))
        .route("/_auth/me", get(me))
        .route("/_auth/check", get(check))
        .route("/_auth/tokens", get(tokens_page).post(mint_token))
        .with_state(state)
}

#[derive(serde::Deserialize, Default)]
struct LoginQuery {
    next: Option<String>,
    /// `oidc`: go to the identity provider even where passwords are on.
    via: Option<String>,
}
#[derive(serde::Deserialize, Default)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn safe_next(next: Option<String>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") => n,
        _ => "/".to_string(),
    }
}

fn request_port(st: &AppState, headers: &HeaderMap) -> u16 {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.rsplit_once(':'))
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or_else(|| st.cfg.server.listen.port())
}

/// The public origin (`server.public_url`, else forwarded host) is a loopback development
/// origin (`walgit.localhost`). Only there does the OAuth client allow a `localhost`
/// redirect and does the callback hop between the two hostnames.
fn loopback_origin(st: &AppState, headers: &HeaderMap) -> bool {
    let base = crate::smart::request_base_url(st, headers);
    let host = base.split("://").nth(1).unwrap_or(&base);
    let host = host.split('/').next().unwrap_or(host);
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host == "walgit.localhost" || host == "localhost" || host == "127.0.0.1" || host == "[::1]"
}

/// Where the issuer sends the code. A public deployment (`public_url = https://git.example.com`,
/// an OAuth client with that exact redirect): `<origin>/_auth/callback`. Local development:
/// most issuers allow only literal `localhost` / `127.0.0.1` as a loopback redirect, the
/// browser origin is `walgit.localhost`, and `callback` hops back to it.
fn redirect_uri(st: &AppState, headers: &HeaderMap) -> String {
    if !loopback_origin(st, headers) {
        return format!(
            "{}/_auth/callback",
            crate::smart::request_base_url(st, headers)
        );
    }
    let scheme = if st.cfg.tls_enabled() {
        "https"
    } else {
        "http"
    };
    let port = request_port(st, headers);
    if port == 443 && scheme == "https" || port == 80 && scheme == "http" {
        format!("{scheme}://localhost/_auth/callback")
    } else {
        format!("{scheme}://localhost:{port}/_auth/callback")
    }
}

fn walgit_origin(st: &AppState, headers: &HeaderMap) -> String {
    let scheme = if st.cfg.tls_enabled() {
        "https"
    } else {
        "http"
    };
    let port = request_port(st, headers);
    if port == 443 && scheme == "https" || port == 80 && scheme == "http" {
        format!("{scheme}://walgit.localhost")
    } else {
        format!("{scheme}://walgit.localhost:{port}")
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("%{b:02X}"));
            }
        }
    }
    out
}

async fn login(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LoginQuery>,
) -> Response {
    if !st.auth.browser_login_enabled() {
        return (
            StatusCode::UNAUTHORIZED,
            "not signed in: browser sign-in is not configured on this server (server.auth.passwords, or \
             server.auth.mode = \"oidc\" with oauth_client_id/oauth_client_secret; both need session_secret). \
             API and git clients authenticate with `Authorization: Bearer <token>`.",
        )
            .into_response();
    }
    let next = safe_next(q.next);
    // D52: password accounts sign in here; the identity provider is a link on the page.
    if st.auth.passwords() != walgit_config::PasswordAccounts::Off
        && q.via.as_deref() != Some("oidc")
    {
        return login_page(&st, &next, "", "", StatusCode::OK);
    }
    oidc_redirect(&st, &headers, next).await
}

/// Off to the identity provider (authorization-code flow); `/_auth/callback` comes back.
async fn oidc_redirect(st: &AppState, headers: &HeaderMap, next: String) -> Response {
    if !st.auth.oidc_login_enabled() {
        return (
            StatusCode::NOT_FOUND,
            "no identity provider is configured here",
        )
            .into_response();
    }
    let Ok(disco) = st.auth.discovery().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity provider unavailable (OIDC discovery failed)",
        )
            .into_response();
    };
    let (client_id, _) = st.auth.oauth_client().unwrap();
    let nonce: u64 = rand::random();
    let payload = format!("{}\n{nonce:x}\n{next}", now() + STATE_TTL_SECS);
    let Some(state) = st.auth.sign(payload.as_bytes()) else {
        return (StatusCode::NOT_IMPLEMENTED, "session secret missing").into_response();
    };
    let sep = if disco.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let mut url = format!(
        "{}{sep}client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&prompt=select_account",
        disco.authorization_endpoint,
        urlencode(client_id),
        urlencode(&redirect_uri(st, headers)),
        urlencode("openid email profile"),
        urlencode(&state),
    );
    // Google honours `hd` as a domain hint on its account chooser; other issuers ignore it.
    if let Some(hd) = st.cfg.server.auth.allowed_domains.first() {
        {
            let _ = std::fmt::Write::write_fmt(&mut url, format_args!("&hd={}", urlencode(hd)));
        };
    }
    let mut r = Redirect::to(&url).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn exchange_code(
    token_endpoint: &str,
    form: &[(&str, &str); 5],
) -> Result<reqwest::Response, reqwest::Error> {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("reqwest client");
    let mut last = None;
    for attempt in 1u8..=2 {
        match client.post(token_endpoint).form(form).send().await {
            Ok(r) => return Ok(r),
            Err(e) if attempt < 2 && (e.is_connect() || e.is_timeout()) => {
                tracing::warn!(attempt, error = %e, "oauth token exchange retrying");
                last = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("retry left an error"))
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    if let Some(e) = q.error {
        return (StatusCode::UNAUTHORIZED, format!("sign-in failed: {e}")).into_response();
    }
    let (Some(code), Some(state)) = (q.code, q.state) else {
        return (StatusCode::BAD_REQUEST, "missing code/state").into_response();
    };
    let Some(payload) = st.auth.verify_signed(&state) else {
        return (StatusCode::BAD_REQUEST, "invalid state").into_response();
    };
    let payload = String::from_utf8_lossy(&payload).to_string();
    let mut parts = payload.splitn(3, '\n');
    let exp: u64 = parts.next().and_then(|e| e.parse().ok()).unwrap_or(0);
    let _nonce = parts.next();
    let next = safe_next(parts.next().map(str::to_string));
    if now() > exp {
        return (StatusCode::BAD_REQUEST, "sign-in expired; try again").into_response();
    }
    let Some((client_id, client_secret)) = st.auth.oauth_client() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "browser sign-in not configured",
        )
            .into_response();
    };
    let form = [
        ("code", code.as_str()),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", &redirect_uri(&st, &headers)),
        ("grant_type", "authorization_code"),
    ];
    let Ok(disco) = st.auth.discovery().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "identity provider unavailable",
        )
            .into_response();
    };
    // The code is single-use: only retry when we never heard back (connect/timeout).
    let resp = match exchange_code(&disco.token_endpoint, &form).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, connect = e.is_connect(), timeout = e.is_timeout(), "oauth token exchange failed");
            return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response();
        }
    };
    let tok: TokenResponse = match resp.json().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "oauth token response unreadable");
            return (StatusCode::BAD_GATEWAY, "token exchange failed").into_response();
        }
    };
    let Some(id_token) = tok.id_token else {
        tracing::warn!(error = ?tok.error, description = ?tok.error_description, "oauth token exchange returned no id_token");
        return (
            StatusCode::UNAUTHORIZED,
            format!("sign-in failed: {}", tok.error.unwrap_or_default()),
        )
            .into_response();
    };
    let identity = match st.auth.verify_login_id_token(&id_token).await {
        Ok(i) => i,
        Err(crate::auth::AuthError::Forbidden) => {
            return (
                StatusCode::FORBIDDEN,
                "your account is not in an allowed domain",
            )
                .into_response();
        }
        Err(e) => {
            tracing::warn!(error = ?e, "login id_token rejected");
            return (StatusCode::UNAUTHORIZED, "sign-in failed: token rejected").into_response();
        }
    };
    // D51: a known sign-in is its account; a new one chooses a username first.
    let accounts = match st.accounts.synced().await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "account log unavailable at sign-in");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "account log unavailable; retry",
            )
                .into_response();
        }
    };
    let Some(user) = accounts.accounts_read(|s| {
        s.user_by_identity(&identity.issuer, &identity.subject)
            .cloned()
    }) else {
        let Some(pending) = st.auth.pending_value(&identity) else {
            return (StatusCode::NOT_IMPLEMENTED, "session secret missing").into_response();
        };
        let origin = if loopback_origin(&st, &headers) {
            walgit_origin(&st, &headers)
        } else {
            String::new()
        };
        let dest = format!(
            "{origin}/_auth/welcome?pending={}&next={}",
            urlencode(&pending),
            urlencode(&next)
        );
        let mut r = Redirect::to(&dest).into_response();
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return r;
    };
    sync_profile(&accounts, &user, &identity).await;
    tracing::info!(login = %user.login, "browser sign-in");
    signed_in_redirect(&st, &headers, &user.user_id, &next)
}

/// Keep the account's profile what the issuer says now (email, name, picture); best effort.
async fn sync_profile(
    accounts: &walgit_wal::RepoHandle,
    user: &walgit_wal::accounts::User,
    id: &crate::auth::Identity,
) {
    use walgit_proto::v1::{AccountEvent, ProfileChanged, account_event::Body};
    let changed = |now: &str, was: &str| {
        if now == was {
            String::new()
        } else {
            now.to_string()
        }
    };
    let p = ProfileChanged {
        user_id: user.user_id.clone(),
        email: changed(&id.email, &user.email),
        name: changed(&id.name, &user.name),
        avatar_url: changed(&id.picture, &user.avatar_url),
    };
    if p.email.is_empty() && p.name.is_empty() && p.avatar_url.is_empty() {
        return;
    }
    let ev = AccountEvent {
        body: Some(Body::ProfileChanged(p)),
    };
    if let Err(e) = accounts.publish_accounts(vec![ev], &user.user_id).await {
        tracing::warn!(login = %user.login, error = %e, "profile update failed");
    }
}

/// Signed in as `user_id`: set the session and go to `next` (on a loopback origin, by way of
/// the `walgit.localhost` hop so both host names carry the session).
fn signed_in_redirect(st: &AppState, headers: &HeaderMap, user_id: &str, next: &str) -> Response {
    let Some(value) = st.auth.session_cookie_value(user_id) else {
        return (StatusCode::NOT_IMPLEMENTED, "session secret missing").into_response();
    };
    let next = next.to_string();
    let cookie = session_set_cookie(st, headers, &value);
    // Public origin: the callback ran there; the cookie is already right — go to `next`.
    if !loopback_origin(st, headers) {
        let mut r = Redirect::to(&next).into_response();
        r.headers_mut()
            .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return r;
    }
    // Standalone: the cookie is host-scoped: set it on localhost (this callback) and hop to
    // walgit.localhost so the same session is issued there too.
    let hop = format!("{}\n{}\n{}", now() + 60, value, next);
    let dest = match st.auth.sign(hop.as_bytes()) {
        Some(ticket) => format!(
            "{}/_auth/claimed?ticket={}&next={}",
            walgit_origin(st, headers),
            urlencode(&ticket),
            urlencode(&next),
        ),
        None => next.clone(),
    };
    let mut r = Redirect::to(&dest).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

#[derive(serde::Deserialize, Default)]
struct WelcomeQuery {
    pending: Option<String>,
    next: Option<String>,
}

/// `GET /_auth/welcome?pending=…` — choose a username (D51). Reached only from the callback
/// with a signed, short-lived pending sign-up.
async fn welcome(State(st): State<Arc<AppState>>, Query(q): Query<WelcomeQuery>) -> Response {
    let pending = q.pending.unwrap_or_default();
    match st.auth.pending_identity(&pending) {
        Some(id) => welcome_page(&id, &pending, &safe_next(q.next), "", "", StatusCode::OK),
        None => Redirect::to("/_auth/login").into_response(),
    }
}

#[derive(serde::Deserialize)]
struct WelcomeForm {
    pending: String,
    login: String,
    #[serde(default)]
    next: Option<String>,
}

/// `POST /_auth/welcome` — create the account: a new `user_id`, the chosen login and the
/// sign-in identity, in one account-log entry (both or neither). A taken or reserved login
/// shows the form again (409).
async fn create_account(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Form(form): axum::Form<WelcomeForm>,
) -> Response {
    use walgit_proto::v1::{AccountEvent, IdentityLinked, UserCreated, account_event::Body};
    // Same-origin only: another site must not sign a browser in as someone else.
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v != "same-origin" && v != "none")
    {
        return crate::error::ApiError::Forbidden.into_response();
    }
    let Some(id) = st.auth.pending_identity(&form.pending) else {
        return Redirect::to("/_auth/login").into_response();
    };
    let next = safe_next(form.next);
    let login = form.login.trim().to_ascii_lowercase();
    let page = |error: &str, status| welcome_page(&id, &form.pending, &next, &login, error, status);
    if let Err(why) = walgit_wal::accounts::valid_user_login(&login) {
        return page(&why, StatusCode::BAD_REQUEST);
    }
    let accounts = match st.accounts.synced().await {
        Ok(h) => h,
        Err(e) => {
            return page(
                &format!("account log unavailable ({e}); retry"),
                StatusCode::SERVICE_UNAVAILABLE,
            );
        }
    };
    let user_id = uuid::Uuid::new_v4().to_string();
    let events = vec![
        AccountEvent {
            body: Some(Body::UserCreated(UserCreated {
                user_id: user_id.clone(),
                login: login.clone(),
                email: id.email.clone(),
                name: id.name.clone(),
                avatar_url: id.picture.clone(),
            })),
        },
        AccountEvent {
            body: Some(Body::IdentityLinked(IdentityLinked {
                user_id: user_id.clone(),
                issuer: id.issuer.clone(),
                subject: id.subject.clone(),
            })),
        },
    ];
    match accounts.publish_accounts(events, &user_id).await {
        Ok(_) => {
            tracing::info!(%login, "account created");
            signed_in_redirect(&st, &headers, &user_id, &next)
        }
        Err(walgit_wal::WalError::Invalid(why)) => {
            // The identity was linked meanwhile (a second tab): sign in as that account.
            match accounts.accounts_read(|s| s.user_by_identity(&id.issuer, &id.subject).cloned()) {
                Some(user) => signed_in_redirect(&st, &headers, &user.user_id, &next),
                None => page(&why, StatusCode::CONFLICT),
            }
        }
        Err(e) => page(
            &format!("could not create the account ({e}); retry"),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    }
}

fn welcome_page(
    id: &crate::auth::Identity,
    pending: &str,
    next: &str,
    login: &str,
    error: &str,
    status: StatusCode,
) -> Response {
    let suggestion = if login.is_empty() {
        id.email
            .split('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "-")
    } else {
        login.to_string()
    };
    let body = format!(
        r#"<p class="muted">Signed in as <strong>{email}</strong>. Your username is your namespace (<code>/username/repository</code>) and how others mention and assign you.</p>
<form method="post" action="/_auth/welcome">
<input type="hidden" name="pending" value="{pending}">
<input type="hidden" name="next" value="{next}">
<label>Username
<input name="login" value="{login}" required maxlength="39" pattern="[a-z0-9]+(-[a-z0-9]+)*" autocomplete="username" autofocus>
<span class="muted small">Lowercase letters, digits and single hyphens, up to 39 characters.</span></label>
<button type="submit" class="btn primary">Create account</button>
</form>"#,
        email = html_escape(&id.email),
        pending = html_escape(pending),
        next = html_escape(next),
        login = html_escape(&suggestion),
    );
    page("Choose a username", error, &body, status)
}

#[derive(serde::Deserialize, Default)]
struct ClaimedQuery {
    ticket: Option<String>,
    next: Option<String>,
}

async fn claimed(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ClaimedQuery>,
) -> Response {
    let next = safe_next(q.next);
    let Some(ticket) = q.ticket else {
        return Redirect::to(&next).into_response();
    };
    let Some(payload) = st.auth.verify_signed(&ticket) else {
        return Redirect::to(&next).into_response();
    };
    let payload = String::from_utf8_lossy(&payload).to_string();
    let mut parts = payload.splitn(3, '\n');
    let exp: u64 = parts.next().and_then(|e| e.parse().ok()).unwrap_or(0);
    let value = parts.next().unwrap_or("");
    if now() > exp || value.is_empty() {
        return Redirect::to(&next).into_response();
    }
    let cookie = session_set_cookie(&st, &headers, value);
    let mut r = Redirect::to(&next).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn logout(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let secure = crate::smart::request_base_url(&st, &headers).starts_with("https://");
    let cookie = format!(
        "{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; {}",
        cookie_site(&st, secure)
    );
    let mut r = Redirect::to("/").into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    r
}

/// `GET /_auth/check` — optional `auth_request` target for an nginx edge. Same verifier as
/// every other route (bearer or session cookie) and no body: `204` +
/// `X-Walgit-Principal` (the login, for the edge's logs) / `X-Walgit-Write` (may create
/// repositories), or `401`/`403`/`503`. Repository access is walgit's alone to decide.
async fn check(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match crate::access::signed_in(&st, &headers).await {
        Ok(s) => {
            let mut r = StatusCode::NO_CONTENT.into_response();
            let h = r.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&s.name) {
                h.insert("x-walgit-principal", v);
            }
            h.insert(
                "x-walgit-write",
                HeaderValue::from_static(if s.can_create { "1" } else { "0" }),
            );
            h.insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=300"),
            );
            r
        }
        Err(e) => crate::access::auth_err(e).into_response(),
    }
}

async fn me(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match crate::access::authenticate(&st, &headers).await {
        Ok(s) if !s.is_anonymous() => {
            let mut r = axum::Json(
                serde_json::json!({ "login": s.name, "user_id": s.user_id, "can_create": s.can_create }),
            )
            .into_response();
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        _ => crate::error::ApiError::Unauthorized.into_response(),
    }
}

/// `SameSite`/`Secure` attributes of the session cookie. With a cross-origin
/// browser lane configured (`server.cors_origins`) the cookie must travel
/// on credentialed fetches from those origins: `SameSite=None; Secure`.
/// Otherwise `Lax`.
pub fn session_set_cookie(st: &AppState, headers: &HeaderMap, value: &str) -> String {
    let secure = crate::smart::request_base_url(st, headers).starts_with("https://");
    format!(
        "{SESSION_COOKIE}={value}; Path=/; Max-Age={}; HttpOnly; {}",
        st.auth.session_ttl().as_secs(),
        cookie_site(st, secure)
    )
}

/// Sliding sessions (middleware on every app response): a valid session cookie
/// older than `session_ttl / 4` whose principal still passes policy is
/// re-issued with a fresh `exp` via `Set-Cookie`.
pub async fn refresh_session(
    State(st): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path();
    let fresh = if path.starts_with("/_auth/") {
        None
    } else {
        st.auth.session_refresh_value(req.headers())
    };
    let cookie = fresh.map(|v| session_set_cookie(&st, req.headers(), &v));
    let mut resp = next.run(req).await;
    if let Some(c) = cookie
        && (resp.status().is_success() || resp.status().is_redirection())
        && let Ok(hv) = HeaderValue::from_str(&c)
    {
        resp.headers_mut().append(header::SET_COOKIE, hv);
    }
    resp
}

/// `POST /_auth/tokens` — mint a walgit access token for the signed-in browser principal.
/// Same-origin only (a cookie-bearing cross-site POST is refused by `Sec-Fetch-Site`).
async fn mint_token(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !st.auth.issued_tokens_enabled() {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "issued tokens are off (server.auth.session_secret unset)",
        )
            .into_response();
    }
    if headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v != "same-origin" && v != "none")
    {
        return crate::error::ApiError::Forbidden.into_response();
    }
    let subject = match crate::access::authenticate(&st, &headers).await {
        Ok(s) if !s.is_anonymous() => s,
        Ok(_) => return crate::error::ApiError::Unauthorized.into_response(),
        Err(e) => return crate::access::auth_err(e).into_response(),
    };
    let Some(token) = subject
        .user_id
        .as_deref()
        .and_then(|id| st.auth.access_token(id))
    else {
        return (StatusCode::NOT_IMPLEMENTED, "session secret missing").into_response();
    };
    let (exp, _) = st
        .auth
        .access_token_claims(&token)
        .unwrap_or((0, String::new()));
    tracing::info!(login = %subject.name, exp, "access token minted");
    let mut r = axum::Json(serde_json::json!({
        "token": token,
        "login": subject.name,
        "can_create": subject.can_create,
        "expires_at": exp,
    }))
    .into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// `GET /_auth/tokens` — a small page: one button that POSTs and shows the token with the
/// git one-liner that stores it. Unauthenticated browsers are sent to sign in first.
async fn tokens_page(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let signed_in = matches!(
        crate::access::authenticate(&st, &headers).await,
        Ok(s) if !s.is_anonymous()
    );
    if !signed_in {
        let mut r = Redirect::to("/_auth/login?next=%2F_auth%2Ftokens").into_response();
        r.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return r;
    }
    let base = crate::smart::request_base_url(&st, &headers);
    let host = base
        .split("://")
        .nth(1)
        .unwrap_or(&base)
        .trim_end_matches('/')
        .to_string();
    let ttl_days = st.auth.access_token_ttl().as_secs() / 86400;
    let body = TOKENS_BODY
        .replace("{{host}}", &html_escape(&host))
        .replace("{{ttl_days}}", &ttl_days.to_string());
    page("Access token", "", &body, StatusCode::OK)
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const TOKENS_BODY: &str = r#"<p class="muted">A token lets <code>git</code> and scripts on <code>{{host}}</code> act as you for {{ttl_days}} days. It is shown once; walgit does not store it.</p>
<form id="mint-form"><button id="mint" type="submit" class="btn primary">Create a token</button></form>
<p id="mint-err" class="flash error" hidden></p>
<div id="out" hidden>
<div class="row gap"><input id="token" readonly aria-label="Access token" style="flex:1"><button id="copy" type="button" class="btn primary">Copy</button></div>
<p>Copy it now. When <code>git push</code> asks:</p>
<pre class="code-block">Username: <span id="login"></span>
Password: &lt;paste the token&gt;</pre>
<p class="muted small">Git keeps it in its credential store, so it asks once (if <code>git config --global credential.helper</code> prints nothing, set it to <code>store</code>).</p>
<p class="muted small">Or send it as <code>Authorization: Bearer &lt;token&gt;</code>. Tokens cannot be listed or revoked one by one; rotating the server's <code>session_secret</code> revokes them all.</p>
</div>
<script>
document.getElementById('mint-form').onsubmit = async (e) => {
  e.preventDefault();
  const r = await fetch('/_auth/tokens', {method:'POST', credentials:'same-origin'});
  if (!r.ok) { const p = document.getElementById('mint-err'); p.textContent = 'Could not create a token: HTTP ' + r.status; p.hidden = false; return; }
  const j = await r.json();
  // An input, not a <pre>: selecting a block copies its line ending too, and git refuses a
  // password with a carriage return. Copy puts exactly the token on the clipboard.
  const t = document.getElementById('token');
  t.value = j.token;
  t.onfocus = () => t.select();
  document.getElementById('copy').onclick = async (ev) => {
    try { await navigator.clipboard.writeText(j.token); ev.target.textContent = 'Copied'; }
    catch { t.focus(); ev.target.textContent = 'Press Ctrl+C'; }
  };
  document.getElementById('login').textContent = j.login;
  document.getElementById('out').hidden = false;
  document.getElementById('mint').disabled = true;
};
</script>"#;

fn cookie_site(st: &AppState, secure: bool) -> &'static str {
    if !st.cfg.server.cors_origins.is_empty() && secure {
        "SameSite=None; Secure"
    } else if secure {
        "SameSite=Lax; Secure"
    } else {
        "SameSite=Lax"
    }
}

// ---- D52: password accounts ---------------------------------------------------------------

/// Same-origin form posts only: another site must not sign a browser in (or out of a password).
fn cross_site(headers: &HeaderMap) -> bool {
    headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v != "same-origin" && v != "none")
}

/// Every `/_auth/*` page: the SPA's stylesheet and top bar around one narrow column.
fn page(title: &str, error: &str, body: &str, status: StatusCode) -> Response {
    let error = if error.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"flash error\" role=\"alert\">{}</p>",
            html_escape(error)
        )
    };
    let html = AUTH_PAGE
        .replace("{{styles}}", crate::web::ui::stylesheets())
        .replace("{{title}}", &html_escape(title))
        .replace("{{error}}", &error)
        .replace("{{body}}", body);
    let mut r = (status, axum::response::Html(html)).into_response();
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn login_page(st: &AppState, next: &str, login: &str, error: &str, status: StatusCode) -> Response {
    let next_q = urlencode(next);
    let mut links = Vec::new();
    if st.auth.passwords() == walgit_config::PasswordAccounts::Open {
        links.push(format!(
            "New here? <a href=\"/_auth/signup?next={next_q}\">Create an account</a>."
        ));
    }
    if st.auth.oidc_login_enabled() {
        links.push(format!(
            "<a href=\"/_auth/login?via=oidc&amp;next={next_q}\">Sign in with your identity provider</a>"
        ));
    }
    let body = format!(
        r#"<form method="post" action="/_auth/login">
<input type="hidden" name="next" value="{next}">
<label>Username
<input name="login" value="{login}" required autocomplete="username"{focus_login}></label>
<label>Password
<input name="password" type="password" required autocomplete="current-password"{focus_password}></label>
<button type="submit" class="btn primary">Sign in</button>
</form>
<p class="auth-alt muted">{links}</p>"#,
        next = html_escape(next),
        login = html_escape(login),
        links = links.join(" · "),
        // Back with an error: the username is filled in, so type the password again.
        focus_login = if login.is_empty() { " autofocus" } else { "" },
        focus_password = if login.is_empty() { "" } else { " autofocus" },
    );
    page("Sign in to walgit", error, &body, status)
}

#[derive(serde::Deserialize)]
struct LoginForm {
    login: String,
    password: String,
    #[serde(default)]
    next: Option<String>,
}

/// `POST /_auth/login` — a password account signs in: a session, then `next`. Ten failures for
/// one login in 15 minutes lock it out for the rest of the window (per instance).
async fn password_login(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Form(f): axum::Form<LoginForm>,
) -> Response {
    if st.auth.passwords() == walgit_config::PasswordAccounts::Off {
        return StatusCode::NOT_FOUND.into_response();
    }
    if cross_site(&headers) {
        return crate::error::ApiError::Forbidden.into_response();
    }
    let next = safe_next(f.next);
    let login = f.login.trim().to_ascii_lowercase();
    if let Some(wait) = st.throttle.locked(&login) {
        let mins = wait.as_secs().div_ceil(60);
        let why = format!("Too many failed attempts. Try again in {mins} minute(s).");
        return login_page(&st, &next, &login, &why, StatusCode::TOO_MANY_REQUESTS);
    }
    let user = match st.accounts.synced().await {
        Ok(h) => h.accounts_read(|s| s.user_by_login(&login).cloned()),
        Err(e) => {
            tracing::warn!(error = %e, "account log unavailable at sign-in");
            return login_page(
                &st,
                &next,
                &login,
                "The account log is unavailable; retry.",
                StatusCode::SERVICE_UNAVAILABLE,
            );
        }
    };
    let phc = user
        .as_ref()
        .map(|u| u.password.clone())
        .unwrap_or_default();
    match user {
        Some(user) if crate::passwords::verify(f.password, phc).await => {
            st.throttle.succeeded(&login);
            tracing::info!(%login, "password sign-in");
            signed_in_redirect(&st, &headers, &user.user_id, &next)
        }
        _ => {
            st.throttle.failed(&login);
            login_page(
                &st,
                &next,
                &login,
                "Wrong username or password.",
                StatusCode::UNAUTHORIZED,
            )
        }
    }
}

fn signup_page(next: &str, login: &str, email: &str, error: &str, status: StatusCode) -> Response {
    let body = format!(
        r#"<form method="post" action="/_auth/signup">
<input type="hidden" name="next" value="{next}">
<label>Username
<input name="login" value="{login}" required maxlength="39" pattern="[a-z0-9]+(-[a-z0-9]+)*" autocomplete="username" autofocus>
<span class="muted small">Your namespace (<code>/username/repository</code>): lowercase letters, digits and single hyphens.</span></label>
<label><span>Email <span class="muted">(optional)</span></span>
<input name="email" type="email" value="{email}" autocomplete="email"></label>
<label>Password
<input name="password" type="password" required minlength="8" autocomplete="new-password"></label>
<label>Password again
<input name="confirm" type="password" required minlength="8" autocomplete="new-password"></label>
<button type="submit" class="btn primary">Create account</button>
</form>
<p class="auth-alt muted">Already have one? <a href="/_auth/login?next={next_q}">Sign in</a>.</p>"#,
        next = html_escape(next),
        next_q = urlencode(next),
        login = html_escape(login),
        email = html_escape(email),
    );
    page("Create your account", error, &body, status)
}

/// `GET /_auth/signup` (`passwords = "open"`).
async fn signup(State(st): State<Arc<AppState>>, Query(q): Query<LoginQuery>) -> Response {
    if st.auth.passwords() != walgit_config::PasswordAccounts::Open {
        return (
            StatusCode::NOT_FOUND,
            "sign-up is not open here; ask a site admin for an account",
        )
            .into_response();
    }
    signup_page(&safe_next(q.next), "", "", "", StatusCode::OK)
}

#[derive(serde::Deserialize)]
struct SignupForm {
    login: String,
    #[serde(default)]
    email: String,
    password: String,
    confirm: String,
    #[serde(default)]
    next: Option<String>,
}

/// `POST /_auth/signup` — the account and its password in one account-log entry, then a session.
async fn create_password_account(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Form(f): axum::Form<SignupForm>,
) -> Response {
    if st.auth.passwords() != walgit_config::PasswordAccounts::Open {
        return StatusCode::NOT_FOUND.into_response();
    }
    if cross_site(&headers) {
        return crate::error::ApiError::Forbidden.into_response();
    }
    let next = safe_next(f.next);
    let login = f.login.trim().to_ascii_lowercase();
    let email = f.email.trim().to_string();
    let again = |why: &str, status| signup_page(&next, &login, &email, why, status);
    if let Err(why) = walgit_wal::accounts::valid_user_login(&login) {
        return again(&why, StatusCode::BAD_REQUEST);
    }
    if let Err(why) = crate::passwords::check_new(&f.password) {
        return again(&why, StatusCode::BAD_REQUEST);
    }
    if f.password != f.confirm {
        return again("The passwords don't match.", StatusCode::BAD_REQUEST);
    }
    match create_user(&st, &login, &email, "", Some(f.password)).await {
        Ok(user_id) => {
            tracing::info!(%login, "password account created");
            signed_in_redirect(&st, &headers, &user_id, &next)
        }
        Err(crate::error::ApiError::Conflict(why)) => again(&why, StatusCode::CONFLICT),
        Err(e) => again(&e.message(), e.status()),
    }
}

/// D52: create an account (and its password, when given) as one account-log entry. Shared by
/// sign-up and the site admin's `POST /api/v1/users`.
pub(crate) async fn create_user(
    st: &AppState,
    login: &str,
    email: &str,
    name: &str,
    password: Option<String>,
) -> Result<String, crate::error::ApiError> {
    use walgit_proto::v1::{AccountEvent, PasswordSet, UserCreated, account_event::Body};
    let user_id = uuid::Uuid::new_v4().to_string();
    let mut events = vec![AccountEvent {
        body: Some(Body::UserCreated(UserCreated {
            user_id: user_id.clone(),
            login: login.to_string(),
            email: email.to_string(),
            name: name.to_string(),
            avatar_url: String::new(),
        })),
    }];
    if let Some(pw) = password {
        crate::passwords::check_new(&pw).map_err(crate::error::ApiError::BadRequest)?;
        let phc = crate::passwords::hash(pw)
            .await
            .map_err(crate::error::ApiError::Internal)?;
        events.push(AccountEvent {
            body: Some(Body::PasswordSet(PasswordSet {
                user_id: user_id.clone(),
                phc,
            })),
        });
    }
    let log = st
        .accounts
        .synced()
        .await
        .map_err(|e| crate::error::ApiError::ServiceUnavailable(format!("account log: {e}")))?;
    log.publish_accounts(events, login)
        .await
        .map_err(|e| match e {
            walgit_wal::WalError::Invalid(why) => crate::error::ApiError::Conflict(why),
            e => crate::error::ApiError::ServiceUnavailable(format!("account log: {e}")),
        })?;
    Ok(user_id)
}

/// D52: set (or clear, with `None`) an account's password.
pub(crate) async fn set_password(
    st: &AppState,
    user_id: &str,
    password: &str,
) -> Result<(), crate::error::ApiError> {
    use walgit_proto::v1::{AccountEvent, PasswordSet, account_event::Body};
    crate::passwords::check_new(password).map_err(crate::error::ApiError::BadRequest)?;
    let phc = crate::passwords::hash(password.to_string())
        .await
        .map_err(crate::error::ApiError::Internal)?;
    let log = st
        .accounts
        .synced()
        .await
        .map_err(|e| crate::error::ApiError::ServiceUnavailable(format!("account log: {e}")))?;
    let ev = AccountEvent {
        body: Some(Body::PasswordSet(PasswordSet {
            user_id: user_id.to_string(),
            phc,
        })),
    };
    log.publish_accounts(vec![ev], user_id)
        .await
        .map(|_| ())
        .map_err(|e| match e {
            walgit_wal::WalError::Invalid(why) => crate::error::ApiError::BadRequest(why),
            e => crate::error::ApiError::ServiceUnavailable(format!("account log: {e}")),
        })
}

fn password_page(error: &str, done: bool, status: StatusCode) -> Response {
    if done {
        return page(
            "Password changed",
            "",
            "<p><a href=\"/settings\">Back to settings</a></p>",
            status,
        );
    }
    let body = r#"<form method="post" action="/_auth/password">
<label><span>Current password <span class="muted">(if you have one)</span></span>
<input name="current" type="password" autocomplete="current-password"></label>
<label>New password
<input name="password" type="password" required minlength="8" autocomplete="new-password"></label>
<button type="submit" class="btn primary">Change password</button>
</form>"#;
    page("Change your password", error, body, status)
}

async fn password_form(State(st): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    match crate::access::authenticate(&st, &headers).await {
        Ok(s)
            if s.user_id.is_some()
                && s.user_id.as_deref() != Some(crate::access::LOCAL_USER_ID) =>
        {
            password_page("", false, StatusCode::OK)
        }
        _ => Redirect::to("/_auth/login?next=%2F_auth%2Fpassword").into_response(),
    }
}

#[derive(serde::Deserialize)]
struct PasswordForm {
    #[serde(default)]
    current: String,
    password: String,
}

/// `POST /_auth/password` — the signed-in account changes its password (the current one first,
/// when it has one).
async fn change_password(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Form(f): axum::Form<PasswordForm>,
) -> Response {
    if cross_site(&headers) {
        return crate::error::ApiError::Forbidden.into_response();
    }
    let Ok(subject) = crate::access::authenticate(&st, &headers).await else {
        return Redirect::to("/_auth/login?next=%2F_auth%2Fpassword").into_response();
    };
    let Some(user_id) = subject
        .user_id
        .clone()
        .filter(|id| id != crate::access::LOCAL_USER_ID)
    else {
        return Redirect::to("/_auth/login?next=%2F_auth%2Fpassword").into_response();
    };
    let current = st
        .accounts
        .cached(|s| s.users.get(&user_id).map(|u| u.password.clone()))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if !current.is_empty() {
        if let Some(wait) = st.throttle.locked(&subject.name) {
            let why = format!(
                "Too many failed attempts. Try again in {} minute(s).",
                wait.as_secs().div_ceil(60)
            );
            return password_page(&why, false, StatusCode::TOO_MANY_REQUESTS);
        }
        if !crate::passwords::verify(f.current, current).await {
            st.throttle.failed(&subject.name);
            return password_page(
                "The current password is wrong.",
                false,
                StatusCode::UNAUTHORIZED,
            );
        }
    }
    match set_password(&st, &user_id, &f.password).await {
        Ok(()) => password_page("", true, StatusCode::OK),
        Err(e) => password_page(&e.message(), false, e.status()),
    }
}

/// Same head as `web/index.html` (pre-paint theme, color-scheme) and the app's top bar.
const AUTH_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="color-scheme" content="dark light">
<script>try { var t = localStorage.getItem("theme"); if (t === "light" || t === "dark") document.documentElement.dataset.theme = t; } catch (e) {}</script>
<title>walgit · {{title}}</title>
{{styles}}
</head><body>
<header class="topbar"><a href="/" class="brand">walgit/</a></header>
<main class="container"><div class="auth">
<h1 class="page-title">{{title}}</h1>
{{error}}
<div class="box"><div class="pad">
{{body}}
</div></div>
</div></main>
</body></html>"#;
