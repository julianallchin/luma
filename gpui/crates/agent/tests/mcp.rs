//! The stdio protocol, end to end over a pipe made of `Vec<u8>`.
//!
//! The transport is small enough to be written by hand, which means it is also
//! small enough to have no excuse for being untested: an MCP client that gets
//! a malformed `initialize` back never reaches the interesting part.

use std::sync::Arc;
use std::time::Duration;

use gpui::{div, prelude::*, AnyView, App, Context, Render, Window};
use gpui_agent::{mcp, Config, Harness};
use luma_ui::node::{Instrument, Role};
use serde_json::{json, Value};

struct Ping;

impl Render for Ping {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child("ping").agent_node(Role::Text, "ping")
    }
}

fn harness() -> Harness {
    let root: gpui_agent::RootFactory =
        Arc::new(|_: &mut Window, cx: &mut App| -> AnyView { cx.new(|_| Ping).into() });
    Harness::headless(
        Config {
            call_timeout: Duration::from_secs(10),
            ..Config::default()
        },
        root,
    )
    .unwrap()
}

/// Feed `requests` in as newline-delimited JSON and collect the responses.
fn exchange(requests: &[Value]) -> Vec<Value> {
    let input: String = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect();
    let mut output = Vec::new();
    mcp::serve(&mut harness(), input.as_bytes(), &mut output).unwrap();
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn tool_payload(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

#[test]
fn a_client_can_initialize_and_list_exactly_two_tools() {
    let out = exchange(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ]);

    // The notification produced no response, so there are two, not three.
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["id"], 1);
    assert_eq!(out[0]["result"]["serverInfo"]["name"], "gpui-agent");
    assert!(out[0]["result"]["capabilities"]["tools"].is_object());

    let names: Vec<&str> = out[1]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["exec", "reset"]);
}

#[test]
fn exec_returns_the_result_stdout_and_frame() {
    let out = exchange(&[json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "exec",
            "arguments": { "code": "console.log('hi'); app.snapshot().nodes.length" },
        },
    })]);

    assert_eq!(out[0]["result"]["isError"], false);
    let payload = tool_payload(&out[0]);
    assert_eq!(payload["result"], 1);
    assert_eq!(payload["stdout"], "hi");
    assert!(payload["frame"].as_u64().unwrap() > 0);
    assert!(payload.get("error").is_none());
}

#[test]
fn a_thrown_script_comes_back_as_a_tool_error_with_its_output_intact() {
    let out = exchange(&[json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": {
            "name": "exec",
            "arguments": { "code": "console.log('before'); throw new Error('nope')" },
        },
    })]);

    assert_eq!(out[0]["result"]["isError"], true);
    let payload = tool_payload(&out[0]);
    assert_eq!(payload["stdout"], "before");
    assert!(payload["error"].as_str().unwrap().contains("nope"));
}

#[test]
fn reset_and_the_scratchpad_agree_over_the_wire() {
    let out = exchange(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "exec", "arguments": {"code": "globalThis.kept = 1"}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
               "params": {"name": "reset", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "exec", "arguments": {"code": "typeof kept"}}}),
    ]);

    assert_eq!(out[1]["result"]["isError"], false);
    assert_eq!(tool_payload(&out[2])["result"], "undefined");
}

#[test]
fn malformed_input_is_reported_rather_than_dropped() {
    let mut output = Vec::new();
    mcp::serve(&mut harness(), "{not json}\n".as_bytes(), &mut output).unwrap();
    let response: Value = serde_json::from_str(String::from_utf8(output).unwrap().trim()).unwrap();
    assert_eq!(response["error"]["code"], -32700);

    let out = exchange(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "nope", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "surprise"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "exec", "arguments": {}}}),
    ]);
    assert_eq!(out[0]["result"]["isError"], true);
    assert_eq!(out[1]["error"]["code"], -32601);
    assert!(tool_payload(&out[2])["error"]
        .as_str()
        .unwrap()
        .contains("`code` is required"));
}
