//! The server-to-client direction: talking *back* to the connected client
//! mid-tool-call, and the `UserHandle` implementation built on it.
//!
//! A tools-only server only ever responds. The moment a tool wants to ask
//! the user something — which is what AARG's copilots do — the server must
//! become a JSON-RPC *client* too: send an `elicitation/create` request and
//! await the answer while still serving the connection. [`McpClient`] is the
//! handle that makes that possible; [`ElicitationUser`] is the fourth
//! `UserHandle` implementation (after `InteractiveUser`, `NonInteractiveUser`,
//! `ScriptedUser`) that routes `ask`/`confirm` through it. The copilots in
//! `commands::tailor::run` don't change at all — they speak `UserHandle`, and
//! this is just a new voice for it.
//!
//! Capability-gated: a client announces `elicitation` support at `initialize`.
//! When it didn't, [`ElicitationUser::is_interactive`] returns false, so the
//! copilots skip exactly as they do for a piped CLI run — no dialog is ever
//! sent to a client that can't show one.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::user::{Answer, AskError, Question, UserHandle};

/// A handle a parked tool handler uses to send a request to the client and
/// await its reply. Cloneable and cheap: it shares the server's outbound
/// channel and pending-response registry. The id counter and capability flag
/// are shared too, so every clone agrees on what the client supports.
#[derive(Clone)]
pub(super) struct McpClient {
    /// Serialized JSON lines headed for stdout (drained by the writer task).
    outbound: mpsc::Sender<String>,
    /// Outbound request id → where to deliver its response. The server's read
    /// loop fulfils these when a matching response arrives.
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    /// Monotonic id for our outbound requests. A separate namespace from the
    /// client's request ids — we route incoming messages by whether they carry
    /// a `method`, so the two never collide.
    next_id: Arc<AtomicI64>,
    /// Whether the client announced `elicitation` support at `initialize`.
    elicitation: Arc<AtomicBool>,
}

impl McpClient {
    pub(super) fn new(
        outbound: mpsc::Sender<String>,
        pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    ) -> Self {
        Self {
            outbound,
            pending,
            next_id: Arc::new(AtomicI64::new(1)),
            elicitation: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Record (at `initialize`) whether the client can show elicitation
    /// dialogs.
    pub(super) fn set_elicitation_supported(&self, supported: bool) {
        self.elicitation.store(supported, Ordering::SeqCst);
    }

    pub(super) fn supports_elicitation(&self) -> bool {
        self.elicitation.load(Ordering::SeqCst)
    }

    /// Send an `elicitation/create` request and await the user's response.
    /// Registers a one-shot keyed by the request id, writes the request, and
    /// parks until the read loop delivers the matching response.
    async fn elicit(&self, message: &str, schema: Value) -> Result<Elicited, ElicitError> {
        if !self.supports_elicitation() {
            return Err(ElicitError::Unsupported);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "elicitation/create",
            "params": { "message": message, "requestedSchema": schema },
        });
        self.outbound
            .send(request.to_string())
            .await
            .map_err(|_| ElicitError::Closed)?;

        let response = rx.await.map_err(|_| ElicitError::Closed)?;
        Ok(parse_elicited(response))
    }
}

/// The user's verdict on one elicitation. `accept` carries the form content;
/// `decline`/`cancel` carry nothing (the user said no, or dismissed it).
enum Elicited {
    Accept(Value),
    Decline,
    Cancel,
}

/// Why an elicitation couldn't produce an answer (distinct from the user
/// answering "no", which is an `Elicited::Decline`).
#[derive(Debug, thiserror::Error)]
enum ElicitError {
    #[error("the client does not support elicitation")]
    Unsupported,
    #[error("the elicitation channel closed before a reply arrived")]
    Closed,
}

/// Read `{action, content}` out of a JSON-RPC response to `elicitation/create`.
/// A transport-level error response, or a missing/garbled action, is treated
/// as a cancel — the safe reading is "no answer", never a fabricated one.
fn parse_elicited(response: Value) -> Elicited {
    if response.get("error").is_some() {
        return Elicited::Cancel;
    }
    let result = response.get("result").cloned().unwrap_or(Value::Null);
    match result.get("action").and_then(Value::as_str) {
        Some("accept") => Elicited::Accept(result.get("content").cloned().unwrap_or(Value::Null)),
        Some("decline") => Elicited::Decline,
        _ => Elicited::Cancel,
    }
}

/// The fourth `UserHandle`: a present user reachable over MCP. Maps the trait's
/// three question kinds onto elicitation form-mode fields, and `confirm` onto a
/// boolean. The copilots gate every prompt on `is_interactive()`, so when the
/// client can't elicit this whole surface goes quiet.
pub(super) struct ElicitationUser {
    client: McpClient,
}

impl ElicitationUser {
    pub(super) fn new(client: McpClient) -> Self {
        Self { client }
    }
}

/// A flat single-field form schema: `{ type: object, properties: { name: field
/// }, required: [name] }`. Elicitation form mode allows only flat primitives,
/// which is exactly what `Question` needs.
fn form(field_name: &str, field: Value) -> Value {
    json!({
        "type": "object",
        "properties": { field_name: field },
        "required": [field_name],
    })
}

#[async_trait]
impl UserHandle for ElicitationUser {
    async fn ask(&self, question: Question) -> Result<Answer, AskError> {
        match question {
            Question::Select { prompt, options } => {
                let schema = form(
                    "choice",
                    json!({"type": "string", "title": "choice", "enum": options.clone()}),
                );
                match self.client.elicit(&prompt, schema).await {
                    Ok(Elicited::Accept(content)) => {
                        let chosen = content.get("choice").and_then(Value::as_str).unwrap_or("");
                        let index = options.iter().position(|o| o == chosen).unwrap_or_default();
                        Ok(Answer::Choice(index))
                    }
                    // Declining or dismissing a required single choice aborts
                    // the step, the same as pressing Esc on the CLI's prompt.
                    _ => Err(AskError::NotInteractive { what: prompt }),
                }
            }
            Question::MultiSelect { prompt, options } => {
                let schema = form(
                    "choices",
                    json!({
                        "type": "array",
                        "title": "choices",
                        "items": {"type": "string", "enum": options.clone()},
                    }),
                );
                match self.client.elicit(&prompt, schema).await {
                    Ok(Elicited::Accept(content)) => {
                        let picked: Vec<&str> = content
                            .get("choices")
                            .and_then(Value::as_array)
                            .map(|items| items.iter().filter_map(Value::as_str).collect())
                            .unwrap_or_default();
                        let indexes = options
                            .iter()
                            .enumerate()
                            .filter(|(_, o)| picked.contains(&o.as_str()))
                            .map(|(i, _)| i)
                            .collect();
                        Ok(Answer::Choices(indexes))
                    }
                    // A dismissed multi-select means "none of them" — a valid,
                    // empty answer, not an error (matches "check none and
                    // continue" on the CLI's checklist).
                    Ok(_) => Ok(Answer::Choices(Vec::new())),
                    Err(_) => Err(AskError::NotInteractive { what: prompt }),
                }
            }
            Question::Text { prompt } => {
                let schema = form("text", json!({"type": "string", "title": "response"}));
                match self.client.elicit(&prompt, schema).await {
                    Ok(Elicited::Accept(content)) => Ok(Answer::Text(
                        content
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    )),
                    _ => Err(AskError::NotInteractive { what: prompt }),
                }
            }
        }
    }

    async fn confirm(&self, prompt: &str, default: bool) -> Result<bool, AskError> {
        let schema = form(
            "confirm",
            json!({"type": "boolean", "title": "confirm", "default": default}),
        );
        match self.client.elicit(prompt, schema).await {
            Ok(Elicited::Accept(content)) => Ok(content
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(default)),
            // Declining an optional detour means "no, skip it" — the safe read
            // for a copilot offer (don't run the optional step).
            Ok(_) => Ok(false),
            // No channel to ask on: fall back to the offer's default, like a
            // non-interactive run.
            Err(_) => Ok(default),
        }
    }

    fn notify(&self, message: &str) {
        // To stderr, which the client captures as diagnostics. Surfacing these
        // as in-client `notifications/message` logs is a possible refinement.
        eprintln!("{message}");
    }

    fn is_interactive(&self) -> bool {
        // The gate that makes the whole copilot surface capability-aware: false
        // when the client can't elicit, so the offers are skipped, not failed.
        self.client.supports_elicitation()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// A test double for the client end of the wire: it hands back an
    /// `McpClient` and lets the test read the request the server emitted and
    /// push the response back, standing in for a real MCP client.
    struct TestPeer {
        outbound_rx: mpsc::Receiver<String>,
        pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    }

    fn wired(elicitation: bool) -> (McpClient, TestPeer) {
        let (tx, rx) = mpsc::channel(8);
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let client = McpClient::new(tx, pending.clone());
        client.set_elicitation_supported(elicitation);
        (
            client,
            TestPeer {
                outbound_rx: rx,
                pending,
            },
        )
    }

    impl TestPeer {
        /// Read the next request the server emitted, returning its id and the
        /// schema it asked for.
        async fn next_request(&mut self) -> Value {
            let line = self.outbound_rx.recv().await.unwrap();
            serde_json::from_str(&line).unwrap()
        }

        /// Deliver a response to the request with `id`, as the real read loop
        /// would.
        async fn respond(&self, id: i64, result: Value) {
            let tx = self.pending.lock().await.remove(&id).unwrap();
            let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
        }
    }

    #[tokio::test]
    async fn a_select_becomes_an_enum_form_and_maps_the_answer_to_its_index() {
        let (client, mut peer) = wired(true);
        let user = ElicitationUser::new(client);

        // Run the ask concurrently with the peer answering it.
        let ask = tokio::spawn(async move {
            user.ask(Question::Select {
                prompt: "pick".into(),
                options: vec!["a".into(), "b".into(), "c".into()],
            })
            .await
        });

        let request = peer.next_request().await;
        assert_eq!(request["method"], "elicitation/create");
        // The Select turned into a single-enum form field.
        let field = &request["params"]["requestedSchema"]["properties"]["choice"];
        assert_eq!(field["enum"], json!(["a", "b", "c"]));
        let id = request["id"].as_i64().unwrap();
        peer.respond(id, json!({"action": "accept", "content": {"choice": "b"}}))
            .await;

        // "b" is option index 1.
        assert_eq!(ask.await.unwrap().unwrap(), Answer::Choice(1));
    }

    #[tokio::test]
    async fn a_declined_confirm_is_no_even_when_the_default_is_true() {
        let (client, mut peer) = wired(true);
        let user = ElicitationUser::new(client);

        // Drive the confirm and the peer's reply concurrently in one task: the
        // confirm parks after sending its request, the peer reads it and
        // declines, then the confirm resolves. (Awaiting the confirm *before*
        // reading the request would deadlock — nothing sends it.)
        let (result, ()) = tokio::join!(user.confirm("proceed?", true), async {
            let request = peer.next_request().await;
            let id = request["id"].as_i64().unwrap();
            peer.respond(id, json!({"action": "decline"})).await;
        });
        // Declining the offer is "no", even though the default was true.
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn without_capability_the_user_is_not_interactive_and_confirm_takes_the_default() {
        let (client, _peer) = wired(false);
        let user = ElicitationUser::new(client);
        // No capability → the copilots' is_interactive() gate is false.
        assert!(!user.is_interactive());
        // And a stray confirm falls back to the default rather than hanging.
        assert!(user.confirm("x", true).await.unwrap());
        assert!(!user.confirm("x", false).await.unwrap());
    }
}
