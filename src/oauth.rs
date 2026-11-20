//! OAuth authorization for the direct HTTPS MCP endpoint.
//!
//! The access key remains the authority. OAuth records hold only its stable key ID,
//! so a key revocation stops every token and an in-flight MCP operation.

#[path = "oauth/cimd.rs"]
mod cimd;

use axum::extract::{ConnectInfo, Form, RawQuery, State};
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

pub(crate) struct OauthService {
    issuer: String,
    resource: String,
    access: Arc<AccessStore>,
    path: PathBuf,
    state: Mutex<OAuthState>,
    pending: Mutex<HashMap<String, Pending>>,
    request_windows: Mutex<HashMap<IpAddr, (u64, u16)>>,
    metadata: cimd::Resolver,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct OAuthState {
    // Keep older registration records intact for a non-destructive upgrade.
    // They no longer authorize a new sign-in or a refresh.
    #[serde(default, rename = "clients", skip_serializing_if = "Vec::is_empty")]
    legacy_clients: Vec<serde_json::Value>,
    codes: Vec<Code>,
    grants: Vec<Grant>,
    access_tokens: Vec<AccessToken>,
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
    #[serde(default)]
    permits_refresh: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct Grant {
    id: String,
    client_id: String,
    resource: String,
    key_id: String,
    refresh_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cimd_redirect: Option<String>,
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
    permits_refresh: bool,
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
    client_secret: Option<String>,
    client_assertion: Option<String>,
    client_assertion_type: Option<String>,
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
            state: Mutex::new(state),
            pending: Mutex::new(HashMap::new()),
            request_windows: Mutex::new(HashMap::new()),
            metadata: cimd::Resolver::default(),
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
        "revocation_endpoint": format!("{issuer}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "authorization_response_iss_parameter_supported": true,
        "client_id_metadata_document_supported": true,
        "token_endpoint_auth_methods_supported": ["none"]
    }))
    .into_response()
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
    if !is_cimd(client_id) || [oauth.issuer.as_str(), oauth.resource.as_str()].contains(&client_id)
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client");
    }
    let client = match oauth.metadata.lookup(client_id).await {
        Ok(client) if client.allows_redirect(redirect_uri) => client,
        Ok(_) => return oauth_error(StatusCode::BAD_REQUEST, "invalid_request"),
        Err(error) => return metadata_error(&oauth, client_id, error),
    };
    let client_host = cimd::Document::id_host(client_id);
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
        client_id: client_id.to_owned(),
        client_name: client.name().to_owned(),
        redirect_uri: redirect_uri.to_owned(),
        challenge: challenge.to_owned(),
        resource: oauth.resource.clone(),
        state: request.state.map(|state| state.into_owned()),
        csrf: csrf.clone(),
        expires_at: now() + CODE_LIFETIME,
        permits_refresh: client.permits_refresh(),
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
            .filter(|entry| entry.client_id == pending.client_id && entry.source == pending.source)
            .count()
            >= MAX_PENDING_PER_CLIENT
    {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable");
    }
    let name = html(&pending.client_name);
    let resource = html(&pending.resource);
    let callback = html(&pending.redirect_uri);
    let client_origin = client_host
        .map(|host| {
            format!(
                "<p>The client metadata comes from <strong>{}</strong>.</p>",
                html(&host)
            )
        })
        .unwrap_or_default();
    state.insert(request_id.clone(), pending);
    let page = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Authorize MCP access</title><h1>Authorize MCP access</h1><p><strong>{name}</strong> requests access to the pools at {resource}.</p>{client_origin}<p>The client name is supplied by the client and does not prove its identity. The response will go to {callback}.</p><form method=\"post\" action=\"/oauth/approve\"><input type=\"hidden\" name=\"request\" value=\"{request_id}\"><label>Access key <input type=\"password\" name=\"access_key\" autocomplete=\"off\" required></label><button type=\"submit\" name=\"decision\" value=\"approve\">Authorize</button><button type=\"submit\" name=\"decision\" value=\"deny\" formnovalidate>Cancel</button></form>"
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
    response.headers_mut().insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
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
    if form.decision.as_deref() != Some("approve") {
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
        permits_refresh: pending.permits_refresh,
    };
    if let Err(response) = oauth.change(|state| {
        if state.codes.len() >= MAX_CLIENTS {
            return Err(oauth_error(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
            ));
        }
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
    headers: HeaderMap,
    Form(request): Form<TokenRequest>,
) -> Response {
    if headers.contains_key(header::AUTHORIZATION)
        || request.client_secret.is_some()
        || request.client_assertion.is_some()
        || request.client_assertion_type.is_some()
    {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client");
    }
    if request.resource.as_deref() != Some(&oauth.resource) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_target");
    }
    if !is_cimd(&request.client_id) {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_client");
    }
    match request.grant_type.as_str() {
        "authorization_code" => exchange_code(&oauth, request),
        "refresh_token" => refresh(&oauth, request).await,
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
            return Ok(None);
        }
        state
            .grants
            .retain(|grant| grant.client_id != code.client_id || grant.key_id != code.key_id);
        if state
            .grants
            .iter()
            .filter(|grant| grant.cimd_redirect.is_some())
            .count()
            >= MAX_CLIENTS
        {
            return Err(oauth_error(
                StatusCode::TOO_MANY_REQUESTS,
                "temporarily_unavailable",
            ));
        }
        state
            .access_tokens
            .retain(|token| state.grants.iter().any(|grant| grant.id == token.grant_id));
        state.grants.push(Grant {
            id: grant_id.clone(),
            client_id: code.client_id,
            resource: code.resource,
            key_id: code.key_id,
            refresh_hash: if code.permits_refresh {
                digest(&refresh_token)
            } else {
                String::new()
            },
            cimd_redirect: Some(code.redirect_uri),
        });
        state.access_tokens.push(AccessToken {
            hash: digest(&access_token),
            grant_id,
            expires_at: now() + ACCESS_LIFETIME,
        });
        Ok(Some(code.permits_refresh))
    });
    match result {
        Ok(Some(permits_refresh)) => {
            token_response(access_token, permits_refresh.then_some(refresh_token))
        }
        Ok(None) => oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"),
        Err(response) => response,
    }
}

async fn refresh(oauth: &OauthService, request: TokenRequest) -> Response {
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
    let redirect = match oauth.state.lock() {
        Ok(state) => state
            .grants
            .iter()
            .find(|grant| {
                Some(grant.id.as_str()) == family_id
                    && grant.client_id == request.client_id
                    && grant.resource == oauth.resource
                    && grant.refresh_hash == hash
                    && oauth.access.authorize_id(&grant.key_id).is_some()
            })
            .and_then(|grant| grant.cimd_redirect.clone()),
        Err(_) => return server_error(),
    };
    if let Some(redirect) = redirect {
        match oauth.metadata.lookup(&request.client_id).await {
            Ok(client) if client.permits_refresh() && client.allows_redirect(&redirect) => {}
            Ok(_) => return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant"),
            Err(error) => return metadata_error(oauth, &request.client_id, error),
        }
    }
    let result = oauth.change(|state| {
        let Some(position) = state.grants.iter().position(|grant| {
            Some(grant.id.as_str()) == family_id
                && grant.client_id == request.client_id
                && grant.resource == oauth.resource
                && grant.cimd_redirect.is_some()
                && !grant.refresh_hash.is_empty()
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
        Ok(Some(next_refresh)) => token_response(access_token, Some(next_refresh)),
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

fn token_response(access_token: String, refresh_token: Option<String>) -> Response {
    let mut payload = json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": ACCESS_LIFETIME,
    });
    if let Some(refresh_token) = refresh_token {
        payload["refresh_token"] = json!(refresh_token);
    }
    let mut response = Json(payload).into_response();
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

fn is_cimd(client_id: &str) -> bool {
    client_id.starts_with("https://")
}

fn metadata_error(oauth: &OauthService, client_id: &str, error: cimd::MetadataError) -> Response {
    match error {
        cimd::MetadataError::Unavailable => {
            oauth_error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
        }
        cimd::MetadataError::Invalid => oauth_error(StatusCode::BAD_REQUEST, "invalid_client"),
        cimd::MetadataError::IdentityMismatch => {
            if let Err(response) = oauth.change(|state| {
                state.codes.retain(|code| code.client_id != client_id);
                state.grants.retain(|grant| grant.client_id != client_id);
                state
                    .access_tokens
                    .retain(|token| state.grants.iter().any(|grant| grant.id == token.grant_id));
                Ok(())
            }) {
                return response;
            }
            oauth_error(StatusCode::BAD_REQUEST, "invalid_client")
        }
    }
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
    use axum::extract::Request;
    use tower_service::Service;

    const CLIENT_DOCUMENT: &str = "https://client.example/oauth/client.json";
    const CALLBACK: &str = "http://localhost:45123/callback";
    const VERIFIER: &str = "a0123456789abcdefghijklmnopqrstuvwxyzABCDEF";

    fn encoded(pairs: &[(&str, &str)]) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs.iter().copied())
            .finish()
    }

    async fn response_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn oauth_request(
        oauth: &Arc<OauthService>,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: String,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(path);
        for (key, value) in headers {
            request = request.header(*key, *value);
        }
        let mut request = request.body(Body::from(body)).unwrap();
        request.extensions_mut().insert(ConnectInfo(
            "192.0.2.42:1234".parse::<SocketAddr>().unwrap(),
        ));
        Service::call(&mut oauth.clone().router(), request)
            .await
            .unwrap()
    }

    async fn authorize_fixture(
        oauth: &Arc<OauthService>,
        redirect: &str,
        resource: &str,
    ) -> Response {
        let challenge = oauth_as::pkce::code_challenge_s256(VERIFIER);
        let query = encoded(&[
            ("response_type", "code"),
            ("client_id", CLIENT_DOCUMENT),
            ("redirect_uri", redirect),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("resource", resource),
            ("state", "fixture-state"),
        ]);
        oauth_request(
            oauth,
            "GET",
            &format!("/oauth/authorize?{query}"),
            &[],
            String::new(),
        )
        .await
    }

    async fn submit_fixture_approval(
        oauth: &Arc<OauthService>,
        key: &str,
        decision: &str,
        origin: &str,
    ) -> Response {
        let page = authorize_fixture(oauth, CALLBACK, &oauth.resource).await;
        assert_eq!(page.status(), StatusCode::OK);
        let cookie = page.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let page = axum::body::to_bytes(page.into_body(), 16 * 1024)
            .await
            .unwrap();
        let page = std::str::from_utf8(&page).unwrap();
        assert!(page.contains("Fixture &lt;client&gt;"));
        assert!(page.contains("client.example"));
        assert!(page.contains(CALLBACK));
        assert!(page.contains(&oauth.resource));
        let id = page
            .split("name=\"request\" value=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        oauth_request(
            oauth,
            "POST",
            "/oauth/approve",
            &[
                ("origin", origin),
                ("cookie", &cookie),
                ("content-type", "application/x-www-form-urlencoded"),
            ],
            encoded(&[("request", id), ("access_key", key), ("decision", decision)]),
        )
        .await
    }

    async fn approve_fixture(oauth: &Arc<OauthService>, key: &str) -> String {
        let approval = submit_fixture_approval(oauth, key, "approve", &oauth.issuer).await;
        assert_eq!(approval.status(), StatusCode::SEE_OTHER);
        let target = Url::parse(approval.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let values: HashMap<_, _> = target.query_pairs().into_owned().collect();
        assert_eq!(values["iss"], oauth.issuer);
        assert_eq!(values["state"], "fixture-state");
        assert_eq!(
            target.origin().ascii_serialization(),
            "http://localhost:45123"
        );
        values["code"].clone()
    }

    async fn token_fixture(oauth: &Arc<OauthService>, data: &[(&str, &str)]) -> Response {
        oauth_request(
            oauth,
            "POST",
            "/oauth/token",
            &[("content-type", "application/x-www-form-urlencoded")],
            encoded(data),
        )
        .await
    }

    async fn exchange_fixture_code(
        oauth: &Arc<OauthService>,
        code: &str,
        redirect: &str,
        verifier: &str,
    ) -> Response {
        token_fixture(
            oauth,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", CLIENT_DOCUMENT),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("resource", &oauth.resource),
            ],
        )
        .await
    }

    async fn exchange_fixture(oauth: &Arc<OauthService>, key: &str) -> serde_json::Value {
        let code = approve_fixture(oauth, key).await;
        let reply = exchange_fixture_code(oauth, &code, CALLBACK, VERIFIER).await;
        assert_eq!(reply.status(), StatusCode::OK);
        let replay = exchange_fixture_code(oauth, &code, CALLBACK, VERIFIER).await;
        assert_eq!(response_json(replay).await["error"], "invalid_grant");
        response_json(reply).await
    }

    async fn assert_pool_call_over_trusted_https(
        oauth: &Arc<OauthService>,
        listener: tokio::net::TcpListener,
        directory: &std::path::Path,
        bearer: &str,
    ) {
        use super::super::{ServeConfig, prepare_server_with_access, serve_tls};
        use rustls::pki_types::{CertificateDer, pem::PemObject};
        use tokio::sync::watch;

        let config = ServeConfig {
            bind: listener.local_addr().unwrap(),
            pool_dir: directory.to_owned(),
            tls_cert: Some(oauth.access.cert_path()),
            tls_key: Some(oauth.access.key_path()),
            max_body_bytes: 1024 * 1024,
            max_tail_timeout_ms: 30_000,
            max_concurrent_tails: 2,
        };
        let (stop, mut shutdown) = watch::channel(false);
        let server = prepare_server_with_access(
            &config,
            Some(oauth.access.clone()),
            false,
            Default::default(),
            shutdown.clone(),
            Default::default(),
        )
        .await
        .unwrap();
        let serve = tokio::spawn(serve_tls(
            listener,
            server.app,
            server.tls_config.unwrap(),
            async move {
                let _ = shutdown.changed().await;
            },
        ));
        let cert = CertificateDer::pem_file_iter(config.tls_cert.unwrap())
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        let resource = oauth.resource.clone();
        let bearer = bearer.to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(cert).unwrap();
            let tls = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let agent = ureq::AgentBuilder::new()
                .try_proxy_from_env(false)
                .tls_config(Arc::new(tls))
                .timeout(std::time::Duration::from_secs(3))
                .build();
            let call = |token: Option<&str>, method: &str, name: Option<&str>, mut params: serde_json::Value| {
                params["_meta"] = json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                });
                let mut request = agent.post(&resource)
                    .set("MCP-Protocol-Version", "2026-07-28").set("MCP-Method", method)
                    .set("Accept", "application/json, text/event-stream");
                if let Some(token) = token {
                    request = request.set("Authorization", &format!("Bearer {token}"));
                }
                if let Some(name) = name {
                    request = request.set("MCP-Name", name);
                }
                request.send_json(json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}))
            };
            let feed = json!({"name":"plasmite_feed","arguments":{"pool":"fixture","create":true,"data":{"fixture":1}}});
            for token in [None, Some("invalid fixture bearer")] {
                assert!(matches!(call(token, "tools/call", Some("plasmite_feed"), feed.clone()), Err(ureq::Error::Status(401, _))));
            }
            let result: serde_json::Value = call(Some(&bearer), "server/discover", None, json!({})).unwrap().into_json().unwrap();
            assert!(result["result"]["supportedVersions"].as_array().unwrap().contains(&json!("2026-07-28")));
            let result: serde_json::Value = call(Some(&bearer), "tools/call", Some("plasmite_feed"), feed).unwrap().into_json().unwrap();
            assert_eq!(result["result"]["structuredContent"]["message"]["seq"], 1);
            let result: serde_json::Value = call(Some(&bearer), "tools/call", Some("plasmite_fetch"),
                json!({"name":"plasmite_fetch","arguments":{"pool":"fixture","since":"oldest"}})).unwrap().into_json().unwrap();
            assert_eq!(result["result"]["structuredContent"]["messages"][0]["data"]["fixture"], 1);
            use base64::{Engine as _, engine::general_purpose::STANDARD};
            let uri = "plasmite:///pools/fixture";
            let name = format!("=?base64?{}?=", STANDARD.encode(uri));
            let result: serde_json::Value = call(Some(&bearer), "resources/read", Some(&name), json!({"uri":uri})).unwrap().into_json().unwrap();
            assert_eq!(result["result"]["contents"][0]["uri"], uri);

            let subscription = json!({
                "name":"pool.message","arguments":{"pool":"fixture"}
            });
            assert!(matches!(call(None, "events/stream", Some("pool.message"), subscription.clone()),
                Err(ureq::Error::Status(401, _))));
            let stream = call(Some(&bearer), "events/stream", Some("pool.message"), subscription).unwrap();
            assert_eq!(stream.header("Content-Type"), Some("text/event-stream"));
            use std::io::BufRead;
            let mut stream = std::io::BufReader::new(stream.into_reader());
            let mut activation = String::new();
            stream.read_line(&mut activation).unwrap();
            assert!(activation.contains("notifications/events/active"));
            drop(stream);

            let legacy_read = json!({"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":uri}});
            let legacy_request = || agent.post(&resource)
                .set("MCP-Protocol-Version", "2025-11-25")
                .set("Accept", "application/json, text/event-stream");
            assert!(matches!(legacy_request().send_json(legacy_read.clone()), Err(ureq::Error::Status(401, _))));
            let result: serde_json::Value = legacy_request()
                .set("Authorization", &format!("Bearer {bearer}"))
                .send_json(legacy_read).unwrap().into_json().unwrap();
            assert_eq!(result["result"]["contents"][0]["uri"], uri);
            assert!(result["result"].get("resultType").is_none());
        })
        .await;
        stop.send(true).unwrap();
        serve.await.unwrap().unwrap();
        result.unwrap();
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

    #[tokio::test]
    async fn metadata_client_uses_browser_authorization_and_independent_key_grants() {
        let directory = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("https://{}", listener.local_addr().unwrap());
        let access =
            Arc::new(AccessStore::open(directory.path(), Some(&issuer), None, None).unwrap());
        let mut oauth = OauthService::open(access).unwrap().unwrap();
        let wrong_identity = Arc::new(std::sync::atomic::AtomicBool::new(false));
        with_fixture_identity(&mut oauth, wrong_identity.clone());
        let discovery = oauth_request(
            &oauth,
            "GET",
            "/.well-known/oauth-authorization-server",
            &[],
            String::new(),
        )
        .await;
        let metadata = response_json(discovery).await;
        assert_eq!(metadata["issuer"], oauth.issuer);
        assert_eq!(metadata["client_id_metadata_document_supported"], true);
        assert!(metadata.get("registration_endpoint").is_none());

        let bad_redirect =
            authorize_fixture(&oauth, "http://localhost:45123/other", &oauth.resource).await;
        assert_eq!(bad_redirect.status(), StatusCode::BAD_REQUEST);
        assert!(!bad_redirect.headers().contains_key(header::LOCATION));
        assert_eq!(
            authorize_fixture(&oauth, CALLBACK, &oauth.issuer)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );

        let key_a = oauth.access.issue("fixture A").unwrap();
        let key_b = oauth.access.issue("fixture B").unwrap();
        assert_eq!(
            submit_fixture_approval(&oauth, &key_a, "approve", "https://other.example")
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            submit_fixture_approval(&oauth, "invalid key", "approve", &oauth.issuer)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        let denied = submit_fixture_approval(&oauth, "", "deny", &oauth.issuer).await;
        let denied = Url::parse(denied.headers()[header::LOCATION].to_str().unwrap()).unwrap();
        let denied_params: HashMap<_, _> = denied.query_pairs().into_owned().collect();
        assert_eq!(denied_params["error"], "access_denied");
        assert_eq!(denied_params["iss"], oauth.issuer);
        assert_eq!(denied_params["state"], "fixture-state");
        assert!(!denied_params.contains_key("code"));
        let code = approve_fixture(&oauth, &key_a).await;
        let wrong_callback =
            exchange_fixture_code(&oauth, &code, "http://localhost:45124/callback", VERIFIER).await;
        assert_eq!(
            response_json(wrong_callback).await["error"],
            "invalid_grant"
        );
        let code = approve_fixture(&oauth, &key_a).await;
        let wrong_verifier = exchange_fixture_code(&oauth, &code, CALLBACK, &"B".repeat(43)).await;
        assert_eq!(
            response_json(wrong_verifier).await["error"],
            "invalid_grant"
        );
        let first = exchange_fixture(&oauth, &key_a).await;
        let second = exchange_fixture(&oauth, &key_b).await;
        let first_bearer = first["access_token"].as_str().unwrap();
        let second_bearer = second["access_token"].as_str().unwrap();
        assert!(oauth.grant(first_bearer).is_some());
        assert!(oauth.grant(second_bearer).is_some());
        assert_pool_call_over_trusted_https(&oauth, listener, directory.path(), first_bearer).await;
        assert!(
            oauth.state.lock().unwrap().legacy_clients.is_empty(),
            "CIMD does not perform DCR"
        );

        let mut reopened = OauthService::open(oauth.access.clone()).unwrap().unwrap();
        with_fixture_identity(&mut reopened, wrong_identity.clone());
        drop(oauth);
        let oauth = reopened;
        assert!(
            oauth.grant(first_bearer).is_some(),
            "saved grant survives reopening"
        );
        let mut moved = OauthService::open(oauth.access.clone()).unwrap().unwrap();
        Arc::get_mut(&mut moved).unwrap().resource = "https://different.example/mcp".into();
        assert!(
            moved.grant(first_bearer).is_none(),
            "grant cannot move to a new resource"
        );
        let refresh_token = first["refresh_token"].as_str().unwrap();
        let refresh_data = [
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_DOCUMENT),
            ("refresh_token", refresh_token),
            ("resource", &oauth.resource),
        ];
        let mut wrong_target = refresh_data;
        wrong_target[3].1 = "https://issuer.example/different";
        let wrong = token_fixture(&oauth, &wrong_target).await;
        assert_eq!(response_json(wrong).await["error"], "invalid_target");
        let refreshed = token_fixture(&oauth, &refresh_data).await;
        assert_eq!(refreshed.status(), StatusCode::OK);
        let refreshed = response_json(refreshed).await;
        let refreshed_bearer = refreshed["access_token"].as_str().unwrap();
        assert_ne!(refreshed["refresh_token"], first["refresh_token"]);
        assert!(oauth.grant(first_bearer).is_none());
        assert!(oauth.grant(refreshed_bearer).is_some());

        let replay = token_fixture(&oauth, &refresh_data).await;
        assert_eq!(response_json(replay).await["error"], "invalid_grant");
        assert!(
            oauth.grant(refreshed_bearer).is_none(),
            "stale refresh revokes the family"
        );
        let fresh = exchange_fixture(&oauth, &key_a).await;
        let bearer = fresh["access_token"].as_str().unwrap();
        for client in ["https://unrelated.example/client.json", CLIENT_DOCUMENT] {
            let response = oauth_request(
                &oauth,
                "POST",
                "/oauth/revoke",
                &[("content-type", "application/x-www-form-urlencoded")],
                encoded(&[("client_id", client), ("token", bearer)]),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(oauth.grant(bearer).is_some(), client != CLIENT_DOCUMENT);
        }
        assert!(
            oauth.grant(second_bearer).is_some(),
            "revoking one key grant does not affect another"
        );
        let active = exchange_fixture(&oauth, &key_a).await;
        let (key_id, _) = oauth.access.authorize_key(&key_a).unwrap();
        oauth.access.revoke(&key_id).unwrap();
        assert!(
            oauth
                .grant(active["access_token"].as_str().unwrap())
                .is_none()
        );
        assert!(oauth.grant(second_bearer).is_some());
        let revoked_refresh = token_fixture(
            &oauth,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_DOCUMENT),
                ("refresh_token", active["refresh_token"].as_str().unwrap()),
                ("resource", &oauth.resource),
            ],
        )
        .await;
        assert_eq!(
            response_json(revoked_refresh).await["error"],
            "invalid_grant"
        );

        wrong_identity.store(true, std::sync::atomic::Ordering::Relaxed);
        let second_refresh = token_fixture(
            &oauth,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", CLIENT_DOCUMENT),
                ("refresh_token", second["refresh_token"].as_str().unwrap()),
                ("resource", &oauth.resource),
            ],
        )
        .await;
        assert_eq!(
            response_json(second_refresh).await["error"],
            "invalid_client"
        );
        assert!(oauth.grant(second_bearer).is_none());
    }

    fn with_fixture_identity(
        oauth: &mut Arc<OauthService>,
        wrong: Arc<std::sync::atomic::AtomicBool>,
    ) {
        Arc::get_mut(oauth).unwrap().metadata = cimd::Resolver::with_fixture(move |id| {
            json!({
            "client_id": if wrong.load(std::sync::atomic::Ordering::Relaxed) { "https://other.example/document.json" } else { id },
            "client_name": "Fixture <client>",
            "redirect_uris": ["http://localhost/callback"],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"]
        }).to_string().into_bytes()
        });
    }

    fn with_public_metadata(oauth: &mut Arc<OauthService>) {
        Arc::get_mut(oauth).unwrap().metadata = cimd::Resolver::with_fixture(|id| {
            json!({
                "client_id": id,
                "client_name": "Test client",
                "redirect_uris": ["https://client.example/callback"],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"]
            })
            .to_string()
            .into_bytes()
        });
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
    async fn one_source_cannot_fill_a_shared_client_queue_for_others() {
        let (_directory, mut oauth) = service();
        with_public_metadata(&mut oauth);
        let peer = "192.0.2.1:1234".parse().unwrap();
        for _ in 0..MAX_PENDING_PER_CLIENT {
            assert_eq!(
                page(&oauth, peer, CLIENT_DOCUMENT).await.status(),
                StatusCode::OK
            );
        }
        assert_eq!(
            page(&oauth, peer, CLIENT_DOCUMENT).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let another_peer = "192.0.2.2:1234".parse().unwrap();
        assert_eq!(
            page(&oauth, another_peer, CLIENT_DOCUMENT).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn one_source_cannot_fill_the_queue_with_many_clients() {
        let (_directory, mut oauth) = service();
        with_public_metadata(&mut oauth);
        let peer = "192.0.2.1:1234".parse().unwrap();
        for index in 0..MAX_PENDING_PER_SOURCE / MAX_PENDING_PER_CLIENT {
            // Model requests in later rate-limit windows without waiting minutes.
            oauth.request_windows.lock().unwrap().clear();
            let client = format!("https://client.example/document/{index}");
            for _ in 0..MAX_PENDING_PER_CLIENT {
                assert_eq!(page(&oauth, peer, &client).await.status(), StatusCode::OK);
            }
        }
        oauth.request_windows.lock().unwrap().clear();
        assert_eq!(
            page(&oauth, peer, CLIENT_DOCUMENT).await.status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        let other_peer = "192.0.2.2:1234".parse().unwrap();
        assert_eq!(
            page(&oauth, other_peer, CLIENT_DOCUMENT).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn malformed_authorization_requests_are_rate_limited() {
        let (_directory, oauth) = service();
        let peer = "192.0.2.1:1234".parse().unwrap();
        for _ in 0..PUBLIC_REQUESTS_PER_MINUTE {
            assert_eq!(
                authorize_page(State(oauth.clone()), ConnectInfo(peer), RawQuery(None))
                    .await
                    .status(),
                StatusCode::BAD_REQUEST
            );
        }
        assert_eq!(
            authorize_page(State(oauth), ConnectInfo(peer), RawQuery(None))
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn upgrade_preserves_existing_access_until_expiry_or_revocation_without_legacy_sign_in() {
        let (_directory, oauth) = service();
        let client = "unused-old-client-id";
        let previous = json!({ "id": client, "name": "Historical test record", "redirect_uris": ["https://client.example/callback"], "expires_at": null });
        let key = oauth.access.issue("historical key").unwrap();
        let (key_id, _) = oauth.access.authorize_key(&key).unwrap();
        let refresh_token = "rt1.test-grant.synthetic";
        oauth
            .change(|state| {
                state.legacy_clients.push(previous.clone());
                state.grants.push(Grant {
                    id: "test-grant".into(),
                    client_id: client.into(),
                    resource: oauth.resource.clone(),
                    key_id: key_id.clone(),
                    refresh_hash: digest(refresh_token),
                    cimd_redirect: None,
                });
                for (bearer, expires_at) in [("valid", now() + 60), ("expired", now() - 1)] {
                    state.access_tokens.push(AccessToken {
                        hash: digest(bearer),
                        grant_id: "test-grant".into(),
                        expires_at,
                    });
                }
                Ok(())
            })
            .unwrap();

        let reopened = OauthService::open(oauth.access.clone()).unwrap().unwrap();
        assert!(reopened.grant("valid").is_some());
        assert!(reopened.grant("expired").is_none());
        assert_eq!(
            reopened.state.lock().unwrap().legacy_clients,
            vec![previous.clone()]
        );
        assert_eq!(
            page(&reopened, "192.0.2.1:1234".parse().unwrap(), client)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        let reply = token_fixture(
            &reopened,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", client),
                ("refresh_token", refresh_token),
                ("resource", &reopened.resource),
            ],
        )
        .await;
        assert_eq!(response_json(reply).await["error"], "invalid_client");
        // Key revocation immediately stops an unexpired bearer after upgrade.
        reopened.access.revoke(&key_id).unwrap();
        assert!(reopened.grant("valid").is_none());
        reopened
            .change(|state| {
                assert_eq!(state.legacy_clients, vec![previous]);
                Ok(())
            })
            .unwrap();
    }
}
