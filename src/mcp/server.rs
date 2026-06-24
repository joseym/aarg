//! The stdio transport loop, as a small actor system.
//!
//! A tools-only server could be a flat read→dispatch→write loop. Elicitation
//! changes that: while a tool is running it may send its *own* request to the
//! client (`elicitation/create`) and park awaiting the reply — so the loop has
//! to keep reading stdin to deliver that reply. The three concerns are split
//! into three tasks that pass messages over channels:
//!
//! - **read loop** (this task): read each line, [`classify`] it. A client
//!   request goes to the worker; a response (to one of *our* elicitations) is
//!   routed to whoever's awaiting it, by id.
//! - **worker**: dispatch inbound requests one at a time. One-at-a-time
//!   serializes tool execution (no two tailors mutating the dataset at once);
//!   because it's a separate task, it can park inside a tool awaiting an
//!   elicitation while the read loop keeps delivering.
//! - **writer**: the single owner of stdout, draining serialized lines from
//!   both the worker (responses) and the client handle (outbound requests).
//!
//! The inviolable rule still holds: **stdout carries only MCP messages**, all
//! logs go to stderr.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::McpError;
use super::client::McpClient;
use super::protocol::{
    CallToolParams, INTERNAL_ERROR, INVALID_PARAMS, Implementation, InitializeParams,
    InitializeResult, ListResourcesResult, ListToolsResult, METHOD_NOT_FOUND, PARSE_ERROR,
    PROTOCOL_VERSION, ReadResourceParams, Request, ResourcesCapability, Response, RpcError,
    ServerCapabilities, ToolsCapability,
};
use super::tools;

/// The registry the read loop fulfils: an outbound request's id → the one-shot
/// that delivers its response to the awaiting handler.
type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

/// Serve MCP over stdio until the client closes the connection (EOF on stdin).
/// Returns `Ok` on a clean disconnect; only a genuine transport IO error
/// surfaces as `McpError`.
pub async fn serve() -> Result<(), McpError> {
    let (outbound_tx, outbound_rx) = mpsc::channel::<String>(64);
    let (inbound_tx, inbound_rx) = mpsc::channel::<Request>(64);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));

    let writer = tokio::spawn(writer_task(outbound_rx));
    let worker = tokio::spawn(worker_task(
        inbound_rx,
        outbound_tx.clone(),
        pending.clone(),
    ));

    log(&format!(
        "aarg MCP server ready on stdio (protocol {PROTOCOL_VERSION})"
    ));

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match classify(line) {
            Classified::Request(request) => {
                if inbound_tx.send(request).await.is_err() {
                    break; // worker gone; nothing more we can do
                }
            }
            Classified::Response { id, body } => {
                // A response to one of our elicitations: hand it to the waiter.
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let _ = tx.send(body);
                } else {
                    log(&format!("dropping response for unknown id {id}"));
                }
            }
            Classified::Unparseable => {
                let response = Response::failure(
                    Value::Null,
                    RpcError::new(PARSE_ERROR, "message was not valid JSON-RPC"),
                );
                if let Ok(line) = serde_json::to_string(&response) {
                    let _ = outbound_tx.send(line).await;
                }
            }
        }
    }

    // EOF: close the worker (drop its inbound sender), let it finish, then drop
    // the last outbound senders so the writer drains and exits.
    drop(inbound_tx);
    let _ = worker.await;
    drop(outbound_tx);
    let _ = writer.await;
    log("client disconnected; shutting down");
    Ok(())
}

/// The single owner of stdout: write each line followed by a newline and
/// flush, so every MCP message lands whole and in order.
async fn writer_task(mut outbound_rx: mpsc::Receiver<String>) {
    let mut stdout = tokio::io::stdout();
    while let Some(line) = outbound_rx.recv().await {
        if stdout.write_all(line.as_bytes()).await.is_err()
            || stdout.write_all(b"\n").await.is_err()
            || stdout.flush().await.is_err()
        {
            break; // the pipe closed; stop writing
        }
    }
}

/// Dispatch inbound requests one at a time. The `McpClient` it builds shares
/// the outbound channel and pending registry, so a tool it runs can elicit.
async fn worker_task(
    mut inbound_rx: mpsc::Receiver<Request>,
    outbound: mpsc::Sender<String>,
    pending: Pending,
) {
    let client = McpClient::new(outbound.clone(), pending);
    while let Some(request) = inbound_rx.recv().await {
        if let Some(response) = respond(&request, &client).await
            && let Ok(line) = serde_json::to_string(&response)
        {
            let _ = outbound.send(line).await;
        }
    }
}

/// What an incoming line turned out to be.
enum Classified {
    /// A client request or notification (it carries a `method`).
    Request(Request),
    /// A response to one of *our* outbound requests (an `id`, no `method`).
    Response { id: i64, body: Value },
    /// Neither — bad JSON, or a shape we can't route.
    Unparseable,
}

/// Split an incoming line. The distinguishing bit is `method`: a request and a
/// notification both carry one; a response to our elicitation does not.
fn classify(line: &str) -> Classified {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Classified::Unparseable,
    };
    if value.get("method").is_some() {
        match serde_json::from_value::<Request>(value) {
            Ok(request) => Classified::Request(request),
            Err(_) => Classified::Unparseable,
        }
    } else if let Some(id) = value.get("id").and_then(Value::as_i64) {
        Classified::Response { id, body: value }
    } else {
        Classified::Unparseable
    }
}

/// Run one request and shape the result into the response to send (or `None`
/// for a notification). Separated from the worker loop so tests can drive it.
async fn respond(request: &Request, client: &McpClient) -> Option<Response> {
    let wants_response = request.wants_response();
    let id = request.response_id();
    match (dispatch(request, client).await, wants_response) {
        (Ok(result), true) => Some(Response::success(id, result)),
        (Ok(_), false) => None,
        (Err(error), true) => Some(Response::failure(id, error)),
        (Err(error), false) => {
            log(&format!(
                "notification {:?} failed: {}",
                request.method, error.message
            ));
            None
        }
    }
}

/// Route a request by method to its handler.
async fn dispatch(request: &Request, client: &McpClient) -> Result<Value, RpcError> {
    match request.method.as_str() {
        "initialize" => initialize(request.params.clone(), client),
        "notifications/initialized" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => tools_list(),
        "tools/call" => tools_call(request.params.clone(), client).await,
        "resources/list" => resources_list(),
        "resources/read" => resources_read(request.params.clone()),
        other if other.starts_with("notifications/") => Ok(Value::Null),
        other => Err(RpcError::new(
            METHOD_NOT_FOUND,
            format!("unknown method {other:?}"),
        )),
    }
}

/// The lifecycle handshake. Besides echoing a supported protocol version and
/// advertising the tools capability, this is where we record whether the
/// client can show elicitation dialogs — the bit that decides whether the
/// tailor copilots speak up.
fn initialize(params: Option<Value>, client: &McpClient) -> Result<Value, RpcError> {
    let parsed: Option<InitializeParams> =
        match params {
            Some(value) => Some(serde_json::from_value(value).map_err(|e| {
                RpcError::new(INVALID_PARAMS, format!("bad initialize params: {e}"))
            })?),
            None => None,
        };

    let supports_elicitation = parsed
        .as_ref()
        .and_then(|p| p.capabilities.as_ref())
        .and_then(|caps| caps.get("elicitation"))
        .is_some();
    client.set_elicitation_supported(supports_elicitation);

    if let Some(info) = parsed.as_ref().and_then(|p| p.client_info.as_ref()) {
        log(&format!(
            "client connected: {} {} (elicitation: {supports_elicitation})",
            info.name, info.version
        ));
    }
    let version = parsed
        .and_then(|p| p.protocol_version)
        .unwrap_or_else(|| PROTOCOL_VERSION.to_string());

    let result = InitializeResult {
        protocol_version: version,
        capabilities: ServerCapabilities {
            tools: Some(ToolsCapability::default()),
            resources: Some(ResourcesCapability::default()),
        },
        server_info: Implementation {
            name: "aarg".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            title: Some("AARG resume tailor".to_string()),
        },
        instructions: Some(
            "AARG tailors the user's recorded resume to a job description without \
             fabricating experience. Start with `dataset_summary` to see what's on \
             file. Use `parse_job` and `analyze_gap` to understand a posting and the \
             user's fit, then `tailor` to produce a build with rendered PDFs. \
             `ingest` rebuilds the dataset from resume text and overwrites it. When \
             you support elicitation, `tailor` may ask the user to confirm and refine \
             weak lines mid-run, just as the CLI does."
                .to_string(),
        ),
    };
    serde_json::to_value(result).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))
}

/// The tool catalogue.
fn tools_list() -> Result<Value, RpcError> {
    let result = ListToolsResult {
        tools: tools::descriptors(),
    };
    serde_json::to_value(result).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))
}

/// Invoke a tool. Bad params are a JSON-RPC error; a tool that runs and fails
/// reports it in-band via the result's `isError` flag. The client handle is
/// threaded through so a tool can elicit.
async fn tools_call(params: Option<Value>, client: &McpClient) -> Result<Value, RpcError> {
    let raw = params.ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call requires params"))?;
    let call: CallToolParams = serde_json::from_value(raw)
        .map_err(|e| RpcError::new(INVALID_PARAMS, format!("bad tools/call params: {e}")))?;
    let arguments = call.arguments.unwrap_or(Value::Null);
    let result = tools::call(&call.name, arguments, client).await;
    serde_json::to_value(result).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))
}

/// The resource catalogue: every build's rendered PDFs.
fn resources_list() -> Result<Value, RpcError> {
    let result = ListResourcesResult {
        resources: tools::list_resources(),
    };
    serde_json::to_value(result).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))
}

/// Read one resource by uri (`aarg://build/<id>/<file>.pdf`). An unknown,
/// malformed, or out-of-bounds uri is an invalid-params error.
fn resources_read(params: Option<Value>) -> Result<Value, RpcError> {
    let raw =
        params.ok_or_else(|| RpcError::new(INVALID_PARAMS, "resources/read requires params"))?;
    let params: ReadResourceParams = serde_json::from_value(raw)
        .map_err(|e| RpcError::new(INVALID_PARAMS, format!("bad resources/read params: {e}")))?;
    match tools::read_resource(&params.uri) {
        Ok(result) => {
            serde_json::to_value(result).map_err(|e| RpcError::new(INTERNAL_ERROR, e.to_string()))
        }
        Err(message) => Err(RpcError::new(INVALID_PARAMS, message)),
    }
}

/// Every server log line goes to stderr — stdout is reserved for MCP messages,
/// and a single stray byte there corrupts the JSON-RPC stream.
fn log(message: &str) {
    eprintln!("{}", crate::style::dim(format!("mcp: {message}")));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    /// A client handle wired to dead channels — enough to drive `dispatch`
    /// without a live peer (these tests never elicit).
    fn test_client() -> McpClient {
        let (tx, _rx) = mpsc::channel::<String>(1);
        McpClient::new(tx, Arc::new(Mutex::new(HashMap::new())))
    }

    fn request(method: &str, params: Option<Value>) -> Request {
        Request {
            jsonrpc: "2.0".to_string(),
            id: Some(json!(1)),
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn initialize_advertises_tools_and_echoes_the_protocol_version() {
        let params = json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "1.0"}
        });
        let result = dispatch(&request("initialize", Some(params)), &test_client())
            .await
            .unwrap();
        assert_eq!(result["protocolVersion"], "2025-06-18");
        assert_eq!(result["serverInfo"]["name"], "aarg");
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn initialize_records_the_clients_elicitation_capability() {
        let client = test_client();
        assert!(!client.supports_elicitation());
        // A client that declares elicitation flips the flag on.
        dispatch(
            &request(
                "initialize",
                Some(json!({"capabilities": {"elicitation": {}}})),
            ),
            &client,
        )
        .await
        .unwrap();
        assert!(client.supports_elicitation());
    }

    #[tokio::test]
    async fn initialize_without_a_version_falls_back_to_ours() {
        let result = dispatch(&request("initialize", Some(json!({}))), &test_client())
            .await
            .unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn ping_returns_an_empty_object() {
        let result = dispatch(&request("ping", None), &test_client())
            .await
            .unwrap();
        assert_eq!(result, json!({}));
    }

    #[tokio::test]
    async fn tools_list_includes_the_flagship_and_read_tools() {
        let result = dispatch(&request("tools/list", None), &test_client())
            .await
            .unwrap();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(names.contains(&"tailor"));
        assert!(names.contains(&"dataset_summary"));
        assert!(names.contains(&"analyze_gap"));
        for tool in result["tools"].as_array().unwrap() {
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[tokio::test]
    async fn an_unknown_method_is_method_not_found() {
        let error = dispatch(&request("does/not/exist", None), &test_client())
            .await
            .unwrap_err();
        assert_eq!(error.code, METHOD_NOT_FOUND);
    }

    #[test]
    fn classify_splits_requests_responses_and_garbage() {
        // A request carries a method.
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
            Classified::Request(_)
        ));
        // A notification carries a method too (no id) — still a Request.
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            Classified::Request(_)
        ));
        // A response to our elicitation: an id, no method.
        match classify(r#"{"jsonrpc":"2.0","id":7,"result":{"action":"accept"}}"#) {
            Classified::Response { id, .. } => assert_eq!(id, 7),
            _ => panic!("expected a response"),
        }
        assert!(matches!(classify("{ not json"), Classified::Unparseable));
        // No method and no id: nothing to route.
        assert!(matches!(
            classify(r#"{"jsonrpc":"2.0"}"#),
            Classified::Unparseable
        ));
    }

    #[tokio::test]
    async fn a_notification_produces_no_response() {
        let note = Request {
            jsonrpc: "2.0".to_string(),
            id: None,
            method: "notifications/initialized".to_string(),
            params: None,
        };
        assert!(respond(&note, &test_client()).await.is_none());
    }

    #[tokio::test]
    async fn an_unknown_method_request_gets_an_error_response() {
        let response = respond(&request("nope", None), &test_client())
            .await
            .unwrap();
        let wire = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["id"], json!(1));
        assert_eq!(wire["error"]["code"], json!(METHOD_NOT_FOUND));
    }
}
