//! MCP over stdio: the message vocabulary, and nothing else.
//!
//! MCP's stdio transport is newline-delimited JSON-RPC 2.0, and a server that
//! exposes a handful of tools needs four methods of it: `initialize`,
//! `tools/list`, `tools/call`, and `ping`. Three of those are answered
//! identically by every server that ever existed; only `tools/call` is the
//! host's own. So this crate owns the framing and those three, and hands
//! `tools/call` back to the caller.
//!
//! `prompts/list` and `prompts/get` join the same class, because a prompt is
//! *static content* exactly as the `tools` array is: the host declares it once
//! in its [`Surface`] and never sees the request. A host that declares none is
//! a tools-only server and answers `-32601` to both, unchanged.
//!
//! What it deliberately does **not** own is the loop. Luma's hosts disagree
//! about concurrency for good reasons — the GPUI harness is one blocking thread
//! per stage, while `luma-mcp` must answer `cancel` *while* a Python cell is
//! still running — and a serve loop shared between them could only be the
//! stricter of the two. Read a line, [`route`] it, write the answer.
//!
//! ```
//! use mcp_stdio::{route, Routed, ServerInfo, Surface};
//! use serde_json::json;
//!
//! let info = ServerInfo { name: "example", version: "0.1.0" };
//! let tools = json!([]);
//! match route(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, info, &Surface::new(&tools)) {
//!     Routed::Respond { response, .. } => assert_eq!(response["result"], json!({})),
//!     Routed::Call { .. } | Routed::Silent => unreachable!(),
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use serde_json::{json, Value};

/// The MCP revision these messages speak.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// How a server names itself in its `initialize` response.
#[derive(Clone, Copy, Debug)]
pub struct ServerInfo {
    /// The server's name, as the client will show it.
    pub name: &'static str,
    /// The server's version.
    pub version: &'static str,
}

/// How a client named itself in its `initialize` request.
///
/// Owned rather than borrowed because the frame it came from is one line of
/// stdin, and the host outlives it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClientInfo {
    /// The client's name, as it reported it.
    pub name: String,
    /// The client's version, or the empty string when it sent none.
    pub version: String,
}

/// Everything a server declares statically: the tools it exposes and the
/// prompts it offers.
///
/// One struct rather than two parameters because both are answered *inside*
/// [`route`] from data the host built once — growing [`Routed`] with a variant
/// per read-only method would make every host answer a question none of them
/// have an opinion about.
#[derive(Clone, Copy, Debug)]
pub struct Surface<'a> {
    /// The array `tools/list` returns. Build entries with [`tool`].
    pub tools: &'a Value,
    /// The array `prompts/list` and `prompts/get` are answered from. Build
    /// entries with [`prompt`]. Empty (or not an array) means this server has
    /// no prompts capability at all.
    pub prompts: &'a Value,
}

/// No prompts, as a value a borrow can outlive.
static NO_PROMPTS: Value = Value::Null;

impl<'a> Surface<'a> {
    /// A tools-only server.
    #[must_use]
    pub fn new(tools: &'a Value) -> Self {
        Self {
            tools,
            prompts: &NO_PROMPTS,
        }
    }

    /// The same server, also offering prompts.
    #[must_use]
    pub fn with_prompts(self, prompts: &'a Value) -> Self {
        Self { prompts, ..self }
    }

    /// The prompt entries, or nothing when this server declares none.
    fn prompt_entries(&self) -> &[Value] {
        self.prompts.as_array().map_or(&[], Vec::as_slice)
    }
}

/// What one inbound frame turned out to be.
///
/// Not `#[non_exhaustive]`: these three are the complete triage of a JSON-RPC
/// frame, and a fourth would be a change every host must answer, not absorb.
#[derive(Debug)]
pub enum Routed {
    /// Write this frame back and move on.
    Respond {
        /// The frame to write.
        response: Value,
        /// Who connected — `Some` on `initialize` and nowhere else. A host
        /// that attributes work to the client reads it here rather than
        /// re-parsing the line, and one that does not ignores it.
        client: Option<ClientInfo>,
    },
    /// A `tools/call` for the host to execute. The response is built from
    /// [`result`], [`text`], [`image_png`] or [`tool_error`].
    Call {
        /// The JSON-RPC id to answer with.
        id: Value,
        /// The tool the client asked for.
        name: String,
        /// The tool's arguments object.
        arguments: Value,
    },
    /// A notification: no id, no answer. `initialized` is the only one that
    /// reaches a tools-only server, and it wants silence, not an ack.
    Silent,
}

/// Triage one line of the stdio transport: answer every method that reads only
/// the [`Surface`], and hand `tools/call` back to the host.
///
/// A blank line is [`Routed::Silent`]; a line that is not JSON comes back as
/// the JSON-RPC parse error to write, so a garbled frame is answered rather
/// than fatal. The `initialize` response carries the client's own
/// [`ClientInfo`] alongside it, since that handshake is the only place a client
/// names itself.
#[must_use]
pub fn route(line: &str, info: ServerInfo, surface: &Surface) -> Routed {
    if line.trim().is_empty() {
        return Routed::Silent;
    }
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(failure) => return respond(error(&Value::Null, -32700, &failure.to_string())),
    };
    let Some(id) = request.get("id").cloned() else {
        return Routed::Silent;
    };
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    match method {
        "initialize" => {
            let mut capabilities = json!({ "tools": {} });
            if !surface.prompt_entries().is_empty() {
                capabilities["prompts"] = json!({});
            }
            Routed::Respond {
                response: ok(
                    &id,
                    &json!({
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": capabilities,
                        "serverInfo": { "name": info.name, "version": info.version },
                    }),
                ),
                client: client_info(&params),
            }
        }
        "ping" => respond(ok(&id, &json!({}))),
        "tools/list" => respond(ok(&id, &json!({ "tools": surface.tools }))),
        // Both prompt methods fall through to `-32601` on a server that
        // declares no prompts, which is what "no capability" has to mean.
        "prompts/list" if !surface.prompt_entries().is_empty() => {
            let listed: Vec<Value> = surface
                .prompt_entries()
                .iter()
                .map(|entry| json!({ "name": entry["name"], "description": entry["description"] }))
                .collect();
            respond(ok(&id, &json!({ "prompts": listed })))
        }
        "prompts/get" if !surface.prompt_entries().is_empty() => {
            let wanted = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match surface
                .prompt_entries()
                .iter()
                .find(|entry| entry["name"] == wanted)
            {
                Some(entry) => respond(ok(
                    &id,
                    &json!({ "description": entry["description"], "messages": entry["messages"] }),
                )),
                None => respond(error(&id, -32602, &format!("unknown prompt: {wanted}"))),
            }
        }
        "tools/call" => Routed::Call {
            id,
            name: params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            arguments: params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({})),
        },
        other => respond(error(&id, -32601, &format!("unknown method: {other}"))),
    }
}

/// A frame to write back, from a method that names no client.
fn respond(response: Value) -> Routed {
    Routed::Respond {
        response,
        client: None,
    }
}

/// The `clientInfo` of an `initialize` request. A client that sends none, or
/// sends a nameless one, gets `None` — a host may not invent an identity for it.
fn client_info(params: &Value) -> Option<ClientInfo> {
    let info = params.get("clientInfo")?;
    let name = info.get("name").and_then(Value::as_str)?;
    if name.is_empty() {
        return None;
    }
    Some(ClientInfo {
        name: name.to_owned(),
        version: info
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

/// One entry of `tools/list`.
#[must_use]
pub fn tool(name: &str, description: &str, input_schema: &Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": input_schema })
}

/// One entry of a [`Surface`]'s prompts.
///
/// Carries the body `prompts/get` returns as well as the two fields
/// `prompts/list` shows, so a host builds each prompt exactly once and neither
/// method can answer from a different copy. Prompts here take no arguments: a
/// skill is a document, not a template.
#[must_use]
pub fn prompt(name: &str, description: &str, body: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "messages": [{ "role": "user", "content": { "type": "text", "text": body } }],
    })
}

/// A successful JSON-RPC response.
#[must_use]
pub fn ok(id: &Value, result: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC error response. Reserve this for *protocol* failures: a tool that
/// failed is a result the model must be able to read, not a transport error.
#[must_use]
pub fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A text content block.
#[must_use]
pub fn text(body: impl Into<String>) -> Value {
    json!({ "type": "text", "text": body.into() })
}

/// An image content block carrying base64 PNG bytes.
#[must_use]
pub fn image_png(base64: impl Into<String>) -> Value {
    json!({ "type": "image", "data": base64.into(), "mimeType": "image/png" })
}

/// A `tools/call` result.
#[must_use]
pub fn result(content: &[Value], is_error: bool) -> Value {
    json!({ "content": content, "isError": is_error })
}

/// A `tools/call` result the model should read as a failure.
#[must_use]
pub fn tool_error(message: impl Into<String>) -> Value {
    result(&[text(message)], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: ServerInfo = ServerInfo {
        name: "test",
        version: "0",
    };

    /// A tools-only server, which is what most of these cases are about.
    fn bare(line: &str) -> Routed {
        let tools = json!([]);
        route(line, INFO, &Surface::new(&tools))
    }

    #[test]
    fn a_notification_is_silent() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(matches!(bare(line), Routed::Silent));
    }

    #[test]
    fn a_garbled_line_is_answered_not_fatal() {
        let Routed::Respond { response, .. } = bare("{not json") else {
            panic!("expected a response");
        };
        assert_eq!(response["error"]["code"], -32700);
        assert_eq!(response["id"], Value::Null);
    }

    #[test]
    fn a_call_carries_its_arguments() {
        let line =
            r#"{"id":7,"method":"tools/call","params":{"name":"python","arguments":{"code":"1"}}}"#;
        let Routed::Call {
            id,
            name,
            arguments,
        } = bare(line)
        else {
            panic!("expected a call");
        };
        assert_eq!(id, json!(7));
        assert_eq!(name, "python");
        assert_eq!(arguments["code"], "1");
    }

    /// The handshake is the one frame that names the client, and it must reach
    /// the host beside the answer rather than only in the crate's response.
    #[test]
    fn initialize_hands_the_client_back() {
        let line = r#"{"id":1,"method":"initialize","params":{"clientInfo":{"name":"claude-code","version":"2.1"}}}"#;
        let Routed::Respond { response, client } = bare(line) else {
            panic!("expected a response");
        };
        assert_eq!(response["result"]["serverInfo"]["name"], "test");
        assert_eq!(
            client,
            Some(ClientInfo {
                name: "claude-code".into(),
                version: "2.1".into()
            })
        );

        let anonymous = r#"{"id":1,"method":"initialize","params":{}}"#;
        let Routed::Respond { client, .. } = bare(anonymous) else {
            panic!("expected a response");
        };
        assert_eq!(client, None, "a nameless client is not given one");

        let pinged = r#"{"id":1,"method":"ping"}"#;
        let Routed::Respond { client, .. } = bare(pinged) else {
            panic!("expected a response");
        };
        assert_eq!(client, None, "only the handshake names a client");
    }

    /// Prompts are a *declared* capability: a server with none is exactly the
    /// tools-only server it was before this method existed.
    #[test]
    fn a_server_without_prompts_does_not_pretend_to_have_them() {
        let Routed::Respond { response, .. } = bare(r#"{"id":1,"method":"initialize"}"#) else {
            panic!("expected a response");
        };
        assert_eq!(response["result"]["capabilities"]["prompts"], Value::Null);
        let Routed::Respond { response, .. } = bare(r#"{"id":2,"method":"prompts/list"}"#) else {
            panic!("expected a response");
        };
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn prompts_are_listed_without_their_bodies_and_fetched_with_them() {
        let tools = json!([]);
        let prompts = json!([prompt("color", "Palettes.", "# Color\n\nUse fewer hues.")]);
        let surface = Surface::new(&tools).with_prompts(&prompts);

        let Routed::Respond { response, .. } =
            route(r#"{"id":1,"method":"initialize"}"#, INFO, &surface)
        else {
            panic!("expected a response");
        };
        assert_eq!(response["result"]["capabilities"]["prompts"], json!({}));

        let Routed::Respond { response, .. } =
            route(r#"{"id":2,"method":"prompts/list"}"#, INFO, &surface)
        else {
            panic!("expected a response");
        };
        assert_eq!(
            response["result"]["prompts"],
            json!([{ "name": "color", "description": "Palettes." }]),
            "the listing carries no body"
        );

        let line = r#"{"id":3,"method":"prompts/get","params":{"name":"color"}}"#;
        let Routed::Respond { response, .. } = route(line, INFO, &surface) else {
            panic!("expected a response");
        };
        assert_eq!(
            response["result"]["messages"][0]["content"]["text"],
            "# Color\n\nUse fewer hues."
        );

        let missing = r#"{"id":4,"method":"prompts/get","params":{"name":"nope"}}"#;
        let Routed::Respond { response, .. } = route(missing, INFO, &surface) else {
            panic!("expected a response");
        };
        assert_eq!(response["error"]["code"], -32602);
    }

    #[test]
    fn an_unknown_method_is_a_protocol_error() {
        let line = r#"{"id":1,"method":"resources/list"}"#;
        let Routed::Respond { response, .. } = bare(line) else {
            panic!("expected a response");
        };
        assert_eq!(response["error"]["code"], -32601);
    }
}
