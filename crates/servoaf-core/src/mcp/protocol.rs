// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! JSON-RPC 2.0 framing and the MCP methods servoaf implements.

use std::rc::Rc;
use std::time::Duration;

use serde_json::{Value, json};

use super::registry::{Args, ToolRegistry, ToolResult};
use crate::runtime::timeout;

pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[LATEST_PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

pub const SERVER_NAME: &str = "servoaf";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

const INSTRUCTIONS: &str = "Controls a running servoaf browser (Servo engine). Coordinates are CSS \
pixels relative to the top-left of the tab's viewport. Use get-clickables to find targets and \
their centers, then move-cursor/click. Tools act on the active tab unless tab_id is given (see \
get-tabs). Page dialogs (alert/confirm/prompt, <select> popups, context menus, file pickers) are \
listed by get-dialogs and answered with respond-dialog.";

pub fn result_message(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub fn error_message(id: &Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// Whether `body` is an `initialize` request (the HTTP layer then issues an
/// `Mcp-Session-Id`).
pub fn is_initialize_request(body: &str) -> bool {
    serde_json::from_str::<Value>(body).is_ok_and(|message| {
        message.get("method").and_then(Value::as_str) == Some("initialize") && message.get("id").is_some()
    })
}

pub struct McpProtocol<C> {
    registry: Rc<ToolRegistry<C>>,
    tool_timeout: Duration,
}

impl<C: 'static> McpProtocol<C> {
    pub fn new(registry: Rc<ToolRegistry<C>>, tool_timeout: Duration) -> Self {
        Self { registry, tool_timeout }
    }

    /// Handles one HTTP POST body. `None` means "no response" (HTTP 202), as for
    /// notifications.
    pub async fn handle_message(&self, context: Rc<C>, body: &str) -> Option<String> {
        let Ok(parsed) = serde_json::from_str::<Value>(body) else {
            return Some(error_message(&Value::Null, PARSE_ERROR, "Parse error"));
        };
        let Some(message) = parsed.as_object() else {
            // JSON-RPC batches were removed from MCP in 2025-06-18.
            return Some(error_message(&Value::Null, INVALID_REQUEST, "Expected a single JSON-RPC object"));
        };
        let id = message.get("id");
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response to a server->client request (we never send any) or junk.
            if id.is_some() && (message.contains_key("result") || message.contains_key("error")) {
                return None;
            }
            return Some(error_message(id.unwrap_or(&Value::Null), INVALID_REQUEST, "Missing method"));
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_message(
                id.unwrap_or(&Value::Null),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        // Notifications (notifications/initialized, notifications/cancelled, ...)
        // need no response.
        let id = id?.clone();
        if !(id.is_string() || id.is_number()) {
            return Some(error_message(&Value::Null, INVALID_REQUEST, "Invalid id"));
        }
        let params = match message.get("params") {
            None => None,
            Some(Value::Object(params)) => Some(params),
            Some(_) => return Some(error_message(&id, INVALID_PARAMS, "params must be an object")),
        };
        Some(self.handle_request(context, &id, method, params).await)
    }

    async fn handle_request(
        &self,
        context: Rc<C>,
        id: &Value,
        method: &str,
        params: Option<&Args>,
    ) -> String {
        match method {
            "initialize" => {
                let requested =
                    params.and_then(|params| params.get("protocolVersion")).and_then(Value::as_str);
                let version = requested
                    .filter(|requested| SUPPORTED_PROTOCOL_VERSIONS.contains(requested))
                    .unwrap_or(LATEST_PROTOCOL_VERSION);
                result_message(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": SERVER_NAME, "title": "servoaf browser", "version": SERVER_VERSION},
                        "instructions": INSTRUCTIONS,
                    }),
                )
            },
            "ping" => result_message(id, json!({})),
            "tools/list" => result_message(id, json!({"tools": self.registry.describe()})),
            "tools/call" => self.handle_tools_call(context, id, params).await,
            // Optional MCP features we don't provide; answer with empty lists so
            // generic clients don't treat the server as broken.
            "resources/list" => result_message(id, json!({"resources": []})),
            "resources/templates/list" => result_message(id, json!({"resourceTemplates": []})),
            "prompts/list" => result_message(id, json!({"prompts": []})),
            _ => error_message(id, METHOD_NOT_FOUND, &format!("Method not found: {method}")),
        }
    }

    async fn handle_tools_call(&self, context: Rc<C>, id: &Value, params: Option<&Args>) -> String {
        let Some(name) = params.and_then(|params| params.get("name")).and_then(Value::as_str) else {
            return error_message(id, INVALID_PARAMS, "Missing tool name");
        };
        let Some(tool) = self.registry.find(name) else {
            return error_message(id, INVALID_PARAMS, &format!("Unknown tool: {name}"));
        };
        let arguments = match params.and_then(|params| params.get("arguments")) {
            None | Some(Value::Null) => Args::new(),
            Some(Value::Object(arguments)) => arguments.clone(),
            Some(_) => return error_message(id, INVALID_PARAMS, "arguments must be an object"),
        };
        if let Some(required) = tool.input_schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !arguments.contains_key(key) {
                    let result = ToolResult::error(format!("Missing required argument: {key}"));
                    return result_message(id, result.into_value());
                }
            }
        }
        let result = match timeout(self.tool_timeout, (tool.handler)(context, arguments)).await {
            Ok(result) => result,
            Err(_) => {
                ToolResult::error(format!("Tool call timed out after {}s", self.tool_timeout.as_secs()))
            },
        };
        result_message(id, result.into_value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::registry::{SchemaBuilder, string_arg};
    use crate::runtime::{Runtime, sleep};

    fn protocol() -> Rc<McpProtocol<()>> {
        let mut registry = ToolRegistry::<()>::default();
        registry.register(
            "echo",
            "Echoes text.",
            SchemaBuilder::new().required_string("text", "Text").build(),
            |_, args| {
                let text = string_arg(&args, "text").unwrap_or_default().to_owned();
                async move { ToolResult::text(text) }
            },
        );
        registry.register("slow", "Never finishes in time.", SchemaBuilder::new().build(), |_, _| async {
            sleep(Duration::from_secs(10)).await;
            ToolResult::text("late")
        });
        Rc::new(McpProtocol::new(Rc::new(registry), Duration::from_millis(50)))
    }

    fn call(body: &str) -> Option<Value> {
        let protocol = protocol();
        let body = body.to_owned();
        Runtime::default()
            .block_on(async move { protocol.handle_message(Rc::new(()), &body).await })
            .map(|reply| serde_json::from_str(&reply).unwrap())
    }

    #[test]
    fn initialize_negotiates_version() {
        let reply = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], "servoaf");
        let reply = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01"}}"#,
        )
        .unwrap();
        assert_eq!(reply["result"]["protocolVersion"], LATEST_PROTOCOL_VERSION);
        assert!(is_initialize_request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#));
        assert!(!is_initialize_request(r#"{"jsonrpc":"2.0","method":"initialize"}"#));
    }

    #[test]
    fn errors_and_notifications() {
        assert_eq!(call("{nope").unwrap()["error"]["code"], PARSE_ERROR);
        assert_eq!(call("[]").unwrap()["error"]["code"], INVALID_REQUEST);
        assert_eq!(
            call(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#).unwrap()["error"]["code"],
            INVALID_REQUEST
        );
        assert_eq!(
            call(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#).unwrap()["error"]["code"],
            METHOD_NOT_FOUND
        );
        assert!(call(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(call(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#).is_none());
        assert_eq!(call(r#"{"jsonrpc":"2.0","id":"a","method":"ping"}"#).unwrap()["id"], "a");
    }

    #[test]
    fn tools() {
        let list = call(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], "echo");
        assert_eq!(list["result"]["tools"][0]["inputSchema"]["required"][0], "text");

        let reply = call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{"text":"hi"}}}"#).unwrap();
        assert_eq!(reply["result"]["content"][0]["text"], "hi");
        assert_eq!(reply["result"]["isError"], false);

        let reply =
            call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"echo","arguments":{}}}"#)
                .unwrap();
        assert_eq!(reply["result"]["isError"], true);

        let reply =
            call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"zzz"}}"#).unwrap();
        assert_eq!(reply["error"]["code"], INVALID_PARAMS);

        let reply =
            call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"slow"}}"#).unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("timed out"));
    }
}
