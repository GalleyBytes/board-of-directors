use crate::agents;
use crate::bugfix::SeverityLevel;
use crate::bugfix_log;
use crate::bugfix_session::{BugfixSession, SessionSnapshot};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::{Duration, timeout};

const INDEX_HTML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/index.html"));
const APP_TSX: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/ui/app.tsx"));
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ServerHandle {
    pub port: u16,
    pub url: String,
    shutdown_tx: watch::Sender<bool>,
    quit_rx: watch::Receiver<bool>,
}

impl ServerHandle {
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub async fn wait_for_quit(&mut self) {
        while self.quit_rx.changed().await.is_ok() {
            if *self.quit_rx.borrow() {
                return;
            }
        }
    }
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
    origin: Option<String>,
    csrf_token: Option<String>,
    host: Option<String>,
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    extra_headers: Vec<(&'static str, String)>,
}

#[derive(Debug, Clone, Serialize)]
struct DocEntry {
    source: String,
    path: String,
    title: String,
    category: String,
    kind: DocKind,
    round_id: Option<String>,
    round_label: Option<String>,
    is_latest: bool,
    pinned: bool,
}

#[derive(Debug, Clone)]
struct DocEntryInternal {
    entry: DocEntry,
    absolute_path: PathBuf,
    group_sort_key: Option<String>,
    item_priority: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DocKind {
    BugfixLog,
    Consolidated,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactKind {
    BugfixLog,
    Consolidated,
    Review,
}

#[derive(Debug, Deserialize)]
struct NotesPayload {
    content: String,
}

#[derive(Debug, Deserialize)]
struct SeverityPayload {
    severity: String,
}

#[derive(Debug, Serialize)]
struct StartResponse {
    started: bool,
    status: SessionSnapshot,
}

pub async fn start(session: BugfixSession) -> Result<ServerHandle, String> {
    let listener = bind_random_port().await?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to inspect listener address: {}", e))?
        .port();
    let url = format!("http://127.0.0.1:{}/", port);
    let csrf_token = generate_csrf_token();
    let session_for_server = session.clone();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (quit_tx, quit_rx) = watch::channel(false);
    let token_for_server = csrf_token.clone();
    tokio::spawn(async move {
        if let Err(e) = run_server(
            listener,
            session_for_server,
            token_for_server,
            shutdown_rx,
            quit_tx,
        )
        .await
        {
            eprintln!("Warning: localhost server stopped unexpectedly: {}", e);
        }
    });

    Ok(ServerHandle {
        port,
        url,
        shutdown_tx,
        quit_rx,
    })
}

pub fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to launch browser with 'open': {}", e))?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to launch browser with 'xdg-open': {}", e))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map_err(|e| format!("Failed to launch browser with 'start': {}", e))?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("Automatic browser launch is not supported on this platform.".to_string())
}

async fn bind_random_port() -> Result<TcpListener, String> {
    let start_offset = random_offset();
    for offset in 0..10_000u16 {
        let candidate = 20_000 + ((start_offset + offset) % 10_000);
        match TcpListener::bind(("127.0.0.1", candidate)).await {
            Ok(listener) => return Ok(listener),
            Err(_) => continue,
        }
    }
    Err("Failed to bind a localhost port in 20000..=29999".to_string())
}

async fn run_server(
    listener: TcpListener,
    session: BugfixSession,
    csrf_token: String,
    mut shutdown_rx: watch::Receiver<bool>,
    quit_tx: watch::Sender<bool>,
) -> Result<(), String> {
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result
                    .map_err(|e| format!("Failed to accept browser connection: {}", e))?;
                let session = session.clone();
                let token = csrf_token.clone();
                let quit = quit_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, session, token, quit).await {
                        eprintln!("Warning: failed to serve browser request: {}", e);
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn handle_connection(
    mut stream: TcpStream,
    session: BugfixSession,
    csrf_token: String,
    quit_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let request = match read_request(&mut stream).await? {
        Some(request) => request,
        None => return Ok(()),
    };
    let response = route(request, session, &csrf_token, &quit_tx).await;
    write_response(&mut stream, response).await
}

fn validate_host(request: &HttpRequest) -> Option<HttpResponse> {
    match &request.host {
        Some(host) => {
            let host_name = host.split(':').next().unwrap_or("");
            if host_name != "127.0.0.1" && host_name != "localhost" {
                return Some(HttpResponse {
                    status: 403,
                    reason: "Forbidden",
                    content_type: "text/plain; charset=utf-8",
                    body: b"Host not allowed".to_vec(),
                    extra_headers: Vec::new(),
                });
            }
        }
        None => {
            return Some(HttpResponse {
                status: 400,
                reason: "Bad Request",
                content_type: "text/plain; charset=utf-8",
                body: b"Missing Host header".to_vec(),
                extra_headers: Vec::new(),
            });
        }
    }
    None
}

fn validate_request(request: &HttpRequest, csrf_token: &str) -> Option<HttpResponse> {
    // Validate Origin header for mutation requests
    if let Some(origin) = &request.origin {
        if !origin.starts_with("http://127.0.0.1:") && !origin.starts_with("http://localhost:") {
            return Some(HttpResponse {
                status: 403,
                reason: "Forbidden",
                content_type: "text/plain; charset=utf-8",
                body: b"Origin not allowed".to_vec(),
                extra_headers: Vec::new(),
            });
        }
    }
    // Validate CSRF token
    if request.csrf_token.as_deref() != Some(csrf_token) {
        return Some(HttpResponse {
            status: 403,
            reason: "Forbidden",
            content_type: "text/plain; charset=utf-8",
            body: b"Invalid or missing X-CSRF-Token header".to_vec(),
            extra_headers: Vec::new(),
        });
    }
    None
}

async fn route(
    request: HttpRequest,
    session: BugfixSession,
    csrf_token: &str,
    quit_tx: &watch::Sender<bool>,
) -> HttpResponse {
    // Validate Host header on all requests to prevent DNS rebinding attacks.
    if let Some(resp) = validate_host(&request) {
        return resp;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let page = INDEX_HTML.replace("{{CSRF_TOKEN}}", csrf_token);
            html_response(&page)
        }
        ("GET", "/assets/app.tsx") => text_response("text/plain; charset=utf-8", APP_TSX),
        ("GET", "/favicon.ico") => HttpResponse {
            status: 204,
            reason: "No Content",
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            extra_headers: vec![("Cache-Control", "no-store".to_string())],
        },
        ("GET", "/api/status") => match serde_json::to_vec(&session.snapshot().await) {
            Ok(body) => json_response(body),
            Err(e) => internal_error(format!("Failed to serialize session status: {}", e)),
        },
        ("GET", "/api/notes") => {
            match bugfix_log::read_user_notes_with_migration(
                &session.state_dir(),
                &session.sanitized_branch(),
            ) {
                Ok(content) => json_response(
                    json!({
                        "content": content,
                    })
                    .to_string()
                    .into_bytes(),
                ),
                Err(e) => internal_error(e),
            }
        }
        ("PUT", "/api/notes") => {
            if let Some(resp) = validate_request(&request, csrf_token) {
                return resp;
            }
            match parse_json::<NotesPayload>(&request.body) {
                Ok(payload) => match bugfix_log::write_user_notes(
                    &session.state_dir(),
                    &session.sanitized_branch(),
                    &payload.content,
                ) {
                    Ok(_) => json_response(
                        json!({
                            "ok": true,
                            "content": payload.content,
                        })
                        .to_string()
                        .into_bytes(),
                    ),
                    Err(e) => internal_error(e),
                },
                Err(e) => bad_request(e),
            }
        }
        ("PUT", "/api/severity") => {
            if let Some(resp) = validate_request(&request, csrf_token) {
                return resp;
            }
            match parse_json::<SeverityPayload>(&request.body) {
                Ok(payload) => match SeverityLevel::from_str(&payload.severity) {
                    Ok(severity) => {
                        session.set_next_severity(severity).await;
                        match serde_json::to_vec(&session.snapshot().await) {
                            Ok(body) => json_response(body),
                            Err(e) => {
                                internal_error(format!("Failed to serialize session status: {}", e))
                            }
                        }
                    }
                    Err(e) => bad_request(e),
                },
                Err(e) => bad_request(e),
            }
        }
        ("POST", "/api/start") => {
            if let Some(resp) = validate_request(&request, csrf_token) {
                return resp;
            }
            let started = session.request_start().await;
            let response = StartResponse {
                started,
                status: session.snapshot().await,
            };
            match serde_json::to_vec(&response) {
                Ok(body) => json_response(body),
                Err(e) => internal_error(format!("Failed to serialize session status: {}", e)),
            }
        }
        ("POST", "/api/cancel") => {
            if let Some(resp) = validate_request(&request, csrf_token) {
                return resp;
            }
            session.request_cancel().await;
            match serde_json::to_vec(&session.snapshot().await) {
                Ok(body) => json_response(body),
                Err(e) => internal_error(format!("Failed to serialize session status: {}", e)),
            }
        }
        ("POST", "/api/quit") => {
            if let Some(resp) = validate_request(&request, csrf_token) {
                return resp;
            }
            let _ = quit_tx.send(true);
            json_response(json!({ "ok": true }).to_string().into_bytes())
        }
        ("GET", "/api/docs") => match build_doc_index(&session, &session.snapshot().await) {
            Ok(index) => {
                let entries: Vec<DocEntry> = index.into_iter().map(|entry| entry.entry).collect();
                json_response(
                    json!({
                        "docs": entries,
                    })
                    .to_string()
                    .into_bytes(),
                )
            }
            Err(e) => internal_error(e),
        },
        ("GET", "/api/doc") => match read_doc_from_query(&request.query, &session).await {
            Ok(body) => json_response(body),
            Err(e) => bad_request(e),
        },
        ("GET", _) if !request.path.starts_with("/api/") => {
            let page = INDEX_HTML.replace("{{CSRF_TOKEN}}", csrf_token);
            html_response(&page)
        }
        _ => not_found(),
    }
}

async fn read_doc_from_query(query: &str, session: &BugfixSession) -> Result<Vec<u8>, String> {
    let snapshot = session.snapshot().await;
    let params = parse_query(query);
    let source = params
        .get("source")
        .ok_or_else(|| "Missing doc source".to_string())?;
    let path = params
        .get("path")
        .ok_or_else(|| "Missing doc path".to_string())?;

    let index = build_doc_index(session, &snapshot)?;
    let entry = index
        .into_iter()
        .find(|entry| entry.entry.source == *source && entry.entry.path == *path)
        .ok_or_else(|| "Unknown markdown document".to_string())?;
    let content = std::fs::read_to_string(&entry.absolute_path)
        .map_err(|e| format!("Failed to read {}: {}", entry.absolute_path.display(), e))?;

    Ok(json!({
        "source": entry.entry.source,
        "path": entry.entry.path,
        "title": entry.entry.title,
        "category": entry.entry.category,
        "content": content,
    })
    .to_string()
    .into_bytes())
}

fn build_doc_index(
    session: &BugfixSession,
    snapshot: &SessionSnapshot,
) -> Result<Vec<DocEntryInternal>, String> {
    let state_dir = session.state_dir();
    let sanitized_branch = session.sanitized_branch();
    let review_codenames = session.review_codenames();
    let mut docs = Vec::new();
    let mut seen_paths = HashSet::new();

    push_state_doc(
        &mut docs,
        &mut seen_paths,
        &state_dir,
        &snapshot.log_filename,
        ArtifactKind::BugfixLog,
        snapshot,
    )?;
    for file_name in agents::list_consolidated_files_for_branch(&state_dir, &sanitized_branch) {
        push_state_doc(
            &mut docs,
            &mut seen_paths,
            &state_dir,
            &file_name,
            ArtifactKind::Consolidated,
            snapshot,
        )?;
    }
    for file_name in
        agents::list_review_files_for_branch(&state_dir, &sanitized_branch, &review_codenames)
    {
        push_state_doc(
            &mut docs,
            &mut seen_paths,
            &state_dir,
            &file_name,
            ArtifactKind::Review,
            snapshot,
        )?;
    }

    docs.sort_by(|a, b| {
        b.entry
            .pinned
            .cmp(&a.entry.pinned)
            .then_with(|| compare_group_sort_key(&a.group_sort_key, &b.group_sort_key))
            .then_with(|| a.item_priority.cmp(&b.item_priority))
            .then_with(|| a.entry.path.cmp(&b.entry.path))
            .then_with(|| a.entry.title.cmp(&b.entry.title))
    });
    Ok(docs)
}

fn compare_group_sort_key(left: &Option<String>, right: &Option<String>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => right.cmp(left),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn push_state_doc(
    out: &mut Vec<DocEntryInternal>,
    seen_paths: &mut HashSet<String>,
    state_dir: &Path,
    file_name: &str,
    kind: ArtifactKind,
    snapshot: &SessionSnapshot,
) -> Result<(), String> {
    if !seen_paths.insert(file_name.to_string()) {
        return Ok(());
    }

    let absolute_path = state_dir.join(file_name);
    match std::fs::metadata(&absolute_path) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            return Err(format!("Failed to stat {}: {}", absolute_path.display(), e));
        }
    }

    let (entry, group_sort_key, item_priority) = doc_for_state_artifact(kind, file_name, snapshot);
    out.push(DocEntryInternal {
        entry,
        absolute_path,
        group_sort_key,
        item_priority,
    });
    Ok(())
}

fn doc_for_state_artifact(
    kind: ArtifactKind,
    file_name: &str,
    snapshot: &SessionSnapshot,
) -> (DocEntry, Option<String>, u8) {
    let round_id = match kind {
        ArtifactKind::BugfixLog => None,
        ArtifactKind::Consolidated | ArtifactKind::Review => {
            agents::extract_timestamp(file_name).map(str::to_string)
        }
    };
    let group_sort_key = round_id
        .as_deref()
        .map(agents::round_id_sort_key)
        .map(str::to_string);
    let round_label = round_id.as_deref().and_then(format_round_label);
    let is_latest = match kind {
        ArtifactKind::BugfixLog => false,
        ArtifactKind::Consolidated => snapshot.latest_report_filename.as_deref() == Some(file_name),
        ArtifactKind::Review => snapshot.latest_round_id.as_deref() == round_id.as_deref(),
    };
    let (title, category, item_priority, doc_kind, pinned) = match kind {
        ArtifactKind::BugfixLog => (
            "Bugfix".to_string(),
            "Bugfix log".to_string(),
            0,
            DocKind::BugfixLog,
            true,
        ),
        ArtifactKind::Consolidated => (
            display_state_doc_title(file_name, round_id.as_deref()),
            "Consolidated".to_string(),
            0,
            DocKind::Consolidated,
            false,
        ),
        ArtifactKind::Review => (
            display_state_doc_title(file_name, round_id.as_deref()),
            "Review".to_string(),
            1,
            DocKind::Review,
            false,
        ),
    };

    (
        DocEntry {
            source: "state".to_string(),
            path: file_name.to_string(),
            title,
            category,
            kind: doc_kind,
            round_id,
            round_label,
            is_latest,
            pinned,
        },
        group_sort_key,
        item_priority,
    )
}

fn display_state_doc_title(file_name: &str, round_id: Option<&str>) -> String {
    let Some(round_id) = round_id else {
        return file_name.to_string();
    };
    let Some(stem) = file_name.strip_suffix(".md") else {
        return file_name.to_string();
    };
    let Some(rest) = stem
        .strip_prefix(round_id)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return file_name.to_string();
    };
    format!("{}.md", rest)
}

fn format_round_label(round_id: &str) -> Option<String> {
    let sort_key = agents::round_id_sort_key(round_id);
    match sort_key.len() {
        14 => NaiveDateTime::parse_from_str(sort_key, "%Y%m%d%H%M%S")
            .ok()
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
        12 => {
            let padded = format!("{}00", sort_key);
            NaiveDateTime::parse_from_str(&padded, "%Y%m%d%H%M%S")
                .ok()
                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        }
        _ => None,
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<Option<HttpRequest>, String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut header_end = None;
    let mut content_length = 0usize;

    loop {
        let read = timeout(REQUEST_READ_TIMEOUT, stream.read(&mut chunk))
            .await
            .map_err(|_| "Timed out while reading browser request".to_string())?
            .map_err(|e| format!("Failed to read request: {}", e))?;
        if read == 0 {
            if buffer.is_empty() {
                return Ok(None);
            }
            if header_end.is_none() {
                return Err(
                    "Connection closed before complete HTTP headers were received".to_string(),
                );
            }
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if header_end.is_none() {
            header_end = find_header_end(&buffer);
            if let Some(end) = header_end {
                let header_text = String::from_utf8_lossy(&buffer[..end]);
                content_length = parse_content_length(&header_text)?;
            }
        }

        if let Some(end) = header_end {
            let total = end + 4 + content_length;
            if buffer.len() >= total {
                break;
            }
        }

        if buffer.len() > 1_048_576 {
            return Err("Request too large".to_string());
        }
    }

    let header_end = header_end.ok_or_else(|| "Malformed HTTP request".to_string())?;
    let header_text = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| "Missing HTTP request line".to_string())?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .ok_or_else(|| "Missing HTTP method".to_string())?
        .to_string();
    let target = request_parts
        .next()
        .ok_or_else(|| "Missing HTTP target".to_string())?;
    let (path, query) = split_target(target);

    let mut origin = None;
    let mut csrf_token = None;
    let mut host = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name_lower = name.trim().to_ascii_lowercase();
            if name_lower == "origin" {
                origin = Some(value.trim().to_string());
            } else if name_lower == "x-csrf-token" {
                csrf_token = Some(value.trim().to_string());
            } else if name_lower == "host" {
                host = Some(value.trim().to_string());
            }
        }
    }

    let body_start = header_end + 4;
    let body_end = body_start + content_length;
    let body = buffer
        .get(body_start..body_end)
        .ok_or_else(|| "HTTP body shorter than advertised".to_string())?
        .to_vec();

    Ok(Some(HttpRequest {
        method,
        path,
        query,
        body,
        origin,
        csrf_token,
        host,
    }))
}

async fn write_response(stream: &mut TcpStream, response: HttpResponse) -> Result<(), String> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(
        format!("HTTP/1.1 {} {}\r\n", response.status, response.reason).as_bytes(),
    );
    bytes.extend_from_slice(format!("Content-Type: {}\r\n", response.content_type).as_bytes());
    bytes.extend_from_slice(format!("Content-Length: {}\r\n", response.body.len()).as_bytes());
    bytes.extend_from_slice(b"Cache-Control: no-store\r\n");
    bytes.extend_from_slice(b"Connection: close\r\n");
    for (name, value) in response.extra_headers {
        bytes.extend_from_slice(format!("{}: {}\r\n", name, value).as_bytes());
    }
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(&response.body);
    stream
        .write_all(&bytes)
        .await
        .map_err(|e| format!("Failed to write browser response: {}", e))
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(header_text: &str) -> Result<usize, String> {
    for line in header_text.lines() {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            return value
                .trim()
                .parse::<usize>()
                .map_err(|e| format!("Invalid Content-Length: {}", e));
        }
    }
    Ok(0)
}

fn split_target(target: &str) -> (String, String) {
    if let Some((path, query)) = target.split_once('?') {
        (path.to_string(), query.to_string())
    } else {
        (target.to_string(), String::new())
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        params.insert(percent_decode(key), percent_decode(value));
    }
    params
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hi = decode_hex(bytes[index + 1]);
                let lo = decode_hex(bytes[index + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi << 4) | lo);
                    index += 3;
                } else {
                    out.push(bytes[index]);
                    index += 1;
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn decode_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, String> {
    serde_json::from_slice(body).map_err(|e| format!("Invalid JSON body: {}", e))
}

fn random_offset() -> u16 {
    let mut bytes = [0u8; 2];
    if let Err(e) = getrandom::fill(&mut bytes) {
        eprintln!(
            "Warning: failed to read random bytes for port selection: {}",
            e
        );
        return 0;
    }
    u16::from_le_bytes(bytes) % 10_000
}

fn generate_csrf_token() -> String {
    let mut bytes = [0u8; 32];
    if let Err(e) = getrandom::fill(&mut bytes) {
        panic!(
            "Failed to generate CSRF token from OS entropy: {}. \
             Cannot start the dashboard server without secure CSRF protection.",
            e
        );
    }
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn html_response(body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        reason: "OK",
        content_type: "text/html; charset=utf-8",
        body: body.as_bytes().to_vec(),
        extra_headers: Vec::new(),
    }
}

fn text_response(content_type: &'static str, body: &str) -> HttpResponse {
    HttpResponse {
        status: 200,
        reason: "OK",
        content_type,
        body: body.as_bytes().to_vec(),
        extra_headers: Vec::new(),
    }
}

fn json_response(body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status: 200,
        reason: "OK",
        content_type: "application/json; charset=utf-8",
        body,
        extra_headers: Vec::new(),
    }
}

fn bad_request(message: String) -> HttpResponse {
    HttpResponse {
        status: 400,
        reason: "Bad Request",
        content_type: "application/json; charset=utf-8",
        body: json!({ "error": message }).to_string().into_bytes(),
        extra_headers: Vec::new(),
    }
}

fn internal_error(message: String) -> HttpResponse {
    HttpResponse {
        status: 500,
        reason: "Internal Server Error",
        content_type: "application/json; charset=utf-8",
        body: json!({ "error": message }).to_string().into_bytes(),
        extra_headers: Vec::new(),
    }
}

fn not_found() -> HttpResponse {
    HttpResponse {
        status: 404,
        reason: "Not Found",
        content_type: "application/json; charset=utf-8",
        body: json!({ "error": "Not found" }).to_string().into_bytes(),
        extra_headers: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bugfix::SeverityLevel;
    use crate::bugfix_log;
    use crate::bugfix_session::{BugfixSession, SessionStatus};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn percent_decode_handles_spaces_and_hex() {
        assert_eq!(percent_decode("repo%2FREADME.md"), "repo/README.md");
        assert_eq!(percent_decode("hello+world"), "hello world");
    }

    #[tokio::test]
    async fn bind_random_port_uses_requested_range() {
        let listener = bind_random_port().await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!((20_000..=29_999).contains(&port));
    }

    #[tokio::test]
    async fn server_serves_status_and_notes_endpoints() {
        let repo_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("README.md"), "# Repo\n").unwrap();
        bugfix_log::ensure_user_notes_section(state_dir.path(), "main").unwrap();

        let session = BugfixSession::new(
            state_dir.path().to_path_buf(),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );
        let server = start(session).await.unwrap();

        let status_response = http_get(server.port, "/api/status").await;
        assert!(status_response.contains("200 OK"));
        assert!(status_response.contains("\"branch\":\"main\""));

        let notes_response = http_get(server.port, "/api/notes").await;
        assert!(notes_response.contains("200 OK"));
        assert!(notes_response.contains("\"content\""));
    }

    #[tokio::test]
    async fn start_endpoint_promotes_waiting_session() {
        let state_dir = tempfile::tempdir().unwrap();
        bugfix_log::ensure_user_notes_section(state_dir.path(), "main").unwrap();

        let session = BugfixSession::new(
            state_dir.path().to_path_buf(),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );
        session.mark_waiting_to_start().await;

        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/api/start".to_string(),
            query: String::new(),
            body: br#"{}"#.to_vec(),
            origin: Some("http://127.0.0.1:23000".to_string()),
            csrf_token: Some("token".to_string()),
            host: Some("127.0.0.1:23000".to_string()),
        };
        let (quit_tx, _) = watch::channel(false);

        let response = route(request, session.clone(), "token", &quit_tx).await;

        assert_eq!(response.status, 200);
        let payload: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(payload["started"], true);
        assert_eq!(payload["status"]["status"], "starting");
        let snapshot = session.snapshot().await;
        assert_eq!(snapshot.status, SessionStatus::Starting);
        assert_eq!(snapshot.current_step_label, "Starting bugfix session");
    }

    #[tokio::test]
    async fn start_endpoint_reports_when_start_is_ignored() {
        let state_dir = tempfile::tempdir().unwrap();
        bugfix_log::ensure_user_notes_section(state_dir.path(), "main").unwrap();

        let session = BugfixSession::new(
            state_dir.path().to_path_buf(),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );

        let request = HttpRequest {
            method: "POST".to_string(),
            path: "/api/start".to_string(),
            query: String::new(),
            body: br#"{}"#.to_vec(),
            origin: Some("http://127.0.0.1:23000".to_string()),
            csrf_token: Some("token".to_string()),
            host: Some("127.0.0.1:23000".to_string()),
        };
        let (quit_tx, _) = watch::channel(false);

        let response = route(request, session.clone(), "token", &quit_tx).await;

        assert_eq!(response.status, 200);
        let payload: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(payload["started"], false);
        assert_eq!(payload["status"]["status"], "starting");
    }

    #[tokio::test]
    async fn build_doc_index_only_returns_branch_state_artifacts() {
        let repo_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo_dir.path().join("reports")).unwrap();
        std::fs::write(repo_dir.path().join("README.md"), "# Repo\n").unwrap();
        std::fs::write(
            repo_dir.path().join("reports").join("noise.md"),
            "# Noise\n",
        )
        .unwrap();
        bugfix_log::ensure_user_notes_section(state_dir.path(), "main").unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260316153045n000000000001-opus-main.md"),
            "latest opus review",
        )
        .unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260316153045n000000000001-codex-main.md"),
            "latest codex review",
        )
        .unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260315153045n000000000001-opus-main.md"),
            "older review",
        )
        .unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260316153045n000000000001-consolidated-main.md"),
            "latest consolidated",
        )
        .unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260315153045n000000000001-consolidated-main.md"),
            "older consolidated",
        )
        .unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260316153045n000000000001-opus-other.md"),
            "other branch review",
        )
        .unwrap();
        std::fs::write(
            state_dir
                .path()
                .join("20260316153045n000000000001-consolidated-other.md"),
            "other branch consolidated",
        )
        .unwrap();
        std::fs::write(state_dir.path().join("notes.md"), "ignore me").unwrap();

        let session = BugfixSession::new(
            state_dir.path().to_path_buf(),
            "repo".to_string(),
            "main".to_string(),
            "main".to_string(),
            vec!["codex".to_string(), "opus".to_string()],
            60,
            SeverityLevel::High,
            "bugfix-main.log.md".to_string(),
        );
        session
            .finish_review_round("20260316153045n000000000001")
            .await;
        session
            .set_latest_report(Some(
                "20260316153045n000000000001-consolidated-main.md".to_string(),
            ))
            .await;

        let snapshot = session.snapshot().await;
        let index = build_doc_index(&session, &snapshot).unwrap();
        let paths: Vec<String> = index.iter().map(|entry| entry.entry.path.clone()).collect();
        let titles: Vec<String> = index
            .iter()
            .map(|entry| entry.entry.title.clone())
            .collect();

        assert_eq!(
            paths,
            vec![
                "bugfix-main.log.md",
                "20260316153045n000000000001-consolidated-main.md",
                "20260316153045n000000000001-codex-main.md",
                "20260316153045n000000000001-opus-main.md",
                "20260315153045n000000000001-consolidated-main.md",
                "20260315153045n000000000001-opus-main.md",
            ]
        );
        assert_eq!(titles[0], "Bugfix");
        assert_eq!(titles[1], "consolidated-main.md");
        assert_eq!(titles[2], "codex-main.md");
        assert_eq!(titles[3], "opus-main.md");
        assert_eq!(index[0].entry.kind, DocKind::BugfixLog);
        assert!(index[0].entry.pinned);
        assert_eq!(
            index[1].entry.round_label.as_deref(),
            Some("2026-03-16 15:30:45")
        );
        assert_eq!(index[1].entry.kind, DocKind::Consolidated);
        assert!(index[1].entry.is_latest);
        assert_eq!(index[2].entry.kind, DocKind::Review);
        assert!(index[2].entry.is_latest);
        assert_eq!(
            index[4].entry.round_label.as_deref(),
            Some("2026-03-15 15:30:45")
        );
        assert!(!index[4].entry.is_latest);
        assert!(!paths.contains(&"README.md".to_string()));
        assert!(!paths.contains(&"reports/noise.md".to_string()));
        assert!(!paths.contains(&"notes.md".to_string()));
        assert!(!paths.contains(&"20260316153045n000000000001-opus-other.md".to_string()));
    }

    async fn http_get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
            path
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    }
}
