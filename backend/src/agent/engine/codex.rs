use super::{
    content,
    process::{command, Process},
    protocol, AgentError, ContentBlock, Event, Request, ToolOutcome, Usage,
};
use serde_json::{json, Value};

pub(in crate::agent) struct Session {
    process: Process,
    request: Request,
    usage: Usage,
}

impl Session {
    pub async fn start(request: Request) -> Result<Self, AgentError> {
        let process = start_process(&request.cwd)?;
        Self::connect(request, process).await
    }

    async fn connect(request: Request, mut process: Process) -> Result<Self, AgentError> {
        initialize(&mut process).await?;
        let usage = request
            .resume
            .as_ref()
            .map_or(Usage::default(), |s| s.usage);
        Ok(Self {
            process,
            request,
            usage,
        })
    }

    pub async fn next(&mut self) -> Result<Event, AgentError> {
        loop {
            let frame = self.process.read().await?;
            if let Some(error) = frame.get("error") {
                return Err(protocol(format!("Codex: {error}")));
            }
            match frame.get("id").and_then(Value::as_u64) {
                Some(1) if frame.get("method").is_none() => {
                    self.process.send(json!({"method":"initialized"})).await?;
                    self.process
                        .send(json!({"id":4,"method":"account/read","params":{}}))
                        .await?;
                }
                Some(4) if frame.get("method").is_none() => {
                    if frame
                        .pointer("/result/account/type")
                        .and_then(Value::as_str)
                        != Some("chatgpt")
                    {
                        return Err(protocol("Codex subscription execution requires `codex login` with ChatGPT on this machine"));
                    }
                    self.process
                        .send(json!({"id":5,"method":"config/read","params":{}}))
                        .await?;
                }
                Some(5) if frame.get("method").is_none() => {
                    let mut config = isolated_config();
                    if let Some(servers) = frame
                        .pointer("/result/config/mcp_servers")
                        .and_then(Value::as_object)
                    {
                        let mut servers = servers.clone();
                        for server in servers.values_mut() {
                            if let Some(fields) = server.as_object_mut() {
                                fields.retain(|_, value| !value.is_null());
                                fields.insert("enabled".into(), json!(false));
                            }
                        }
                        config["mcp_servers"] = Value::Object(servers);
                    }
                    let mut params = json!({
                        "cwd":self.request.cwd,"model":self.request.model,
                        "developerInstructions":self.request.system,"dynamicTools":super::tool_definitions(&self.request.tools),
                        "approvalPolicy":"untrusted","sandbox":"read-only",
                        "config":config
                    });
                    let method = if let Some(id) = &self.request.resume {
                        params["threadId"] = json!(id.id);
                        "thread/resume"
                    } else {
                        "thread/start"
                    };
                    self.process
                        .send(json!({"id":2,"method":method,"params":params}))
                        .await?;
                }
                Some(2) if frame.get("method").is_none() => {
                    let thread = frame
                        .pointer("/result/thread/id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| protocol("Codex did not return a thread id"))?
                        .to_string();
                    self.process
                        .send(json!({"id":3,"method":"turn/start","params":{
                            "threadId":thread,"effort":self.request.effort,"input":[{"type":"text","text":self.request.prompt}]
                        }}))
                        .await?;
                    return Ok(Event::Session {
                        id: thread,
                        model: frame
                            .pointer("/result/model")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                }
                _ => {}
            }
            let params = &frame["params"];
            match frame["method"].as_str().unwrap_or("") {
                "item/agentMessage/delta" => return Ok(Event::Text(text(params, "delta")?)),
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    return Ok(Event::Reasoning(text(params, "delta")?))
                }
                "item/tool/call" => {
                    return Ok(Event::Tool {
                        id: text(params, "callId")?,
                        name: text(params, "tool")?,
                        input: params["arguments"].clone(),
                        reply: frame["id"].clone(),
                    });
                }
                "thread/tokenUsage/updated" => {
                    let usage = &params["tokenUsage"]["total"];
                    let cached = count(usage, "cachedInputTokens");
                    let total = Usage {
                        input_tokens: count(usage, "inputTokens").saturating_sub(cached),
                        output_tokens: count(usage, "outputTokens"),
                        cache_read_input_tokens: cached,
                        ..Usage::default()
                    };
                    let delta = Usage {
                        input_tokens: total.input_tokens.saturating_sub(self.usage.input_tokens),
                        output_tokens: total.output_tokens.saturating_sub(self.usage.output_tokens),
                        cache_read_input_tokens: total
                            .cache_read_input_tokens
                            .saturating_sub(self.usage.cache_read_input_tokens),
                        ..Usage::default()
                    };
                    self.usage = total;
                    return Ok(Event::Usage(delta));
                }
                "turn/completed" => {
                    if params.pointer("/turn/status").and_then(Value::as_str) == Some("completed") {
                        return Ok(Event::Done);
                    }
                    return Err(protocol(format!(
                        "Codex turn did not complete: {}",
                        params["turn"]
                    )));
                }
                "error" => return Err(protocol(format!("Codex: {}", params["error"]))),
                _ if frame.get("method").is_some() && frame.get("id").is_some() => {
                    // No native shell, filesystem, network or user-input approval
                    // is silently granted by a lighting agent.
                    self.process.send(json!({"id":frame["id"],"error":{"code":-32601,"message":"Only Luma tools are available in this session"}})).await?;
                }
                _ => {}
            }
        }
    }

    pub fn usage_total(&self) -> Usage {
        self.usage
    }

    pub async fn reply(&mut self, id: Value, outcome: ToolOutcome) -> Result<(), AgentError> {
        let (blocks, failed) = content(outcome);
        let items: Vec<_> = blocks.into_iter().filter_map(|b| match b {
            ContentBlock::Text(text) => Some(json!({"type":"inputText","text":text})),
            ContentBlock::Image {media_type,data} => Some(json!({"type":"inputImage","imageUrl":format!("data:{media_type};base64,{data}")})),
            _ => None,
        }).collect();
        self.process
            .send(json!({"id":id,"result":{"contentItems":items,"success":!failed}}))
            .await
    }
}

fn isolated_config() -> Value {
    json!({
        "features.shell_tool":false, "features.hooks":false,
        "features.multi_agent":false, "agents.enabled":false,
        "features.apps":false, "features.plugins":false,
        "features.browser_use":false, "features.computer_use":false,
        "features.image_generation":false, "features.view_image":false,
        "features.code_mode":false, "features.skip_host_skill_discovery":true,
        "features.remote_control":false, "web_search":"disabled",
        "model_provider":"openai", "forced_login_method":"chatgpt"
    })
}

fn text(value: &Value, key: &str) -> Result<String, AgentError> {
    value[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| protocol(format!("Codex frame is missing {key}")))
}
fn count(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn tools_images_usage_and_completion() {
        let temp = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys
read=lambda: json.loads(sys.stdin.readline())
def send(x): print(json.dumps(x),flush=True)
assert read()['method']=='initialize'
send({'id':1,'result':{}})
assert read()['method']=='initialized'
assert read()['method']=='account/read'
send({'id':4,'result':{'account':{'type':'chatgpt'}}})
assert read()['method']=='config/read'
send({'id':5,'result':{'config':{'mcp_servers':{'external':{}}}}})
start=read()
assert start['params']['config']['mcp_servers']['external']['enabled'] == False
assert start['params']['dynamicTools'][0]['name']=='echo'
assert start['params']['sandbox']=='read-only'
send({'id':2,'result':{'thread':{'id':'native'}}})
turn=read()
assert turn['method']=='turn/start'
assert turn['params']['effort']=='high'
send({'id':3,'result':{}})
send({'id':'call','method':'item/tool/call','params':{'callId':'tool-1','tool':'echo','arguments':{'value':'hi'}}})
reply=read()
assert reply['id']=='call'
assert reply['result']['success']
assert reply['result']['contentItems'][0]['type']=='inputImage'
send({'method':'thread/tokenUsage/updated','params':{'tokenUsage':{'total':{'inputTokens':100,'cachedInputTokens':40,'outputTokens':10}}}})
send({'method':'item/agentMessage/delta','params':{'delta':'done'}})
send({'method':'turn/completed','params':{'turn':{'status':'completed'}}})
"#;
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", script]);
        let mut request = super::super::tests::request(super::super::Engine::Codex, temp.path());
        request.effort = Some("high".into());
        let mut session = Session::connect(request, Process::start(command).unwrap())
            .await
            .unwrap();
        assert!(matches!(session.next().await.unwrap(), Event::Session {id,..} if id == "native"));
        let Event::Tool {
            id,
            name,
            input,
            reply,
        } = session.next().await.unwrap()
        else {
            panic!("tool")
        };
        assert_eq!((id.as_str(), name.as_str()), ("tool-1", "echo"));
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
        let Event::Usage(usage) = session.next().await.unwrap() else {
            panic!("usage")
        };
        assert_eq!(
            (
                usage.input_tokens,
                usage.cache_read_input_tokens,
                usage.output_tokens
            ),
            (60, 40, 10)
        );
        assert!(matches!(session.next().await.unwrap(),Event::Text(text) if text == "done"));
        assert!(matches!(session.next().await.unwrap(), Event::Done));
    }
}

fn start_process(cwd: &std::path::Path) -> Result<Process, AgentError> {
    let mut cmd = command("codex", cwd);
    cmd.args(["app-server", "--stdio"]);
    for (key, value) in isolated_config().as_object().expect("config object") {
        cmd.arg("-c").arg(format!("{key}={value}"));
    }
    Process::start(cmd)
}

async fn initialize(process: &mut Process) -> Result<(), AgentError> {
    process
        .send(json!({"id":1,"method":"initialize","params":{
            "clientInfo":{"name":"luma","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true}
        }}))
        .await
}

pub(super) async fn models(
    cwd: &std::path::Path,
) -> Result<Vec<super::catalog::ModelChoice>, AgentError> {
    let mut process = start_process(cwd)?;
    initialize(&mut process).await?;
    let mut models = vec![super::catalog::ModelChoice {
        id: None,
        label: "Default".into(),
        resolved_model: None,
        effort_levels: Vec::new(),
    }];
    loop {
        let frame = process.read().await?;
        if let Some(error) = frame.get("error") {
            return Err(protocol(format!("Codex: {error}")));
        }
        match frame.get("id").and_then(Value::as_u64) {
            Some(1) => {
                process.send(json!({"method":"initialized"})).await?;
                process
                    .send(json!({"id":2,"method":"model/list","params":{}}))
                    .await?;
            }
            Some(2) => {
                let page = frame
                    .pointer("/result/data")
                    .and_then(Value::as_array)
                    .ok_or_else(|| protocol("Codex did not return its model catalog"))?;
                for model in page {
                    models.push(super::catalog::ModelChoice {
                        id: Some(text(model, "model")?),
                        label: text(model, "displayName")?,
                        resolved_model: Some(text(model, "model")?),
                        effort_levels: model["supportedReasoningEfforts"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|value| value["reasoningEffort"].as_str())
                            .map(str::to_string)
                            .collect(),
                    });
                }
                if let Some(default) = page.iter().find(|model| model["isDefault"] == true) {
                    if let Some(choice) = models
                        .iter()
                        .find(|choice| choice.id.as_deref() == default["model"].as_str())
                        .cloned()
                    {
                        models[0] = super::catalog::ModelChoice {
                            id: None,
                            label: format!("Default · {}", choice.label),
                            ..choice
                        };
                    }
                }
                if let Some(cursor) = frame.pointer("/result/nextCursor").and_then(Value::as_str) {
                    process
                        .send(json!({"id":2,"method":"model/list","params":{"cursor":cursor}}))
                        .await?;
                } else {
                    return Ok(models);
                }
            }
            _ => {}
        }
    }
}
