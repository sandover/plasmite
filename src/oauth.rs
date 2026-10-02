//! OAuth authorization for the direct HTTPS MCP endpoint.
//!
//! The access key remains the authority. OAuth records hold only its stable key ID,
//! so a key revocation stops every token and an in-flight MCP operation.

use axum::extract::{ConnectInfo, Form, FromRequest, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use getrandom::fill;
use oauth_as::AuthorizationRequest;
use oauth_as::pkce::{verifier_is_valid, verify_s256};
use plasmite::api::Error;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use url::{Host, Url};

use crate::access_store::{AccessGrant, AccessStore, ensure_private, read_json, write_atomic_json};

const CODE_LIFETIME: u64 = 600;
const ACCESS_LIFETIME: u64 = 900;
const MAX_CLIENTS: usize = 1024;
const MAX_PENDING: usize = 1024;
const MAX_PENDING_PER_SOURCE: usize = 32;
const MAX_PENDING_PER_CLIENT: usize = 8;
const PUBLIC_REQUESTS_PER_MINUTE: u16 = 30;

#[derive(Clone)]
pub(crate) struct OauthService {
    issuer: String,
    resource: String,
    access: Arc<AccessStore>,
    path: PathBuf,
    state: Arc<Mutex<OAuthState>>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    request_windows: Arc<Mutex<HashMap<IpAddr, (u64, u16)>>>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct OAuthState {
    clients: Vec<Client>,
    codes: Vec<Code>,
    grants: Vec<Grant>,
    access_tokens: Vec<AccessToken>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Client {
    id: String,
    name: String,
    redirect_uris: Vec<String>,
    expires_at: Option<u64>,
    #[serde(default)]
    key_id: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Code {
    hash: String,
    client_id: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    key_id: String,
    expires_at: u64,
}

#[derive(Clone, Deserialize, Serialize)]
struct Grant {
    id: String,
    client_id: String,
    resource: String,
    key_id: String,
    refresh_hash: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct AccessToken {
    hash: String,
    grant_id: String,
    expires_at: u64,
}

struct Pending {
    source: IpAddr,
    client_id: String,
    client_name: String,
    redirect_uri: String,
    challenge: String,
    resource: String,
    state: Option<String>,
    csrf: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct RegisterRequest {
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: Option<String>,
}

#[derive(Deserialize)]
struct Approval {
    request: String,
    access_key: Option<String>,
    decision: Option<String>,
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    client_id: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

#[derive(Deserialize)]
struct RevokeRequest {
    token: String,
    client_id: String,
}

impl OauthService {
    pub(crate) fn open(access: Arc<AccessStore>) -> Result<Option<Arc<Self>>, Error> {
        let Some(address) = access.shared_address() else {
            return Ok(None);
        };
        let issuer = address.trim_end_matches('/').to_owned();
        let resource = format!("{issuer}/mcp");
        let path = access.state_dir().join("oauth.json");
        let state = if path.exists() {
            ensure_private(&path)?;
            read_json(&path)?
        } else {
            OAuthState::default()
        };
        Ok(Some(Arc::new(Self {
            issuer,
            resource,
            access,
            path,
            state: Arc::new(Mutex::new(state)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_windows: Arc::new(Mutex::new(HashMap::new())),
        })))
    }

    pub(crate) fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                get(resource_metadata),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                get(resource_metadata),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                get(server_metadata),
            )
            .route("/oauth/register", post(register))
            .route("/oauth/authorize", get(authorize_page))
            .route("/oauth/approve", post(approve))
            .route("/oauth/token", post(token))
            .route("/oauth/revoke", post(revoke))
            .with_state(self)
    }

    pub(crate) fn challenge(&self) -> Response {
        let metadata = format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
            self.issuer
        );
        let mut response = oauth_error(StatusCode::UNAUTHORIZED, "invalid_token");
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_str(&metadata).expect("validated HTTPS issuer"),
        );
        response
    }

    pub(crate) fn grant(&self, bearer: &str) -> Option<AccessGrant> {
        let hash = digest(bearer);
        let state = self.state.lock().ok()?;
        let access_token = state
            .access_tokens
            .iter()
            .find(|token| token.hash == hash && token.expires_at > now())?;
        let grant = state
            .grants
            .iter()
            .find(|grant| grant.id == access_token.grant_id)?;
        if grant.resource != self.resource {
            return None;
        }
        self.access.authorize_id(&grant.key_id)
    }

    fn change<T>(
        &self,
        change: impl FnOnce(&mut OAuthState) -> Result<T, Response>,
    ) -> Result<T, Response> {
        let mut current = self.state.lock().map_err(|_| server_error())?;
        let mut next = current.clone();
        let now = now();
        next.codes.retain(|code| {
            code.expires_at > now && self.access.authorize_id(&code.key_id).is_some()
        });
        next.grants
            .retain(|grant| self.access.authorize_id(&grant.key_id).is_some());
        next.access_tokens.retain(|token| {
            token.expires_at > now && next.grants.iter().any(|grant| grant.id == token.grant_id)
        });
        next.clients.retain(|client| {
            client.expires_at.is_some_and(|expiry| expiry > now)
                || client
                    .key_id
                    .as_ref()
                    .is_some_and(|id| self.access.authorize_id(id).is_some())
                || next.codes.iter().any(|code| code.client_id == client.id)
                || next.grants.iter().any(|grant| grant.client_id == client.id)
        });
        let result = change(&mut next)?;
        if let Err(err) = write_atomic_json(&self.path, &next) {
            // A failed directory sync may follow a successful rename. Rejoin the
            // durable file before serving another request.
            *current = read_json(&self.path).unwrap_or_default();
            tracing::error!("OAuth state write failed: {err}");
            return Err(server_error());
        }
        *current = next;
        Ok(result)
    }

    fn admit_public_request(&self, source: IpAddr) -> bool {
        let minute = now() / 60;
        let Ok(mut windows) = self.request_windows.lock() else {
            return false;
        };
        windows.retain(|_, (seen_minute, _)| *seen_minute == minute);
        if !windows.contains_key(&source) && windows.len() >= MAX_CLIENTS {
            return false;
        }
        let window = windows.entry(source).or_insert((minute, 0));
        if window.1 >= PUBLIC_REQUESTS_PER_MINUTE {
            return false;
        }
        window.1 += 1;
        true
    }
}

async fn resource_metadata(State(oauth): State<Arc<OauthService>>) -> Response {
    Json(json!({
        "resource": oauth.resource,
        "authorization_servers": [oauth.issuer],
        "bearer_methods_supported": ["header"]
    }))
    .into_response()
}

async fn server_metadata(State(oauth): State<Arc<OauthService>>) -> Response {
    let issuer = &oauth.issuer;
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "token_endpoint_auth_methods_supported": ["none"]
    }))
    .into_response()
}

async fn register(
    State(oauth): State<Arc<OauthService>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
) -> Response {
    if !oauth.admit_public_request(peer.ip()) {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable");
    }
    let Json(request) = match Json::<RegisterRequest>::from_request(request, &()).await {
        Ok(request) => request,
        Err(err) => return err.into_response(),
    };
    if request.redirect_uris.is_empty()
        || request.redirect_uris.len() > 8
        || request
            .redirect_uris
            .iter()
            .any(|uri| uri.len() > 2048 || !valid_redirect(uri))
        || request
            .token_endpoint_auth_method
            .as_deref()
            .is_some_and(|method| method != "none")
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata");
    }
    let name = request
        .client_name
        .unwrap_or_else(|| "MCP client".to_owned());
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client_metadata");
    }
    let id = match random_hex() {
        Ok(id) => id,
        Err(response) => return response,
    };
    let client = Client {
        id: id.clone(),
        name,
        redirect_uris: request.redirect_uris,
        expires_at: Some(now() + CODE_LIFETIME),
        key_id: None,
    };
    match oauth.change(|state| {
        if state.clients.len() >= MAX_CLIENTS {
            return Err(oauth_error(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
            ));
        }
        state.clients.push(client.clone());
        Ok(())
    }) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({
                "client_id": id,
                "client_name": client.name,
                "redirect_uris": client.redirect_uris,
                "token_endpoint_auth_method": "none"
            })),
        )
            .into_response(),
        Err(response) => response,
    }
}

async fn authorize_page(
    State(oauth): State<Arc<OauthService>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    RawQuery(raw): RawQuery,
) -> Response {
    if !oauth.admit_public_request(peer.ip()) {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable");
    }
    let Some(raw) = raw else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let pairs: Vec<(String, String)> = url::form_urlencoded::parse(raw.as_bytes())
        .into_owned()
        .collect();
    let mut seen = std::collections::HashSet::new();
    if pairs
        .iter()
        .any(|(name, _)| name != "resource" && !seen.insert(name))
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let request = AuthorizationRequest::from_pairs(pairs);
    let Some(client_id) = request.client_id.as_deref() else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let Some(redirect_uri) = request.redirect_uri.as_deref() else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let Some(challenge) = request.code_challenge.as_deref() else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if request.response_type.as_deref() != Some("code")
        || request.code_challenge_method.as_deref() != Some("S256")
        || challenge.len() != 43
        || !challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || request.resource.is_empty()
        || request
            .resource
            .iter()
            .any(|resource| resource != &oauth.resource)
        || request
            .scope
            .as_deref()
            .is_some_and(|scope| !scope.is_empty())
        || request.authorization_details.is_some()
        || request
            .state
            .as_deref()
            .is_some_and(|state| state.len() > 1024)
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let client = match oauth.state.lock() {
        Ok(state) => state
            .clients
            .iter()
            .find(|client| {
                client.id == *client_id && client.expires_at.is_none_or(|expiry| expiry > now())
            })
            .cloned(),
        Err(_) => return server_error(),
    };
    let Some(client) = client else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client");
    };
    if !client.redirect_uris.iter().any(|uri| uri == redirect_uri) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let request_id = match random_hex() {
        Ok(value) => value,
        Err(response) => return response,
    };
    let csrf = match random_hex() {
        Ok(value) => value,
        Err(response) => return response,
    };
    let pending = Pending {
        source: peer.ip(),
        client_id: client.id,
        client_name: client.name,
        redirect_uri: redirect_uri.to_owned(),
        challenge: challenge.to_owned(),
        resource: oauth.resource.clone(),
        state: request.state.map(|state| state.into_owned()),
        csrf: csrf.clone(),
        expires_at: now() + CODE_LIFETIME,
    };
    let mut state = match oauth.pending.lock() {
        Ok(state) => state,
        Err(_) => return server_error(),
    };
    state.retain(|_, pending| pending.expires_at > now());
    if state.len() >= MAX_PENDING
        || state
            .values()
            .filter(|pending| pending.source == peer.ip())
            .count()
            >= MAX_PENDING_PER_SOURCE
        || state
            .values()
            .filter(|entry| entry.client_id == pending.client_id)
            .count()
            >= MAX_PENDING_PER_CLIENT
    {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable");
    }
    let name = html(&pending.client_name);
    let resource = html(&pending.resource);
    let callback = html(&pending.redirect_uri);
    state.insert(request_id.clone(), pending);
    let page = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Authorize MCP access</title><h1>Authorize MCP access</h1><p><strong>{name}</strong> requests access to the pools at {resource}.</p><p>The client name is supplied by the client and does not prove its identity. The response will go to {callback}.</p><form method=\"post\" action=\"/oauth/approve\"><input type=\"hidden\" name=\"request\" value=\"{request_id}\"><label>Access key <input type=\"password\" name=\"access_key\" autocomplete=\"off\" required></label><button type=\"submit\" name=\"decision\" value=\"approve\">Authorize</button><button type=\"submit\" name=\"decision\" value=\"deny\" formnovalidate>Cancel</button></form>"
    );
    let cookie = format!(
        "plasmite_oauth={csrf}; Secure; HttpOnly; SameSite=Lax; Path=/oauth/approve; Max-Age={CODE_LIFETIME}"
    );
    let mut response = Html(page).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("hex cookie"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn approve(
    State(oauth): State<Arc<OauthService>>,
    headers: HeaderMap,
    Form(form): Form<Approval>,
) -> Response {
    if !origin_matches(&headers, &oauth.issuer) {
        return oauth_error(StatusCode::FORBIDDEN, "invalid_request");
    }
    let pending = match oauth.pending.lock() {
        Ok(mut state) => state.remove(&form.request),
        Err(_) => return server_error(),
    };
    let Some(pending) = pending else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if pending.expires_at <= now()
        || cookie(&headers, "plasmite_oauth") != Some(pending.csrf.as_str())
    {
        return oauth_error(StatusCode::FORBIDDEN, "invalid_request");
    }
    if form.decision.as_deref() == Some("deny") {
        let mut target = Url::parse(&pending.redirect_uri).expect("registered redirect URI");
        target
            .query_pairs_mut()
            .append_pair("error", "access_denied");
        target.query_pairs_mut().append_pair("iss", &oauth.issuer);
        if let Some(state) = pending.state {
            target.query_pairs_mut().append_pair("state", &state);
        }
        return Redirect::to(target.as_str()).into_response();
    }
    if !matches!(form.decision.as_deref(), None | Some("approve")) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let Some((key_id, _grant)) = form
        .access_key
        .as_deref()
        .and_then(|key| oauth.access.authorize_key(key.trim()))
    else {
        return oauth_error(StatusCode::UNAUTHORIZED, "access_denied");
    };
    let code = match random_hex() {
        Ok(code) => code,
        Err(response) => return response,
    };
    let record = Code {
        hash: digest(&code),
        client_id: pending.client_id,
        redirect_uri: pending.redirect_uri.clone(),
        challenge: pending.challenge,
        resource: pending.resource,
        key_id,
        expires_at: now() + CODE_LIFETIME,
    };
    if let Err(response) = oauth.change(|state| {
        if state.codes.len() >= MAX_CLIENTS {
            return Err(oauth_error(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
            ));
        }
        let Some(client) = state
            .clients
            .iter_mut()
            .find(|client| client.id == record.client_id)
        else {
            return Err(oauth_error(StatusCode::BAD_REQUEST, "invalid_client"));
        };
        client.expires_at = None;
        client.key_id = Some(record.key_id.clone());
        state.codes.push(record);
        Ok(())
    }) {
        return response;
    }
    let mut target = Url::parse(&pending.redirect_uri).expect("registered redirect URI");
    target.query_pairs_mut().append_pair("code", &code);
    target.query_pairs_mut().append_pair("iss", &oauth.issuer);
    if let Some(state) = pending.state {
        target.query_pairs_mut().append_pair("state", &state);
    }
    let mut response = Redirect::to(target.as_str()).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "plasmite_oauth=; Secure; HttpOnly; SameSite=Lax; Path=/oauth/approve; Max-Age=0",
        ),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn token(
    State(oauth): State<Arc<OauthService>>,
    Form(request): Form<TokenRequest>,
) -> Response {
    if request.resource.as_deref() != Some(&oauth.resource) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_target");
    }
    match request.grant_type.as_str() {
        "authorization_code" => exchange_code(&oauth, request),
        "refresh_token" => refresh(&oauth, request),
        _ => oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type"),
    }
}

fn exchange_code(oauth: &OauthService, request: TokenRequest) -> Response {
    let (Some(code), Some(redirect_uri), Some(verifier)) =
        (request.code, request.redirect_uri, request.code_verifier)
    else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    if !verifier_is_valid(&verifier) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let access_token = match random_hex() {
        Ok(value) => value,
        Err(response) => return response,
    };
    let grant_id = match random_hex() {
        Ok(value) => value,
        Err(response) => return response,
    };
    let refresh_token = match new_refresh(&grant_id) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let result = oauth.change(|state| {
        let Some(position) = state
            .codes
            .iter()
            .position(|entry| entry.hash == digest(&code))
        else {
            return Err(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"));
        };
        let code = state.codes.remove(position);
        if code.expires_at <= now()
            || code.client_id != request.client_id
            || code.redirect_uri != redirect_uri
            || code.resource != oauth.resource
            || !verify_s256(&verifier, &code.challenge)
            || oauth.access.authorize_id(&code.key_id).is_none()
        {
            return Ok(false);
        }
        state
            .grants
            .retain(|grant| grant.client_id != code.client_id);
        state
            .access_tokens
            .retain(|token| state.grants.iter().any(|grant| grant.id == token.grant_id));
        state.grants.push(Grant {
            id: grant_id.clone(),
            client_id: code.client_id,
            resource: code.resource,
            key_id: code.key_id,
            refresh_hash: digest(&refresh_token),
        });
        state.access_tokens.push(AccessToken {
            hash: digest(&access_token),
            grant_id,
            expires_at: now() + ACCESS_LIFETIME,
        });
        Ok(true)
    });
    match result {
        Ok(true) => token_response(access_token, refresh_token),
        Ok(false) => oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"),
        Err(response) => response,
    }
}

fn refresh(oauth: &OauthService, request: TokenRequest) -> Response {
    let Some(refresh_token) = request.refresh_token else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request");
    };
    let access_token = match random_hex() {
        Ok(value) => value,
        Err(response) => return response,
    };
    let next_secret = match random_hex() {
        Ok(value) => value,
        Err(response) => return response,
    };
    let hash = digest(&refresh_token);
    let family_id = refresh_token
        .strip_prefix("rt1.")
        .and_then(|rest| rest.split_once('.'))
        .map(|(id, _)| id);
    let result = oauth.change(|state| {
        let Some(position) = state.grants.iter().position(|grant| {
            Some(grant.id.as_str()) == family_id
                && grant.client_id == request.client_id
                && grant.resource == oauth.resource
        }) else {
            return Err(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"));
        };
        if state.grants[position].refresh_hash != hash {
            let id = state.grants.remove(position).id;
            state.access_tokens.retain(|token| token.grant_id != id);
            return Ok(None);
        }
        let grant = &mut state.grants[position];
        if oauth.access.authorize_id(&grant.key_id).is_none() {
            return Err(oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"));
        }
        let next_refresh = format!("rt1.{}.{next_secret}", grant.id);
        grant.refresh_hash = digest(&next_refresh);
        state
            .access_tokens
            .retain(|token| token.grant_id != grant.id);
        state.access_tokens.push(AccessToken {
            hash: digest(&access_token),
            grant_id: grant.id.clone(),
            expires_at: now() + ACCESS_LIFETIME,
        });
        Ok(Some(next_refresh))
    });
    match result {
        Ok(Some(next_refresh)) => token_response(access_token, next_refresh),
        Ok(None) => oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"),
        Err(response) => response,
    }
}

async fn revoke(
    State(oauth): State<Arc<OauthService>>,
    Form(request): Form<RevokeRequest>,
) -> Response {
    let hash = digest(&request.token);
    match oauth.change(|state| {
        if let Some(position) = state
            .grants
            .iter()
            .position(|grant| grant.client_id == request.client_id && grant.refresh_hash == hash)
        {
            let id = state.grants.remove(position).id;
            state.access_tokens.retain(|token| token.grant_id != id);
        } else if let Some(position) = state
            .access_tokens
            .iter()
            .position(|token| token.hash == hash)
        {
            let id = state.access_tokens[position].grant_id.clone();
            if state
                .grants
                .iter()
                .find(|grant| grant.id == id)
                .is_some_and(|grant| grant.client_id == request.client_id)
            {
                state.grants.retain(|grant| grant.id != id);
                state.access_tokens.retain(|token| token.grant_id != id);
            }
        }
        Ok(())
    }) {
        Ok(()) => StatusCode::OK.into_response(),
        Err(response) => response,
    }
}

fn token_response(access_token: String, refresh_token: String) -> Response {
    let mut response = Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_LIFETIME,
        "refresh_token": refresh_token,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

fn oauth_error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

fn server_error() -> Response {
    oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error")
}

fn valid_redirect(uri: &str) -> bool {
    let Ok(url) = Url::parse(uri) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return false;
    }
    match (url.scheme(), url.host()) {
        ("https", Some(_)) => true,
        ("http", Some(Host::Domain(name))) => name.eq_ignore_ascii_case("localhost"),
        ("http", Some(Host::Ipv4(address))) => address.is_loopback(),
        ("http", Some(Host::Ipv6(address))) => address.is_loopback(),
        _ => false,
    }
}

fn origin_matches(headers: &HeaderMap, issuer: &str) -> bool {
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == issuer)
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| part.trim().strip_prefix(name)?.strip_prefix('='))
}

fn random_hex() -> Result<String, Response> {
    let mut bytes = [0u8; 32];
    fill(&mut bytes).map_err(|_| server_error())?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn new_refresh(grant_id: &str) -> Result<String, Response> {
    Ok(format!("rt1.{grant_id}.{}", random_hex()?))
}

fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower_service::Service;

    const VALID_REGISTRATION: &str = r#"{"client_name":"Security regression","redirect_uris":["https://client.example/callback"]}"#;

    fn registration_request(peer: SocketAddr, body: &str) -> Request {
        let mut request = Request::builder()
            .method("POST")
            .uri("/oauth/register")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    fn service() -> (tempfile::TempDir, Arc<OauthService>) {
        let directory = tempfile::tempdir().unwrap();
        let access = Arc::new(
            AccessStore::open(directory.path(), Some("https://localhost:9743"), None, None)
                .unwrap(),
        );
        let service = OauthService::open(access).unwrap().unwrap();
        (directory, service)
    }

    async fn register_client(oauth: &Arc<OauthService>, peer: SocketAddr) -> String {
        let response = register(
            State(oauth.clone()),
            ConnectInfo(peer),
            registration_request(peer, VALID_REGISTRATION),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["client_id"]
            .as_str()
            .unwrap()
            .into()
    }

    async fn page(oauth: &Arc<OauthService>, peer: SocketAddr, client: &str) -> Response {
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("response_type", "code")
            .append_pair("client_id", client)
            .append_pair("redirect_uri", "https://client.example/callback")
            .append_pair("code_challenge", &"A".repeat(43))
            .append_pair("code_challenge_method", "S256")
            .append_pair("resource", &oauth.resource)
            .finish();
        authorize_page(
            State(oauth.clone()),
            ConnectInfo(peer),
            RawQuery(Some(query)),
        )
        .await
    }

    #[tokio::test]
    async fn one_public_client_cannot_fill_the_authorization_queue() {
        let (_directory, oauth) = service();
        let peer = "192.0.2.1:1234".parse().unwrap();
        let client = register_client(&oauth, peer).await;
        for _ in 0..MAX_PENDING_PER_CLIENT {
            assert_eq!(page(&oauth, peer, &client).await.status(), StatusCode::OK);
        }
        assert_eq!(
            page(&oauth, peer, &client).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let other_client = register_client(&oauth, peer).await;
        assert_eq!(
            page(&oauth, peer, &other_client).await.status(),
            StatusCode::OK
        );
        assert_eq!(
            oauth.pending.lock().unwrap().len(),
            MAX_PENDING_PER_CLIENT + 1
        );
    }

    #[tokio::test]
    async fn one_source_cannot_fill_the_queue_with_many_clients() {
        let (_directory, oauth) = service();
        let peer = "192.0.2.1:1234".parse().unwrap();
        for _ in 0..MAX_PENDING_PER_SOURCE / MAX_PENDING_PER_CLIENT {
            // Model requests in later rate-limit windows without waiting minutes.
            oauth.request_windows.lock().unwrap().clear();
            let client = register_client(&oauth, peer).await;
            for _ in 0..MAX_PENDING_PER_CLIENT {
                assert_eq!(page(&oauth, peer, &client).await.status(), StatusCode::OK);
            }
        }
        oauth.request_windows.lock().unwrap().clear();
        let client = register_client(&oauth, peer).await;
        assert_eq!(
            page(&oauth, peer, &client).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let other_peer = "192.0.2.2:1234".parse().unwrap();
        assert_eq!(
            page(&oauth, other_peer, &client).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn public_registration_and_authorization_share_a_rate_limit() {
        let (_directory, oauth) = service();
        let peer = "192.0.2.1:1234".parse().unwrap();
        // Even malformed requests consume the public endpoint budget.
        for _ in 0..PUBLIC_REQUESTS_PER_MINUTE {
            assert_eq!(
                authorize_page(State(oauth.clone()), ConnectInfo(peer), RawQuery(None))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            authorize_page(State(oauth.clone()), ConnectInfo(peer), RawQuery(None))
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let response = register(
            State(oauth),
            ConnectInfo(peer),
            registration_request(peer, VALID_REGISTRATION),
        )
        .await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn malformed_registration_requests_use_the_public_rate_limit() {
        let (_directory, oauth) = service();
        let peer = "192.0.2.1:1234".parse().unwrap();
        let mut router = oauth.router();

        for _ in 0..PUBLIC_REQUESTS_PER_MINUTE {
            let response = Service::call(&mut router, registration_request(peer, "{"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        let response = Service::call(&mut router, registration_request(peer, "{"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
