//! The Model Context Protocol wire types, hand-written.
//!
//! This is the `LlmClient` / `reqwest` move applied to MCP: rather than
//! adopt the `rmcp` SDK, the protocol is a handful of `serde` structs and
//! a read/dispatch/write loop ([`super::server`]). MCP over stdio is just
//! JSON-RPC 2.0 with newline-delimited messages, and a tools-only server
//! needs only a small slice of it — `initialize`, `tools/list`,
//! `tools/call`, `ping`, and the JSON-RPC error envelope. Everything in
//! this file is that slice, and nothing more.
//!
//! Field names on the wire are camelCase (`protocolVersion`,
//! `inputSchema`, `isError`); `#[serde(rename_all = "camelCase")]` carries
//! the Rust snake_case names across that gap, so the rest of the codebase
//! reads in its own idiom.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP protocol revision this server speaks. Stable as of mid-2026;
/// the lifecycle handshake echoes the client's requested version when we
/// support it, falling back to this.
pub const PROTOCOL_VERSION: &str = "2025-11-25";

// ---------------------------------------------------------------------
// JSON-RPC 2.0 envelope
// ---------------------------------------------------------------------

/// One incoming JSON-RPC message. A *request* carries an `id` and expects
/// a response; a *notification* omits `id` and must not be answered. We
/// keep `id` optional and branch on its presence rather than model the two
/// as distinct types, because on stdio they arrive interleaved on one
/// stream and the method name already tells us which handler to run.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// Always `"2.0"`. Accepted but not enforced — a stricter check buys
    /// nothing against the one client on the other end of a pipe.
    #[allow(dead_code)]
    #[serde(default)]
    pub jsonrpc: String,
    /// Present on a request (a number or string), absent on a
    /// notification. `Value::Null` is treated as "no response expected".
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

impl Request {
    /// Whether this message expects a response. A notification (no `id`,
    /// or an explicit null `id`) is handled for its side effect and then
    /// dropped — sending a response to one is a protocol error.
    pub fn wants_response(&self) -> bool {
        !matches!(self.id, None | Some(Value::Null))
    }

    /// The id to echo in a response, defaulting to null for the rare
    /// malformed request we still want to answer.
    pub fn response_id(&self) -> Value {
        self.id.clone().unwrap_or(Value::Null)
    }
}

/// A JSON-RPC response: `result` on success, `error` on failure, never
/// both, with `id` echoing the request. Built through [`Response::success`]
/// and [`Response::failure`] so that invariant holds by construction.
#[derive(Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Value, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// The JSON-RPC error object. The codes below are the standard ones; a
/// server-defined code would sit outside the reserved -32768..=-32000 band.
#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// The message could not be parsed as JSON.
pub const PARSE_ERROR: i32 = -32700;
/// The method does not exist on this server.
pub const METHOD_NOT_FOUND: i32 = -32601;
/// The params were missing or the wrong shape for the method.
pub const INVALID_PARAMS: i32 = -32602;
/// An unexpected server-side failure while handling a valid request.
pub const INTERNAL_ERROR: i32 = -32603;

// ---------------------------------------------------------------------
// Lifecycle: initialize / initialized
// ---------------------------------------------------------------------

/// The `initialize` request params we read. The client announces its
/// protocol version and capabilities; we only act on the version (to echo
/// a supported one) and log who connected.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub client_info: Option<Implementation>,
    /// The client's declared capabilities. We read one bit of it: whether
    /// `elicitation` is present, which gates whether tools may ask the user.
    #[serde(default)]
    pub capabilities: Option<Value>,
}

/// A name/version pair identifying a participant — us in the response,
/// the client in the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Implementation {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Our `initialize` response: the agreed protocol version, what we can do
/// (tools), and who we are.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    pub server_info: Implementation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// What this server advertises it can do. A `Some` field opts the
/// capability in; the omitted ones (resources, prompts, ...) are simply
/// not offered, and a well-behaved client won't call them.
#[derive(Debug, Default, Serialize)]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
}

/// The tools capability. `listChanged` would let us notify the client when
/// the tool set changes at runtime; ours is fixed, so it's left off.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// The resources capability. We expose read-only resources (rendered PDFs);
/// `subscribe`/`listChanged` are left off — the set is browsed on demand.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

// ---------------------------------------------------------------------
// Resources (read-only artifacts: rendered PDFs)
// ---------------------------------------------------------------------

/// One resource as it appears in `resources/list`: a `uri` the client reads
/// back, a human `name`, and an optional MIME type.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// The `resources/list` result.
#[derive(Debug, Serialize)]
pub struct ListResourcesResult {
    pub resources: Vec<Resource>,
}

/// The `resources/read` request params.
#[derive(Debug, Deserialize)]
pub struct ReadResourceParams {
    pub uri: String,
}

/// The `resources/read` result: the contents of the requested resource.
#[derive(Debug, Serialize)]
pub struct ReadResourceResult {
    pub contents: Vec<ResourceContents>,
}

// ---------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------

/// One tool as it appears in `tools/list`: a name, a human description the
/// client's model reads to decide when to call it, and a JSON Schema for
/// its arguments. The schema is hand-written `serde_json::Value` — no
/// `schemars` derive — so the contract is visible at the call site.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

/// The `tools/list` result.
#[derive(Debug, Serialize)]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
}

/// The `tools/call` request params: which tool, and its arguments as a
/// free-form object the handler validates itself.
#[derive(Debug, Deserialize)]
pub struct CallToolParams {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<Value>,
}

/// The `tools/call` result. `content` is what the client shows / feeds to
/// its model; `structuredContent` is the same data as a machine-readable
/// object for clients that prefer it. `isError: true` reports a *tool*
/// failure in-band (the call reached the tool and it failed) — distinct
/// from a JSON-RPC error, which means the request itself was malformed.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}

impl CallToolResult {
    /// A successful plain-text result.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![Content::text(text)],
            is_error: None,
            structured_content: None,
        }
    }

    /// A successful structured result: the value pretty-printed as the text
    /// block (so every client shows something readable) and attached as
    /// `structuredContent` (for clients that parse it). Falls back to a
    /// plain note if the value somehow can't serialize.
    pub fn json(value: Value) -> Self {
        let text = serde_json::to_string_pretty(&value)
            .unwrap_or_else(|_| "<unserializable result>".to_string());
        Self {
            content: vec![Content::text(text)],
            is_error: None,
            structured_content: Some(value),
        }
    }

    /// A tool-level failure, reported in-band so the client's model can
    /// read the reason and adjust rather than seeing a transport error.
    pub fn failure(message: impl Into<String>) -> Self {
        Self {
            content: vec![Content::text(message)],
            is_error: Some(true),
            structured_content: None,
        }
    }

    /// Append resource-link content blocks (e.g. rendered PDFs) so the client
    /// surfaces them as openable artifacts alongside the text result.
    pub fn with_resource_links(mut self, links: impl IntoIterator<Item = Content>) -> Self {
        self.content.extend(links);
        self
    }
}

/// A single content block. MCP defines several kinds (text, image, embedded
/// resource, resource link); this server returns text plus `resource_link`s. A
/// rendered PDF is handed over as a `resource_link` pointing at its
/// `resources/read` uri, never as an embedded blob: a binary PDF inlined in the
/// model-visible content makes the client try to read it as an image and reject
/// the media type, whereas a link resolves to the PDF the client opens.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    Text {
        text: String,
    },
    /// A reference to a resource the client can fetch via `resources/read`
    /// (here, a rendered PDF). It carries no bytes, so it surfaces as an
    /// openable artifact without the client choking on a binary blob.
    #[serde(rename = "resource_link")]
    ResourceLink {
        uri: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text { text: text.into() }
    }

    /// A link to a fetchable resource (e.g. a rendered PDF by its `aarg://`
    /// uri), so a tool result hands over an openable artifact, not a path.
    pub fn resource_link(
        uri: impl Into<String>,
        name: impl Into<String>,
        mime_type: Option<String>,
    ) -> Self {
        Content::ResourceLink {
            uri: uri.into(),
            name: name.into(),
            description: None,
            mime_type,
        }
    }
}

/// The contents of one resource: UTF-8 `text` or a base64 `blob`, tagged by
/// `uri` and (usually) a `mimeType`. Returned by `resources/read`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceContents {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_request_with_an_id_wants_a_response_a_notification_does_not() {
        let req: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert!(req.wants_response());
        assert_eq!(req.response_id(), json!(1));

        let note: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(!note.wants_response());

        let null_id: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"x"}"#).unwrap();
        assert!(!null_id.wants_response());
    }

    #[test]
    fn a_success_response_carries_result_and_no_error_field() {
        let resp = Response::success(json!(7), json!({"ok": true}));
        let wire = serde_json::to_value(&resp).unwrap();
        assert_eq!(wire["jsonrpc"], "2.0");
        assert_eq!(wire["id"], json!(7));
        assert_eq!(wire["result"], json!({"ok": true}));
        // The error key is omitted entirely, not null.
        assert!(wire.get("error").is_none());
    }

    #[test]
    fn an_error_response_carries_error_and_no_result_field() {
        let resp = Response::failure(json!(7), RpcError::new(METHOD_NOT_FOUND, "no such method"));
        let wire = serde_json::to_value(&resp).unwrap();
        assert_eq!(wire["error"]["code"], json!(METHOD_NOT_FOUND));
        assert_eq!(wire["error"]["message"], "no such method");
        assert!(wire.get("result").is_none());
    }

    #[test]
    fn a_tool_text_block_serializes_with_a_type_tag() {
        let result = CallToolResult::text("hello");
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["content"][0]["type"], "text");
        assert_eq!(wire["content"][0]["text"], "hello");
        // No error flag on a success.
        assert!(wire.get("isError").is_none());
    }

    #[test]
    fn a_resource_link_serializes_with_snake_case_type_and_camel_mime() {
        let result = CallToolResult::json(json!({"build_id": "047"})).with_resource_links([
            Content::resource_link(
                "aarg://build/047/resume.ats.pdf",
                "build 047 · resume.ats.pdf",
                Some("application/pdf".to_string()),
            ),
        ]);
        let wire = serde_json::to_value(&result).unwrap();
        // The text result is still first; the link rides alongside it.
        assert_eq!(wire["content"][0]["type"], "text");
        let link = &wire["content"][1];
        assert_eq!(link["type"], "resource_link");
        assert_eq!(link["uri"], "aarg://build/047/resume.ats.pdf");
        assert_eq!(link["name"], "build 047 · resume.ats.pdf");
        assert_eq!(link["mimeType"], "application/pdf");
        // No description key when it is unset.
        assert!(link.get("description").is_none());
    }

    #[test]
    fn a_structured_result_attaches_both_text_and_structured_content() {
        let result = CallToolResult::json(json!({"count": 3}));
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["structuredContent"], json!({"count": 3}));
        // The text block is the pretty-printed JSON, so every client shows it.
        assert!(
            wire["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("count")
        );
    }

    #[test]
    fn a_failure_sets_the_in_band_error_flag() {
        let result = CallToolResult::failure("nope");
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["isError"], json!(true));
        assert_eq!(wire["content"][0]["text"], "nope");
    }

    #[test]
    fn the_initialize_result_uses_camelcase_on_the_wire() {
        let result = InitializeResult {
            protocol_version: PROTOCOL_VERSION.to_string(),
            capabilities: ServerCapabilities {
                tools: Some(ToolsCapability::default()),
                resources: None,
            },
            server_info: Implementation {
                name: "aarg".into(),
                version: "0.1.0".into(),
                title: None,
            },
            instructions: None,
        };
        let wire = serde_json::to_value(&result).unwrap();
        assert_eq!(wire["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(wire["serverInfo"]["name"], "aarg");
        // tools is present (an object), resources/prompts are absent.
        assert!(wire["capabilities"]["tools"].is_object());
        assert!(wire["capabilities"].get("resources").is_none());
    }

    #[test]
    fn resource_contents_serialize_a_blob_with_camelcase_mime() {
        let contents = ResourceContents {
            uri: "aarg://build/041/ats.pdf".into(),
            mime_type: Some("application/pdf".into()),
            text: None,
            blob: Some("QUJD".into()),
        };
        let wire = serde_json::to_value(&contents).unwrap();
        assert_eq!(wire["uri"], "aarg://build/041/ats.pdf");
        assert_eq!(wire["mimeType"], "application/pdf");
        assert_eq!(wire["blob"], "QUJD");
        // An absent text field is omitted, not null.
        assert!(wire.get("text").is_none());
    }
}
