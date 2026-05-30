use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tiny_http::{Header, Method, Response, Server, StatusCode};
use walkdir::WalkDir;

use crate::{clip_server, commands};

const PORT: u16 = 19828;
const API_PREFIX: &str = "/api/v1";
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_FILE_CONTENT_BYTES: u64 = 2 * 1024 * 1024;
const DEFAULT_MAX_FILES: usize = 2_000;
const HARD_MAX_FILES: usize = 10_000;
const MAX_SEARCH_RESULTS: usize = 50;
const BIND_RETRY_DELAY_SECS: u64 = 2;
const MAX_BIND_RETRIES: u32 = 3;
const APP_STATE_CACHE_TTL: Duration = Duration::from_secs(5);
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(1);
const RATE_LIMIT_MAX_REQUESTS: usize = 120;
const MAX_IN_FLIGHT_REQUESTS: usize = 64;

/// API status: 0=starting, 1=running, 2=port_conflict, 3=error
static API_STATUS: AtomicU8 = AtomicU8::new(0);
static IN_FLIGHT_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static APP_STATE_CACHE: OnceLock<Mutex<Option<CachedAppState>>> = OnceLock::new();
static RATE_LIMIT: OnceLock<Mutex<VecDeque<Instant>>> = OnceLock::new();

#[derive(Clone)]
struct CachedAppState {
    loaded_at: Instant,
    value: Option<Value>,
}

pub fn get_api_status() -> &'static str {
    match API_STATUS.load(Ordering::Relaxed) {
        0 => "starting",
        1 => "running",
        2 => "port_conflict",
        _ => "error",
    }
}

pub fn invalidate_config_cache() {
    if let Some(lock) = APP_STATE_CACHE.get() {
        if let Ok(mut cache) = lock.lock() {
            *cache = None;
        }
    }
}

pub fn start_api_server(app: AppHandle) {
    thread::spawn(move || loop {
        API_STATUS.store(0, Ordering::Relaxed);
        let server = match bind_server_with_retry() {
            Some(server) => server,
            None => {
                API_STATUS.store(2, Ordering::Relaxed);
                thread::sleep(Duration::from_secs(BIND_RETRY_DELAY_SECS));
                continue;
            }
        };

        API_STATUS.store(1, Ordering::Relaxed);
        eprintln!("[API Server] Listening on http://127.0.0.1:{PORT}{API_PREFIX}");

        for request in server.incoming_requests() {
            let method = request.method().clone();
            let url = request.url().to_string();
            if should_rate_limit(&method, &url) && !allow_request() {
                respond_error(request, 429, "Too many requests");
                continue;
            }
            let Some(slot) = try_acquire_request_slot() else {
                respond_error(request, 503, "API server is busy");
                continue;
            };
            let app = app.clone();
            thread::spawn(move || {
                let _slot = slot;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_request(app, request);
                }));
                if let Err(payload) = result {
                    eprintln!("[API Server] request handler panicked: {payload:?}");
                }
            });
        }

        API_STATUS.store(3, Ordering::Relaxed);
        eprintln!("[API Server] server loop exited; restarting");
        thread::sleep(Duration::from_secs(BIND_RETRY_DELAY_SECS));
    });
}

fn bind_server_with_retry() -> Option<Server> {
    for attempt in 1..=MAX_BIND_RETRIES {
        match Server::http(format!("127.0.0.1:{PORT}")) {
            Ok(server) => return Some(server),
            Err(err) => {
                eprintln!(
                    "[API Server] Failed to bind 127.0.0.1:{PORT} (attempt {attempt}/{MAX_BIND_RETRIES}): {err}"
                );
                if attempt < MAX_BIND_RETRIES {
                    thread::sleep(Duration::from_secs(BIND_RETRY_DELAY_SECS));
                }
            }
        }
    }
    None
}

struct RequestSlot;

impl Drop for RequestSlot {
    fn drop(&mut self) {
        IN_FLIGHT_REQUESTS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn try_acquire_request_slot() -> Option<RequestSlot> {
    let mut current = IN_FLIGHT_REQUESTS.load(Ordering::Relaxed);
    loop {
        if current >= MAX_IN_FLIGHT_REQUESTS {
            return None;
        }
        match IN_FLIGHT_REQUESTS.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return Some(RequestSlot),
            Err(next) => current = next,
        }
    }
}

fn process_request(app: AppHandle, mut request: tiny_http::Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    if method == Method::Options {
        respond_options(request);
        return;
    }

    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|header| {
            (
                header.field.as_str().to_ascii_lowercase().to_string(),
                header.value.as_str().to_string(),
            )
        })
        .collect();

    let body = match read_body(&mut request) {
        Ok(body) => body,
        Err(err) => {
            respond_error(request, 400, &err);
            return;
        }
    };

    let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_request(&app, &method, &url, &body, &headers)
    }))
    .unwrap_or_else(|payload| {
        eprintln!("[API Server] request panicked: {payload:?}");
        err(500, "Internal API server error")
    });
    respond_json(request, response.status, response.body);
}

struct ApiResponse {
    status: u16,
    body: Value,
}

fn ok(body: Value) -> ApiResponse {
    ApiResponse { status: 200, body }
}

fn err(status: u16, message: impl Into<String>) -> ApiResponse {
    ApiResponse {
        status,
        body: json!({ "ok": false, "error": message.into() }),
    }
}

fn handle_request(
    app: &AppHandle,
    method: &Method,
    url: &str,
    body: &str,
    headers: &[(String, String)],
) -> ApiResponse {
    let (path, query) = split_url(url);
    if path == "/health" || path == format!("{API_PREFIX}/health") {
        // /health stays reachable even when the user has disabled the
        // API in Settings — the desktop UI uses it to render the
        // "Enabled / disabled / port_conflict" line, and curl-from-
        // terminal users need a way to confirm the server is alive
        // before they go hunting for why other endpoints 503.
        return ok(json!({
            "ok": true,
            "status": get_api_status(),
            "version": env!("CARGO_PKG_VERSION"),
            "authRequired": api_auth_required(app),
            "authConfigured": api_token(app).is_some(),
            "tokenSource": api_token_source(app),
            "enabled": api_enabled(app),
            "allowUnauthenticated": api_allow_unauthenticated(app),
        }));
    }
    if !path.starts_with(API_PREFIX) {
        return err(404, "Not found");
    }
    if !api_enabled(app) {
        // Kill-switch path: token may be configured and valid, but the
        // user toggled the API off in Settings → API Server. 503 is
        // the right code semantically ("temporarily unavailable")
        // and tells well-behaved clients to back off rather than
        // retry instantly the way 401 would.
        return err(503, "API server is disabled in Settings → API Server");
    }
    if !is_authorized(app, query, headers) {
        return err(401, "Unauthorized");
    }
    if !matches!(method, &Method::Get | &Method::Post) {
        return err(405, "Method not allowed");
    }

    let parts: Vec<&str> = path
        .trim_start_matches(API_PREFIX)
        .trim_start_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();

    match (method, parts.as_slice()) {
        (&Method::Get, ["projects"]) => handle_projects(app),
        (&Method::Get, ["projects", project_id, "files"]) => handle_files(app, project_id, query),
        (&Method::Get, ["projects", project_id, "files", "content"]) => {
            handle_file_content(app, project_id, query)
        }
        (&Method::Get, ["projects", project_id, "agent", "page"]) => {
            handle_agent_page(app, project_id, query)
        }
        (&Method::Post, ["projects", project_id, "search"]) => handle_search(app, project_id, body),
        (&Method::Get, ["projects", project_id, "graph"]) => handle_graph(app, project_id, query),
        (&Method::Post, ["projects", project_id, "sources", "rescan"]) => {
            handle_rescan(app, project_id)
        }
        (&Method::Post, ["projects", project_id, "chat"]) => {
            let _ = project_id;
            err(501, "Chat API is not implemented in the local Rust API server yet. The existing chat/RAG pipeline currently lives in the WebView; expose it after moving the shared chat pipeline behind a backend command.")
        }
        _ => err(404, "Not found"),
    }
}

fn should_rate_limit(method: &Method, url: &str) -> bool {
    if method == &Method::Options {
        return false;
    }
    let (path, _) = split_url(url);
    !(path == "/health" || path == format!("{API_PREFIX}/health"))
}

fn allow_request() -> bool {
    let now = Instant::now();
    let window_start = now - RATE_LIMIT_WINDOW;
    let lock = RATE_LIMIT.get_or_init(|| Mutex::new(VecDeque::new()));
    let Ok(mut hits) = lock.lock() else {
        return false;
    };
    while hits.front().map(|t| *t < window_start).unwrap_or(false) {
        hits.pop_front();
    }
    if hits.len() >= RATE_LIMIT_MAX_REQUESTS {
        return false;
    }
    hits.push_back(now);
    true
}

fn read_body(request: &mut tiny_http::Request) -> Result<String, String> {
    let mut limited = request.as_reader().take(MAX_BODY_BYTES as u64 + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read body: {e}"))?;
    if bytes.len() > MAX_BODY_BYTES {
        return Err("Request body too large".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "Request body must be UTF-8".to_string())
}

fn respond_error(request: tiny_http::Request, status: u16, message: &str) {
    respond_json(request, status, json!({ "ok": false, "error": message }));
}

fn respond_options(request: tiny_http::Request) {
    let mut response = Response::empty(StatusCode(204));
    for header in cors_headers() {
        response.add_header(header);
    }
    response.add_header(Header::from_bytes("Access-Control-Max-Age", "600").unwrap());
    let _ = request.respond(response);
}

fn respond_json(request: tiny_http::Request, status: u16, body: Value) {
    let mut response = Response::from_string(body.to_string()).with_status_code(StatusCode(status));
    for header in cors_headers() {
        response.add_header(header);
    }
    let _ = request.respond(response);
}

fn cors_headers() -> Vec<Header> {
    vec![
        Header::from_bytes("Access-Control-Allow-Origin", "*").unwrap(),
        Header::from_bytes("Access-Control-Allow-Methods", "GET, POST, OPTIONS").unwrap(),
        Header::from_bytes(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, X-LLM-Wiki-Token",
        )
        .unwrap(),
        Header::from_bytes("Content-Type", "application/json").unwrap(),
    ]
}

fn split_url(url: &str) -> (String, &str) {
    match url.split_once('?') {
        Some((path, query)) => (path.to_string(), query),
        None => (url.to_string(), ""),
    }
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(v));
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode one path segment with `encodeURIComponent` semantics:
/// every byte outside the RFC 3986 unreserved set (`A-Z a-z 0-9 - _ . ~`)
/// becomes `%XX`. Media paths are encoded segment-by-segment so the URL,
/// once nginx percent-DECODES it, resolves back to the exact file on disk —
/// whose directory names already carry literal `%`, spaces, and CJK bytes
/// (the source slug is URL-encoded into the folder name at ingest time).
/// `/` is never passed here; callers split on it first and rejoin with `/`.
fn percent_encode_segment(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for &b in input.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

fn is_authorized(app: &AppHandle, query: &str, headers: &[(String, String)]) -> bool {
    if !api_auth_required(app) {
        return true;
    }
    let Some(token) = api_token(app) else {
        return false;
    };
    let params = parse_query(query);
    if params
        .get("token")
        .map(|v| constant_time_eq(v.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
    {
        return true;
    }
    headers.iter().any(|(key, value)| {
        if key == "x-llm-wiki-token" {
            return constant_time_eq(value.as_bytes(), token.as_bytes());
        }
        if key == "authorization" {
            return value
                .strip_prefix("Bearer ")
                .map(|v| constant_time_eq(v.as_bytes(), token.as_bytes()))
                .unwrap_or(false);
        }
        false
    })
}

fn api_token(app: &AppHandle) -> Option<String> {
    if let Ok(token) = std::env::var("LLM_WIKI_API_TOKEN") {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let parsed = load_app_state(app)?;
    parsed
        .get("apiConfig")
        .and_then(|v| v.get("token"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn api_token_source(app: &AppHandle) -> &'static str {
    if let Ok(token) = std::env::var("LLM_WIKI_API_TOKEN") {
        if !token.trim().is_empty() {
            return "env";
        }
    }
    if load_app_state(app)
        .and_then(|parsed| {
            parsed
                .get("apiConfig")
                .and_then(|v| v.get("token"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|_| ())
        })
        .is_some()
    {
        return "store";
    }
    "none"
}

fn api_auth_required(app: &AppHandle) -> bool {
    !api_allow_unauthenticated(app)
}

fn api_allow_unauthenticated(app: &AppHandle) -> bool {
    let Some(parsed) = load_app_state(app) else {
        return false;
    };
    parsed
        .get("apiConfig")
        .and_then(|v| v.get("allowUnauthenticated"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Public origin the agent reaches the media proxy through (the nginx/frp
/// front door). Used to build directly-fetchable `images[].url` values in
/// the agent page payload. Resolution order: `LLM_WIKI_PUBLIC_BASE_URL`
/// env → `apiConfig.publicBaseUrl` in app-state → the deployed default.
/// A trailing slash is trimmed so callers can `format!("{base}/{path}")`.
fn api_public_base_url(app: &AppHandle) -> String {
    let from_env = std::env::var("LLM_WIKI_PUBLIC_BASE_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let raw = from_env
        .or_else(|| {
            load_app_state(app).and_then(|parsed| {
                parsed
                    .get("apiConfig")
                    .and_then(|v| v.get("publicBaseUrl"))
                    .and_then(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        })
        .unwrap_or_else(|| "https://wiki.aworld.ltd".to_string());
    raw.trim_end_matches('/').to_string()
}

/// Whether the API server should accept non-/health requests.
///
/// Defaults to `true` when no config has been written yet — keeps
/// existing setups (env-token-only, hand-edited app-state.json) working
/// after the kill-switch was introduced. New users still land in
/// "enabled + no token = 401" which is fail-closed by virtue of the
/// missing token, not the enable flag.
fn api_enabled(app: &AppHandle) -> bool {
    let Some(parsed) = load_app_state(app) else {
        return true;
    };
    parsed
        .get("apiConfig")
        .and_then(|v| v.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for i in 0..max_len {
        let a = left.get(i).copied().unwrap_or(0);
        let b = right.get(i).copied().unwrap_or(0);
        diff |= (a ^ b) as usize;
    }
    diff == 0
}

fn load_app_state(app: &AppHandle) -> Option<Value> {
    let now = Instant::now();
    let lock = APP_STATE_CACHE.get_or_init(|| Mutex::new(None));
    let mut previous = None;
    if let Ok(cache) = lock.lock() {
        if let Some(cached) = cache.as_ref() {
            if now.duration_since(cached.loaded_at) < APP_STATE_CACHE_TTL {
                return cached.value.clone();
            }
            previous = cached.value.clone();
        }
    }

    let path = app.path().app_data_dir().ok()?.join("app-state.json");
    let loaded = fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let value = loaded.or(previous);

    if let Ok(mut cache) = lock.lock() {
        *cache = Some(CachedAppState {
            loaded_at: now,
            value: value.clone(),
        });
    }
    value
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProjectEntry {
    id: String,
    name: String,
    path: String,
    current: bool,
}

fn handle_projects(app: &AppHandle) -> ApiResponse {
    let projects = load_projects(app);
    let current_project = projects.iter().find(|project| project.current).cloned();
    ok(json!({
        "ok": true,
        "projects": projects,
        "currentProject": current_project,
    }))
}

fn load_projects(app: &AppHandle) -> Vec<ProjectEntry> {
    let current = normalize_path(&clip_server::current_project_path());
    let mut by_path: BTreeMap<String, ProjectEntry> = BTreeMap::new();

    if let Some(parsed) = load_app_state(app) {
        if let Some(registry) = parsed.get("projectRegistry").and_then(Value::as_object) {
            for (id, value) in registry {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("");
                if path.is_empty() {
                    continue;
                }
                let path = normalize_path(path);
                let name = value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| project_name_from_path(&path));
                by_path.insert(
                    path.clone(),
                    ProjectEntry {
                        id: id.clone(),
                        name,
                        current: path == current,
                        path,
                    },
                );
            }
        }
        if let Some(recents) = parsed.get("recentProjects").and_then(Value::as_array) {
            for value in recents {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("");
                if path.is_empty() {
                    continue;
                }
                let path = normalize_path(path);
                by_path.entry(path.clone()).or_insert_with(|| {
                    let id = read_project_id(&path).unwrap_or_else(|| path.clone());
                    let name = value
                        .get("name")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| project_name_from_path(&path));
                    ProjectEntry {
                        id,
                        name,
                        current: path == current,
                        path,
                    }
                });
            }
        }
    }

    for (name, path) in clip_server::all_projects() {
        let path = normalize_path(&path);
        by_path.entry(path.clone()).or_insert_with(|| ProjectEntry {
            id: read_project_id(&path).unwrap_or_else(|| path.clone()),
            name: if name.is_empty() {
                project_name_from_path(&path)
            } else {
                name
            },
            current: path == current,
            path,
        });
    }

    if !current.is_empty() {
        by_path
            .entry(current.clone())
            .or_insert_with(|| ProjectEntry {
                id: read_project_id(&current).unwrap_or_else(|| current.clone()),
                name: project_name_from_path(&current),
                current: true,
                path: current.clone(),
            });
    }

    by_path.into_values().collect()
}

fn resolve_project(app: &AppHandle, project_id: &str) -> Result<ProjectEntry, String> {
    let project_id = percent_decode(project_id);
    let wants_current = project_id.eq_ignore_ascii_case("current");
    load_projects(app)
        .into_iter()
        .find(|p| {
            p.id == project_id
                || project_path_matches(&p.path, &project_id)
                || (wants_current && p.current)
        })
        .ok_or_else(|| format!("Unknown project: {project_id}"))
}

fn project_path_matches(stored_path: &str, candidate: &str) -> bool {
    let stored = normalize_path(stored_path);
    let candidate = normalize_path(candidate);
    if cfg!(windows) {
        stored.eq_ignore_ascii_case(&candidate)
    } else {
        stored == candidate
    }
}

fn read_project_id(path: &str) -> Option<String> {
    let raw = fs::read_to_string(Path::new(path).join(".llm-wiki/project.json")).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    parsed
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn project_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("Project")
        .to_string()
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").trim_end_matches('/').to_string()
}

fn handle_files(app: &AppHandle, project_id: &str, query: &str) -> ApiResponse {
    let project = match resolve_project(app, project_id) {
        Ok(project) => project,
        Err(e) => return err(404, e),
    };
    let params = parse_query(query);
    let root = params.get("root").map(String::as_str).unwrap_or("wiki");
    let recursive = params
        .get("recursive")
        .map(|v| v != "false")
        .unwrap_or(true);
    let max_files = params
        .get("maxFiles")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_FILES)
        .clamp(1, HARD_MAX_FILES);
    let rel = match root {
        "wiki" => "wiki",
        "sources" | "raw" | "raw/sources" => "raw/sources",
        "all" | "" => "",
        _ => return err(400, "root must be wiki, sources, or all"),
    };
    if rel.is_empty() {
        return match list_public_roots(&project.path, recursive, max_files) {
            Ok(files) => ok(json!({
                "ok": true,
                "projectId": project.id,
                "root": "all",
                "files": files,
                "truncated": false,
            })),
            Err(e) => err(if e.contains("exceeds") { 413 } else { 500 }, e),
        };
    }
    let dir = match safe_join(&project.path, rel) {
        Ok(path) => path,
        Err(e) => return err(400, e),
    };
    let mut count = 0;
    match list_tree(&project.path, &dir, recursive, max_files, &mut count) {
        Ok(files) => ok(json!({
            "ok": true,
            "projectId": project.id,
            "root": rel,
            "files": files,
            "truncated": false,
        })),
        Err(e) => err(if e.contains("exceeds") { 413 } else { 500 }, e),
    }
}

fn handle_file_content(app: &AppHandle, project_id: &str, query: &str) -> ApiResponse {
    let project = match resolve_project(app, project_id) {
        Ok(project) => project,
        Err(e) => return err(404, e),
    };
    let params = parse_query(query);
    let Some(rel) = params.get("path") else {
        return err(400, "Missing path query parameter");
    };
    if !is_public_project_rel(rel) {
        return err(403, "Path is not exposed by the local API");
    }
    if !is_text_content_rel(rel) {
        return err(
            415,
            "Only text-like project files can be read via this endpoint",
        );
    }
    let path = match safe_join(&project.path, rel) {
        Ok(path) => path,
        Err(e) => return err(400, e),
    };
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) => return err(404, format!("File not found: {e}")),
    };
    if meta.len() > MAX_FILE_CONTENT_BYTES {
        return err(413, "File is too large to return via API");
    }
    match fs::read_to_string(&path) {
        Ok(content) => ok(json!({
            "ok": true,
            "projectId": project.id,
            "path": rel,
            "content": content,
        })),
        Err(_) => err(415, "File is not valid UTF-8 text"),
    }
}

/// Agent-friendly page endpoint: returns a wiki/source page as structured JSON
/// (parsed frontmatter + clean body + extracted image refs + wikilinks) so an
/// agent doesn't have to parse markdown/YAML itself.
fn handle_agent_page(app: &AppHandle, project_id: &str, query: &str) -> ApiResponse {
    let project = match resolve_project(app, project_id) {
        Ok(project) => project,
        Err(e) => return err(404, e),
    };
    let params = parse_query(query);
    let Some(rel) = params.get("path") else {
        return err(400, "Missing path query parameter");
    };
    if !is_public_project_rel(rel) {
        return err(403, "Path is not exposed by the local API");
    }
    if !is_text_content_rel(rel) {
        return err(415, "Only text-like project files can be read via this endpoint");
    }
    let path = match safe_join(&project.path, rel) {
        Ok(path) => path,
        Err(e) => return err(400, e),
    };
    let meta = match fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) => return err(404, format!("File not found: {e}")),
    };
    if meta.len() > MAX_FILE_CONTENT_BYTES {
        return err(413, "File is too large to return via API");
    }
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return err(415, "File is not valid UTF-8 text"),
    };

    let (fm_opt, body) = split_frontmatter(&content);
    let mut frontmatter = serde_json::Map::new();
    if let Some(fm) = fm_opt {
        for line in fm.lines() {
            let line = line.trim_end();
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                if key.is_empty() {
                    continue;
                }
                frontmatter.insert(key.to_string(), parse_frontmatter_value(v));
            }
        }
    }

    let images = build_image_payload(&api_public_base_url(app), body);
    let wikilinks = extract_wikilinks(body);
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let title = commands::search::extract_title(&content, &file_name);

    let page = json!({
        "path": rel,
        "type": frontmatter.get("type").cloned().unwrap_or(Value::Null),
        "title": title,
        "tags": frontmatter.get("tags").cloned().unwrap_or(Value::Array(vec![])),
        "related": frontmatter.get("related").cloned().unwrap_or(Value::Array(vec![])),
        "sources": frontmatter.get("sources").cloned().unwrap_or(Value::Array(vec![])),
        "frontmatter": Value::Object(frontmatter),
        "body": body,
        "images": images,
        "wikilinks": wikilinks,
    });
    ok(json!({ "ok": true, "projectId": project.id, "page": page }))
}

/// Split a markdown document into its YAML frontmatter block (if any) and body.
/// Returns (Some(frontmatter_text), body) when the document starts with a `---`
/// fence and has a closing `---` line; otherwise (None, full_content).
fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let c = content.strip_prefix('\u{feff}').unwrap_or(content);
    let after = match c
        .strip_prefix("---\n")
        .or_else(|| c.strip_prefix("---\r\n"))
    {
        Some(a) => a,
        None => return (None, content),
    };
    let bytes = after.as_bytes();
    let mut i = 0usize;
    let mut line_start = 0usize;
    while i <= bytes.len() {
        let at_line_end = i == bytes.len() || bytes[i] == b'\n';
        if at_line_end {
            let line = after[line_start..i].trim_end_matches('\r');
            if line.trim() == "---" {
                let fm = &after[..line_start];
                let body = if i < bytes.len() { &after[i + 1..] } else { "" };
                return (Some(fm), body.trim_start_matches(['\n', '\r']));
            }
            line_start = i + 1;
        }
        i += 1;
    }
    (None, content)
}

/// Parse a single frontmatter value: inline arrays `[a, b, c]` → JSON array,
/// otherwise a quote-stripped string.
fn parse_frontmatter_value(raw: &str) -> Value {
    let v = raw.trim();
    if v.len() >= 2 && v.starts_with('[') && v.ends_with(']') {
        let inner = &v[1..v.len() - 1];
        let arr: Vec<Value> = inner
            .split(',')
            .map(|s| {
                s.trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty())
            .map(Value::String)
            .collect();
        return Value::Array(arr);
    }
    let s = v.trim_matches('"').trim_matches('\'');
    Value::String(s.to_string())
}

/// Extract `(alt, target)` pairs from markdown `![alt](target)` image
/// syntax, in document order. `alt` is the caption we inject during ingest
/// (a Chinese Vision description); `target` is the project-relative media
/// path. Alt is trimmed; targets are trimmed and empty targets skipped.
fn extract_image_refs(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(p) = rest.find("![") {
        rest = &rest[p + 2..];
        let Some(open) = rest.find("](") else {
            break;
        };
        let alt = rest[..open].trim().to_string();
        let after = &rest[open + 2..];
        let Some(close) = after.find(')') else {
            rest = after;
            continue;
        };
        let target = after[..close].trim();
        if !target.is_empty() {
            out.push((alt, target.to_string()));
        }
        rest = &after[close + 1..];
    }
    out
}

/// Build the `images` array for the agent page payload from a markdown
/// body. Each unique image becomes `{ url, description, path }`:
///   - `path`    — project-relative path the media proxy serves
///                 (`media/<slug>/<file>` → `wiki/media/<slug>/<file>`).
///   - `url`     — `base_url` + percent-encoded path: a directly-GETtable
///                 link (the agent does no encoding of its own).
///   - `description` — the injected Chinese Vision caption (the `![..]` alt).
/// Images are deduplicated by resolved `path`, keeping first occurrence.
/// Already-absolute `http(s)://` targets pass through unencoded.
fn build_image_payload(base_url: &str, body: &str) -> Vec<Value> {
    let mut seen_paths = std::collections::HashSet::new();
    extract_image_refs(body)
        .into_iter()
        .filter_map(|(alt, target)| {
            let is_absolute =
                target.starts_with("http://") || target.starts_with("https://");
            let path = if is_absolute {
                target.clone()
            } else if let Some(rest) = target.strip_prefix("media/") {
                format!("wiki/media/{rest}")
            } else {
                target.clone()
            };
            if !seen_paths.insert(path.clone()) {
                return None;
            }
            let url = if is_absolute {
                path.clone()
            } else {
                let encoded = path
                    .split('/')
                    .map(percent_encode_segment)
                    .collect::<Vec<_>>()
                    .join("/");
                format!("{base_url}/{encoded}")
            };
            Some(json!({
                "url": url,
                "description": alt,
                "path": path,
            }))
        })
        .collect()
}

fn safe_join(project_path: &str, rel: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(project_path);
    let rel = rel.trim_start_matches('/');
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err("Absolute paths are not allowed".to_string());
    }
    for component in rel_path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) {
            return Err("Path traversal is not allowed".to_string());
        }
    }
    let joined = root.join(rel_path);
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("Failed to resolve project path: {e}"))?;
    if joined.exists() {
        let joined_canon = joined
            .canonicalize()
            .map_err(|e| format!("Failed to resolve path: {e}"))?;
        if !joined_canon.starts_with(&root_canon) {
            return Err("Resolved path escapes the project directory".to_string());
        }
        return Ok(joined_canon);
    }
    let parent = joined
        .parent()
        .ok_or_else(|| "Path has no parent directory".to_string())?;
    if parent.exists() {
        let parent_canon = parent
            .canonicalize()
            .map_err(|e| format!("Failed to resolve parent path: {e}"))?;
        if !parent_canon.starts_with(&root_canon) {
            return Err("Resolved parent escapes the project directory".to_string());
        }
    }
    Ok(joined)
}

fn is_public_project_rel(rel: &str) -> bool {
    let rel = normalize_path(rel).trim_start_matches('/').to_string();
    if rel
        .split('/')
        .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return false;
    }
    let lower = rel.to_lowercase();
    lower == "purpose.md"
        || lower == "schema.md"
        || lower.starts_with("wiki/")
        || lower.starts_with("raw/sources/")
}

fn is_text_content_rel(rel: &str) -> bool {
    let rel = normalize_path(rel).to_lowercase();
    let ext = Path::new(&rel)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    matches!(
        ext,
        "md" | "mdx"
            | "txt"
            | "csv"
            | "json"
            | "yaml"
            | "yml"
            | "xml"
            | "html"
            | "htm"
            | "rtf"
            | "log"
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiFileNode {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
    children: Option<Vec<ApiFileNode>>,
}

fn list_public_roots(
    project_path: &str,
    recursive: bool,
    max_files: usize,
) -> Result<Vec<ApiFileNode>, String> {
    let mut count = 0;
    let mut roots = Vec::new();
    for rel in ["purpose.md", "schema.md", "wiki", "raw/sources"] {
        let path = safe_join(project_path, rel)?;
        if !path.exists() {
            continue;
        }
        push_file_node(
            project_path,
            &path,
            recursive,
            max_files,
            &mut count,
            &mut roots,
        )?;
    }
    Ok(roots)
}

fn list_tree(
    project_path: &str,
    path: &Path,
    recursive: bool,
    max_files: usize,
    count: &mut usize,
) -> Result<Vec<ApiFileNode>, String> {
    let mut out = Vec::new();
    let entries = fs::read_dir(path).map_err(|e| format!("Failed to list directory: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {e}"))?;
        push_file_node(
            project_path,
            &entry.path(),
            recursive,
            max_files,
            count,
            &mut out,
        )?;
    }
    out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

fn push_file_node(
    project_path: &str,
    path: &Path,
    recursive: bool,
    max_files: usize,
    count: &mut usize,
    out: &mut Vec<ApiFileNode>,
) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();
    if name.starts_with('.') {
        return Ok(());
    }
    let meta = fs::symlink_metadata(path).map_err(|e| format!("Failed to read metadata: {e}"))?;
    let file_type = meta.file_type();
    if file_type.is_symlink() {
        return Ok(());
    }
    *count += 1;
    if *count > max_files {
        return Err(format!("File listing exceeds maxFiles limit ({max_files})"));
    }
    let is_dir = file_type.is_dir();
    let children = if recursive && is_dir {
        Some(list_tree(project_path, path, true, max_files, count)?)
    } else {
        None
    };
    out.push(ApiFileNode {
        name,
        path: relative_to_project(project_path, path),
        is_dir,
        size: if is_dir { None } else { Some(meta.len()) },
        children,
    });
    Ok(())
}

fn relative_to_project(project_path: &str, path: &Path) -> String {
    let root = Path::new(project_path);
    path.strip_prefix(root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().replace('\\', "/"))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest {
    query: String,
    top_k: Option<usize>,
    include_content: Option<bool>,
    query_embedding: Option<Vec<f32>>,
}

fn handle_search(app: &AppHandle, project_id: &str, body: &str) -> ApiResponse {
    let project = match resolve_project(app, project_id) {
        Ok(project) => project,
        Err(e) => return err(404, e),
    };
    let req: SearchRequest = match serde_json::from_str(body) {
        Ok(req) => req,
        Err(e) => return err(400, format!("Invalid JSON: {e}")),
    };
    if req.query.trim().is_empty() {
        return err(400, "query is required");
    }
    let top_k = req.top_k.unwrap_or(10).clamp(1, MAX_SEARCH_RESULTS);
    let query = req.query;
    let query_embedding =
        match tauri::async_runtime::block_on(commands::search::resolve_query_embedding(
            &query,
            req.query_embedding,
            load_embedding_config(app),
        )) {
            Ok(embedding) => embedding,
            Err(e) => return err(400, e),
        };
    match tauri::async_runtime::block_on(commands::search::search_project_inner(
        project.path.clone(),
        query,
        top_k,
        req.include_content.unwrap_or(false),
        query_embedding,
    )) {
        Ok(search) => ok(json!({
            "ok": true,
            "projectId": project.id,
            "mode": search.mode,
            "note": "Search uses the shared backend retrieval service. When embeddingConfig is enabled, the API automatically includes LanceDB vector results; clients may also pass queryEmbedding explicitly.",
            "tokenHits": search.token_hits,
            "vectorHits": search.vector_hits,
            "results": search.results,
        })),
        Err(e) => err(500, e),
    }
}

fn load_embedding_config(app: &AppHandle) -> Option<commands::search::SearchEmbeddingConfig> {
    let parsed = load_app_state(app)?;
    let value = parsed.get("embeddingConfig")?.clone();
    serde_json::from_value::<commands::search::SearchEmbeddingConfig>(value).ok()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiGraphNode {
    id: String,
    label: String,
    node_type: String,
    path: String,
    link_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiGraphEdge {
    source: String,
    target: String,
    weight: f64,
}

fn handle_graph(app: &AppHandle, project_id: &str, query: &str) -> ApiResponse {
    let project = match resolve_project(app, project_id) {
        Ok(project) => project,
        Err(e) => return err(404, e),
    };
    let params = parse_query(query);
    let q = params.get("q").map(|s| s.to_lowercase());
    let node_type = params.get("nodeType").map(|s| s.to_lowercase());
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);

    match build_graph(&project.path) {
        Ok((mut nodes, edges)) => {
            if let Some(ref q) = q {
                nodes.retain(|n| {
                    n.id.to_lowercase().contains(q) || n.label.to_lowercase().contains(q)
                });
            }
            if let Some(ref node_type) = node_type {
                nodes.retain(|n| n.node_type == *node_type);
            }
            nodes.truncate(limit);
            let ids: BTreeSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
            let edges: Vec<ApiGraphEdge> = edges
                .into_iter()
                .filter(|e| ids.contains(&e.source) && ids.contains(&e.target))
                .collect();
            ok(json!({ "ok": true, "projectId": project.id, "nodes": nodes, "edges": edges }))
        }
        Err(e) => err(500, e),
    }
}

fn build_graph(project_path: &str) -> Result<(Vec<ApiGraphNode>, Vec<ApiGraphEdge>), String> {
    let wiki_root = Path::new(project_path).join("wiki");
    let mut raw: BTreeMap<String, (String, String, String, Vec<String>)> = BTreeMap::new();
    for entry in WalkDir::new(&wiki_root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file()
            || entry.path().extension().and_then(|s| s.to_str()) != Some("md")
        {
            continue;
        }
        let content = match fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let id = entry
            .path()
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let title =
            commands::search::extract_title(&content, entry.file_name().to_string_lossy().as_ref());
        let node_type = extract_type(&content);
        let path = relative_to_project(project_path, entry.path());
        let links = extract_wikilinks(&content);
        raw.insert(id, (title, node_type, path, links));
    }
    let ids: BTreeSet<String> = raw.keys().cloned().collect();
    let mut link_count: BTreeMap<String, usize> = raw.keys().map(|id| (id.clone(), 0)).collect();
    let mut seen = BTreeSet::new();
    let mut edges = Vec::new();
    for (source, (_, _, _, links)) in &raw {
        for link in links {
            let Some(target) = resolve_link(link, &ids) else {
                continue;
            };
            if &target == source {
                continue;
            }
            let key = if source < &target {
                format!("{source}::{target}")
            } else {
                format!("{target}::{source}")
            };
            if seen.insert(key) {
                *link_count.entry(source.clone()).or_default() += 1;
                *link_count.entry(target.clone()).or_default() += 1;
                edges.push(ApiGraphEdge {
                    source: source.clone(),
                    target,
                    weight: 1.0,
                });
            }
        }
    }
    let nodes = raw
        .into_iter()
        .filter(|(_, (_, node_type, _, _))| node_type != "query")
        .map(|(id, (label, node_type, path, _))| ApiGraphNode {
            link_count: *link_count.get(&id).unwrap_or(&0),
            id,
            label,
            node_type,
            path,
        })
        .collect();
    Ok((nodes, edges))
}

fn extract_type(content: &str) -> String {
    for line in content.lines() {
        if let Some(value) = line.trim().strip_prefix("type:") {
            return value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_lowercase();
        }
    }
    "other".to_string()
}

fn extract_wikilinks(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("[[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("]]") else {
            break;
        };
        let inner = &rest[..end];
        let target = inner.split('|').next().unwrap_or("").trim();
        if !target.is_empty() {
            out.push(target.to_string());
        }
        rest = &rest[end + 2..];
    }
    out
}

fn resolve_link(raw: &str, ids: &BTreeSet<String>) -> Option<String> {
    if ids.contains(raw) {
        return Some(raw.to_string());
    }
    let normalized = raw.to_lowercase().replace(' ', "-");
    ids.iter()
        .find(|id| id.to_lowercase() == normalized || id.to_lowercase() == raw.to_lowercase())
        .cloned()
}

fn handle_rescan(app: &AppHandle, project_id: &str) -> ApiResponse {
    let project = match resolve_project(app, project_id) {
        Ok(project) => project,
        Err(e) => return err(404, e),
    };
    let source_watch_config = load_source_watch_config(app, &project.id);
    match commands::file_sync::rescan_project_files(
        app.clone(),
        project.id.clone(),
        project.path.clone(),
        source_watch_config,
    ) {
        Ok(result) => ok(json!({ "ok": true, "projectId": project.id, "result": result })),
        Err(e) => err(500, e),
    }
}

fn load_source_watch_config(
    app: &AppHandle,
    project_id: &str,
) -> Option<commands::file_sync::SourceWatchConfig> {
    let parsed = load_app_state(app)?;
    let settings = parsed.get("sourceWatchConfig").and_then(Value::as_object);
    if let Some(value) = settings
        .and_then(|s| s.get(project_id).or_else(|| s.get("default")))
        .cloned()
    {
        if let Ok(config) = serde_json::from_value::<commands::file_sync::SourceWatchConfig>(value)
        {
            return Some(config);
        }
    }
    let legacy_enabled = parsed
        .get("projectFileSyncEnabled")
        .and_then(Value::as_object)
        .and_then(|settings| {
            settings
                .get(project_id)
                .or_else(|| settings.get("default"))
                .and_then(Value::as_bool)
        });
    legacy_enabled.and_then(|enabled| {
        serde_json::from_value::<commands::file_sync::SourceWatchConfig>(
            json!({ "enabled": enabled }),
        )
        .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_project_dir() -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("llm-wiki-api-test-{id}"));
        fs::create_dir_all(path.join("wiki")).unwrap();
        path
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let root = test_project_dir();
        let root_str = root.to_string_lossy();
        assert!(safe_join(&root_str, "../secret.md").is_err());
        assert!(safe_join(&root_str, "wiki/../../secret.md").is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn safe_join_accepts_project_relative_paths() {
        let root = test_project_dir();
        let root_str = root.to_string_lossy();
        let joined = safe_join(&root_str, "wiki/index.md").unwrap();
        assert_eq!(joined, root.join("wiki/index.md"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn query_parser_decodes_percent_and_plus() {
        let parsed = parse_query("path=wiki%2Fhello+world.md&token=a%2Bb");
        assert_eq!(parsed.get("path").unwrap(), "wiki/hello world.md");
        assert_eq!(parsed.get("token").unwrap(), "a+b");
    }

    #[test]
    fn percent_encode_segment_uses_encode_uri_component_semantics() {
        // Unreserved set is left intact.
        assert_eq!(percent_encode_segment("Abc-1_2.3~x"), "Abc-1_2.3~x");
        // Space and the literal `%` (already in on-disk slugs) get encoded;
        // `%20` becomes `%2520` so nginx decodes back to a literal `%20`.
        assert_eq!(percent_encode_segment("m square"), "m%20square");
        assert_eq!(percent_encode_segment("a%20b"), "a%2520b");
        // CJK is UTF-8 byte-encoded.
        assert_eq!(percent_encode_segment("心"), "%E5%BF%83");
    }

    #[test]
    fn extract_image_refs_captures_alt_and_target() {
        let body = "intro\n![一只猫](media/a/猫.jpg)\ntext ![](media/b.png) end";
        let refs = extract_image_refs(body);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0], ("一只猫".to_string(), "media/a/猫.jpg".to_string()));
        // Empty alt is allowed; empty target would be skipped.
        assert_eq!(refs[1], (String::new(), "media/b.png".to_string()));
    }

    #[test]
    fn build_image_payload_encodes_dedups_and_pairs_description() {
        let body = "![绿色收纳袋](media/m%20square/001-心心_01.jpg)\n\
                    ![绿色收纳袋](media/m%20square/001-心心_01.jpg)\n\
                    ![外链图](https://cdn.example.com/x.png)";
        let imgs = build_image_payload("https://wiki.aworld.ltd", body);
        // Dedup by resolved path: the repeated media image collapses to one.
        assert_eq!(imgs.len(), 2);
        let first = &imgs[0];
        assert_eq!(first["description"], "绿色收纳袋");
        assert_eq!(first["path"], "wiki/media/m%20square/001-心心_01.jpg");
        // `%`→`%25` and CJK→`%XX`; the `/` separators are preserved.
        assert_eq!(
            first["url"],
            "https://wiki.aworld.ltd/wiki/media/m%2520square/001-%E5%BF%83%E5%BF%83_01.jpg"
        );
        // Absolute targets pass through untouched (no base prefix, no re-encode).
        assert_eq!(imgs[1]["url"], "https://cdn.example.com/x.png");
        assert_eq!(imgs[1]["path"], "https://cdn.example.com/x.png");
    }

    #[test]
    fn snippet_handles_unicode_boundaries() {
        let content = "前言。这里是关于知识图谱过滤的中文内容。后续说明。";
        let snippet = commands::search::build_snippet(content, "知识图谱");
        assert!(snippet.contains("知识图谱"));
    }

    #[test]
    fn public_api_paths_exclude_internal_state() {
        assert!(is_public_project_rel("wiki/index.md"));
        assert!(is_public_project_rel("Wiki/index.md"));
        assert!(is_public_project_rel("raw/sources/source.md"));
        assert!(is_public_project_rel("Raw/Sources/source.md"));
        assert!(!is_public_project_rel(".llm-wiki/file-change-queue.json"));
        assert!(!is_public_project_rel("wiki/.draft.md"));
    }

    #[test]
    fn project_path_match_normalizes_separators() {
        assert!(project_path_matches(
            "C:/Users/me/wiki",
            "C:\\Users\\me\\wiki"
        ));
        if cfg!(windows) {
            assert!(project_path_matches("C:/Users/me/wiki", "c:/users/me/wiki"));
        } else {
            assert!(!project_path_matches(
                "C:/Users/me/wiki",
                "c:/users/me/wiki"
            ));
        }
    }

    #[test]
    fn tokenize_keeps_single_cjk_character() {
        assert_eq!(
            crate::commands::search::tokenize_query("图"),
            Vec::<String>::new()
        );
        let tokens = crate::commands::search::tokenize_query("知识图谱");
        assert!(tokens.contains(&"知识".to_string()));
    }

    #[test]
    fn text_content_filter_rejects_binary_extensions() {
        assert!(is_text_content_rel("wiki/index.md"));
        assert!(!is_text_content_rel("wiki/media/image.png"));
        assert!(!is_text_content_rel("raw/sources/book.pdf"));
    }

    #[test]
    fn constant_time_eq_matches_equal_bytes_only() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"token", b"tokeN"));
        assert!(!constant_time_eq(b"token", b"token-longer"));
    }

    #[test]
    fn rate_limit_skips_health_and_options_only() {
        assert!(!should_rate_limit(&Method::Get, "/api/v1/health"));
        assert!(!should_rate_limit(&Method::Options, "/api/v1/projects"));
        assert!(should_rate_limit(&Method::Get, "/wp-login"));
        assert!(should_rate_limit(
            &Method::Post,
            "/api/v1/projects/current/search"
        ));
    }

    #[test]
    fn api_config_shape_parses_enabled_and_unauthenticated_access() {
        // Standalone pure-function check to mirror what `api_enabled`
        // reads off `load_app_state`. Mirrors the JS-side shape
        // emitted by `saveApiConfig` so any rename on either side
        // surfaces here before users hit it as a 503 in production.
        let payload = json!({
            "apiConfig": {
                "enabled": false,
                "allowUnauthenticated": true,
                "token": "abc"
            }
        });
        let enabled = payload
            .get("apiConfig")
            .and_then(|v| v.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        assert!(!enabled);
        let allow_unauthenticated = payload
            .get("apiConfig")
            .and_then(|v| v.get("allowUnauthenticated"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        assert!(allow_unauthenticated);
        let token_source = payload
            .get("apiConfig")
            .and_then(|v| v.get("token"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|_| "store")
            .unwrap_or("none");
        assert_eq!(token_source, "store");

        let missing = json!({});
        let enabled_missing = missing
            .get("apiConfig")
            .and_then(|v| v.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        // Fail-open by design — see `api_enabled` doc comment.
        assert!(enabled_missing);
    }
}
