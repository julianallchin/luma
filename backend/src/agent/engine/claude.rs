//! Claude's stream-json control protocol, including SDK MCP requests. The
//! unmodified CLI owns authentication; Luma owns every advertised tool.

use super::{
    content,
    process::{command, Process},
    protocol, AgentError, ContentBlock, Event, Request, ToolOutcome, Usage,
};
use serde_json::{json, Value};

pub(in crate::agent) struct Session {
    process: Process,
    request: Request,
    completed: bool,
}

impl Session {
    pub async fn start(request: Request) -> Result<Self, AgentError> {
        let mut auth = claude_command(&request.cwd);
        auth.args(["auth", "status", "--json"]).kill_on_drop(true);
        let status = tokio::time::timeout(std::time::Duration::from_secs(15), auth.output())
            .await
            .map_err(|_| protocol("Claude authentication check timed out"))?
            .map_err(|e| protocol(format!("could not check Claude authentication: {e}")))?;
        let status: Value = serde_json::from_slice(&status.stdout)
            .map_err(|e| protocol(format!("invalid Claude authentication status: {e}")))?;
        if status["loggedIn"] != true || status["authMethod"] != "claude.ai" {
            return Err(protocol("A new Claude CLI process reports no Claude.ai login. An already-open terminal session may still be signed in. Run `claude auth login` in a fresh terminal, then retry."));
        }
        let mut cmd = stream_command(&request.cwd);
        cmd.arg("--system-prompt").arg(&request.system);
        cmd.arg("--mcp-config")
            .arg(json!({"mcpServers":{"luma":{"type":"sdk","name":"luma"}}}).to_string());
        if let Some(effort) = &request.effort {
            cmd.arg("--effort").arg(effort);
        }
        if let Some(model) = &request.model {
            cmd.arg("--model").arg(model);
        }
        if let Some(resume) = &request.resume {
            cmd.arg("--resume").arg(&resume.id);
        }
        Self::connect(request, Process::start(cmd)?).await
    }

    async fn connect(request: Request, mut process: Process) -> Result<Self, AgentError> {
        initialize(&mut process).await?;
        Ok(Self {
            process,
            request,
            completed: false,
        })
    }

    pub async fn next(&mut self) -> Result<Event, AgentError> {
        if self.completed {
            return Ok(Event::Done);
        }
        loop {
            let frame = self.process.read().await?;
            match frame["type"].as_str().unwrap_or("") {
                "control_response" if frame["response"]["request_id"] == "initialize" => {
                    if frame["response"]["subtype"] != "success" {
                        return Err(protocol(format!(
                            "Claude initialization: {}",
                            frame["response"]
                        )));
                    }
                    self.process.send(json!({"type":"user","message":{"role":"user","content":self.request.prompt}})).await?;
                }
                "control_request" => {
                    let request = &frame["request"];
                    let id = frame["request_id"].clone();
                    match request["subtype"].as_str().unwrap_or("") {
                        "mcp_message" if request["server_name"] == "luma" => {
                            let message = &request["message"];
                            let reply = json!({"control":id,"mcp":message["id"]});
                            match message["method"].as_str().unwrap_or("") {
                                "tools/call" => return Ok(Event::Tool {
                                    id: format!("claude-{}",id.as_str().unwrap_or("tool")),
                                    name: message["params"]["name"].as_str().ok_or_else(|| protocol("Claude tool has no name"))?.into(),
                                    input: message["params"]["arguments"].clone(), reply,
                                }),
                                "initialize" => self.mcp_reply(reply,json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"luma","version":env!("CARGO_PKG_VERSION")}})).await?,
                                "tools/list" => {
                                    self.mcp_reply(reply,json!({"tools":super::tool_definitions(&self.request.tools)})).await?;
                                }
                                "notifications/initialized" | "ping" => self.mcp_reply(reply,json!({})).await?,
                                method => return Err(protocol(format!("unsupported Claude MCP method: {method}"))),
                            }
                        }
                        "can_use_tool" => {
                            let allowed = request["tool_name"].as_str().is_some_and(|name| {
                                self.request
                                    .tools
                                    .iter()
                                    .any(|tool| name == format!("mcp__luma__{}", tool.name))
                            });
                            let response = if allowed {
                                json!({"behavior":"allow","updatedInput":request["input"]})
                            } else {
                                json!({"behavior":"deny","message":"Only Luma tools are available in this session"})
                            };
                            self.control_reply(id, response).await?;
                        }
                        subtype => {
                            return Err(protocol(format!(
                                "unsupported Claude control request: {subtype}"
                            )))
                        }
                    }
                }
                "system" if frame["subtype"] == "init" => {
                    return Ok(Event::Session {
                        id: frame["session_id"]
                            .as_str()
                            .ok_or_else(|| protocol("Claude returned no session id"))?
                            .into(),
                        model: frame["model"].as_str().map(str::to_string),
                    });
                }
                "stream_event" => {
                    let event = &frame["event"];
                    match event["delta"]["type"].as_str().unwrap_or("") {
                        "text_delta" => {
                            return Ok(Event::Text(
                                event["delta"]["text"].as_str().unwrap_or("").into(),
                            ))
                        }
                        "thinking_delta" => {
                            return Ok(Event::Reasoning(
                                event["delta"]["thinking"].as_str().unwrap_or("").into(),
                            ))
                        }
                        _ => {}
                    }
                }
                "assistant" => {
                    if frame.get("error").is_some() {
                        return Err(claude_error(&frame));
                    }
                }

                "result" => {
                    if frame["is_error"] == true {
                        return Err(claude_error(&frame));
                    }
                    self.completed = true;
                    let usage = &frame["usage"];
                    return Ok(Event::Usage(Usage {
                        input_tokens: count(usage, "input_tokens"),
                        output_tokens: count(usage, "output_tokens"),
                        cache_creation_input_tokens: count(usage, "cache_creation_input_tokens"),
                        cache_read_input_tokens: count(usage, "cache_read_input_tokens"),
                    }));
                }
                _ => {}
            }
        }
    }

    async fn control_reply(&mut self, id: Value, response: Value) -> Result<(), AgentError> {
        self.process.send(json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":response}})).await
    }

    async fn mcp_reply(&mut self, id: Value, result: Value) -> Result<(), AgentError> {
        self.control_reply(
            id["control"].clone(),
            json!({"mcp_response":{"jsonrpc":"2.0","id":id["mcp"],"result":result}}),
        )
        .await
    }

    pub async fn reply(&mut self, id: Value, outcome: ToolOutcome) -> Result<(), AgentError> {
        let (blocks, failed) = content(outcome);
        let blocks: Vec<_> = blocks
            .into_iter()
            .filter_map(|b| match b {
                ContentBlock::Text(text) => Some(json!({"type":"text","text":text})),
                ContentBlock::Image { media_type, data } => {
                    Some(json!({"type":"image","mimeType":media_type,"data":data}))
                }
                _ => None,
            })
            .collect();
        self.mcp_reply(id, json!({"content":blocks,"isError":failed}))
            .await
    }
}
fn stream_command(cwd: &std::path::Path) -> tokio::process::Command {
    let mut cmd = claude_command(cwd);
    cmd.args([
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--tools",
        "",
        "--strict-mcp-config",
        "--permission-prompt-tool",
        "stdio",
        "--setting-sources",
        "",
        "--max-turns",
        "64",
    ]);
    cmd
}

pub(super) async fn models(
    cwd: &std::path::Path,
) -> Result<Vec<super::catalog::ModelChoice>, AgentError> {
    let mut process = Process::start(stream_command(cwd))?;
    initialize(&mut process).await?;
    loop {
        let frame = process.read().await?;
        if frame["type"] == "control_response" && frame["response"]["request_id"] == "initialize" {
            let models = frame
                .pointer("/response/response/models")
                .and_then(Value::as_array)
                .ok_or_else(|| protocol("Claude did not return its model catalog"))?;
            return models
                .iter()
                .map(|model| {
                    Ok(super::catalog::ModelChoice {
                        id: model["value"]
                            .as_str()
                            .filter(|id| *id != "default")
                            .map(str::to_string),
                        label: {
                            let label = model["description"]
                                .as_str()
                                .and_then(|text| text.split('·').next())
                                .map(str::trim)
                                .filter(|text| !text.is_empty())
                                .or_else(|| model["displayName"].as_str())
                                .ok_or_else(|| protocol("Claude model has no name"))?;
                            if model["value"] == "default" {
                                format!("Default · {label}")
                            } else {
                                label.into()
                            }
                        },
                        resolved_model: model["resolvedModel"].as_str().map(str::to_string),
                        effort_levels: model["supportedEffortLevels"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect(),
                    })
                })
                .collect();
        }
    }
}

async fn initialize(process: &mut Process) -> Result<(), AgentError> {
    process.send(json!({"type":"control_request","request_id":"initialize","request":{"subtype":"initialize","hooks":null}})).await
}

fn claude_command(cwd: &std::path::Path) -> tokio::process::Command {
    let mut cmd = command("claude", cwd);
    for variable in [
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        cmd.env_remove(variable);
    }
    cmd
}

fn claude_error(frame: &Value) -> AgentError {
    let messages: Vec<_> = frame["errors"]
        .as_array()
        .or_else(|| frame.pointer("/message/content").and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().or_else(|| value["text"].as_str()))
        .filter(|text| !text.trim().is_empty())
        .collect();
    let message = if messages.is_empty() {
        ["result", "error", "subtype"]
            .into_iter()
            .filter_map(|key| frame[key].as_str())
            .find(|text| !text.trim().is_empty())
            .unwrap_or("unknown provider error")
            .to_owned()
    } else {
        messages.join("\n")
    };
    protocol(format!("Claude: {message}"))
}

fn count(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_errors_preserve_the_explanation() {
        for frame in [
            json!({"error":"invalid_request","message":{"content":[{"type":"text","text":"Request rejected: try a new session."}]}}),
            json!({"is_error":true,"errors":[],"result":"Request rejected: try a new session."}),
            json!({"is_error":true,"errors":["Request rejected: try a new session."]}),
        ] {
            assert!(claude_error(&frame)
                .to_string()
                .contains("Request rejected: try a new session."));
        }
        assert!(claude_error(&json!({"error":"invalid_request"}))
            .to_string()
            .contains("invalid_request"));
    }

    #[tokio::test]
    async fn scoped_mcp_round_trip_and_denied_native_tool() {
        let temp = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys
read=lambda: json.loads(sys.stdin.readline())
def send(x): print(json.dumps(x),flush=True)
def control(id,request): send({'type':'control_request','request_id':id,'request':request})
assert read()['request']['subtype']=='initialize'
control('list',{'subtype':'mcp_message','server_name':'luma','message':{'id':1,'method':'tools/list'}})
assert read()['response']['response']['mcp_response']['result']['tools'][0]['name']=='echo'
send({'type':'control_response','response':{'request_id':'initialize','subtype':'success','response':{}}})
assert read()['type']=='user'
send({'type':'system','subtype':'init','session_id':'native'})
control('deny',{'subtype':'can_use_tool','tool_name':'Bash','input':{'command':'touch forbidden'}})
assert read()['response']['response']['behavior']=='deny'
control('call',{'subtype':'mcp_message','server_name':'luma','message':{'id':2,'method':'tools/call','params':{'name':'echo','arguments':{'value':'hi'}}}})
reply=read()['response']['response']['mcp_response']
assert reply['id']==2
assert reply['result']['content'][0]['type']=='image'
send({'type':'stream_event','event':{'delta':{'type':'text_delta','text':'done'}}})
send({'type':'assistant','message':{'usage':{'input_tokens':10,'output_tokens':2}}})
send({'type':'result','is_error':False,'usage':{'input_tokens':10,'output_tokens':2}})
"#;
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", script]);
        let request = super::super::tests::request(super::super::Engine::Claude, temp.path());
        let mut session = Session::connect(request, Process::start(command).unwrap())
            .await
            .unwrap();
        assert!(matches!(session.next().await.unwrap(),Event::Session {id,..} if id == "native"));
        let Event::Tool {
            name, input, reply, ..
        } = session.next().await.unwrap()
        else {
            panic!("tool")
        };
        assert_eq!(name, "echo");
        assert_eq!(input, json!({"value":"hi"}));
        session
            .reply(
                reply,
                ToolOutcome::Content(vec![ContentBlock::Image {
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                }]),
            )
            .await
            .unwrap();
        assert!(matches!(session.next().await.unwrap(),Event::Text(text) if text=="done"));
        assert!(
            matches!(session.next().await.unwrap(),Event::Usage(usage) if usage.input_tokens==10)
        );
        assert!(matches!(session.next().await.unwrap(), Event::Done));
    }
}
