use super::{
    content,
    process::{command, Process},
    protocol, AgentError, ContentBlock, Event, Request, ToolOutcome, Usage,
};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

pub(in crate::agent) struct Session {
    process: Process,
    request: Request,
    usage: Usage,
    pub(super) last_usage: Option<Usage>,
    pub(super) context_window: Option<u64>,
    steers: Arc<Mutex<Steers>>,
    /// Events one frame produced beyond the one returned.
    ready: VecDeque<Event>,
    /// The turn completed while steers were still waiting for their answer.
    completing: bool,
}

// Adapted from Comet, MIT, (c) 2026 Wing: `turn/steer` with the turn-completed
// race falling back to the next `turn/start` on the same thread.
/// What the session and its [`Steerer`] share about steering.
#[derive(Default)]
struct Steers {
    thread: Option<String>,
    /// The turn a steer may join: set by `turn/start`'s answer, cleared at
    /// `turn/completed`.
    turn: Option<String>,
    /// `turn/steer` requests awaiting an answer, by request id.
    requests: HashMap<String, (String, String)>,
    /// Steers no running turn took, delivered as the next `turn/start`.
    queued: VecDeque<(String, String)>,
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
            last_usage: None,
            context_window: None,
            steers: Arc::default(),
            ready: VecDeque::new(),
            completing: false,
        })
    }

    pub(super) fn steerer(&self) -> Steerer {
        Steerer {
            input: self.process.input(),
            steers: Arc::clone(&self.steers),
        }
    }

    pub async fn next(&mut self) -> Result<Event, AgentError> {
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Ok(event);
            }
            let frame = self.process.read().await?;
            if frame.get("method").is_none() {
                if let Some(id) = frame["id"].as_str() {
                    if let Some(event) = self.answered(id, &frame).await? {
                        return Ok(event);
                    }
                    continue;
                }
            }
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
                    self.steers.lock().expect("steers").thread = Some(thread.clone());
                    return Ok(Event::Session {
                        id: thread,
                        model: frame
                            .pointer("/result/model")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    });
                }
                Some(3) if frame.get("method").is_none() => {
                    self.steers.lock().expect("steers").turn = frame
                        .pointer("/result/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    continue;
                }
                _ => {}
            }
            let params = &frame["params"];
            match frame["method"].as_str().unwrap_or("") {
                "item/agentMessage/delta" => return Ok(Event::Text(text(params, "delta")?)),
                "item/completed"
                    if params["item"]["type"] == "agentMessage"
                        && params["item"]["phase"] == "final_answer" =>
                {
                    return Ok(Event::FinalAnswer(text(&params["item"], "text")?));
                }
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
                    self.last_usage = params["tokenUsage"].get("last").map(|last| {
                        let cached = count(last, "cachedInputTokens");
                        Usage {
                            input_tokens: count(last, "inputTokens").saturating_sub(cached),
                            output_tokens: count(last, "outputTokens"),
                            cache_read_input_tokens: cached,
                            ..Usage::default()
                        }
                    });
                    self.context_window = params["tokenUsage"]["modelContextWindow"]
                        .as_u64()
                        .filter(|window| *window > 0);
                    if let Some(last) = self.last_usage {
                        self.ready.push_back(Event::Step(last));
                    }
                    return Ok(Event::Usage(delta));
                }
                "turn/completed" => {
                    if params.pointer("/turn/status").and_then(Value::as_str) == Some("completed") {
                        let waiting = {
                            let mut steers = self.steers.lock().expect("steers");
                            steers.turn = None;
                            !steers.requests.is_empty()
                        };
                        if waiting {
                            self.completing = true;
                            continue;
                        }
                        return self.finish_turn().await;
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

    /// The answer to a request this session made by string id: a steer, or a
    /// turn it started for one.
    async fn answered(&mut self, id: &str, frame: &Value) -> Result<Option<Event>, AgentError> {
        if id.starts_with(TURN_REQUEST) {
            if let Some(error) = frame.get("error") {
                return Err(protocol(format!("Codex: steering failed: {error}")));
            }
            self.steers.lock().expect("steers").turn = frame
                .pointer("/result/turn/id")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Ok(None);
        }
        let (steer, rejected) = {
            let mut steers = self.steers.lock().expect("steers");
            let Some(steer) = steers.requests.remove(id) else {
                return Ok(None);
            };
            let rejected = frame.get("error").is_some();
            if rejected {
                // Most often the turn finished between the send and this
                // request: the text is fine, the turn is gone.
                steers.queued.push_back(steer.clone());
            }
            (steer, rejected)
        };
        let settled = self.completing && self.steers.lock().expect("steers").requests.is_empty();
        let mut event = (!rejected).then(|| Event::Steered(vec![steer.0]));
        if settled {
            self.completing = false;
            let end = self.finish_turn().await?;
            match event {
                Some(_) => self.ready.push_back(end),
                None => event = Some(end),
            }
        }
        Ok(event)
    }

    /// The turn is over. A steer no turn took becomes the next turn on the
    /// same thread; otherwise the run is done.
    async fn finish_turn(&mut self) -> Result<Event, AgentError> {
        let next = {
            let mut steers = self.steers.lock().expect("steers");
            steers
                .queued
                .pop_front()
                .map(|steer| (steer, steers.thread.clone()))
        };
        let Some(((id, text), Some(thread))) = next else {
            return Ok(Event::Done);
        };
        self.process
            .send(json!({"id":format!("{TURN_REQUEST}{id}"),"method":"turn/start","params":{
                "threadId":thread,"effort":self.request.effort,"input":[{"type":"text","text":text}]
            }}))
            .await?;
        Ok(Event::Steered(vec![id]))
    }

    pub(super) fn input(&self) -> super::process::Input {
        self.process.input()
    }

    #[cfg(test)]
    pub async fn reply(&mut self, id: Value, outcome: ToolOutcome) -> Result<(), AgentError> {
        self.process.send(reply_frame(id, outcome)).await
    }
}

const STEER_REQUEST: &str = "luma-steer-";
const TURN_REQUEST: &str = "luma-turn-";

/// Sends steers into a running session's live turn.
pub(in crate::agent) struct Steerer {
    input: super::process::Input,
    steers: Arc<Mutex<Steers>>,
}

impl Steerer {
    pub async fn steer(&self, id: &str, text: &str) -> Result<(), AgentError> {
        let request = {
            let mut steers = self.steers.lock().expect("steers");
            match (steers.thread.clone(), steers.turn.clone()) {
                (Some(thread), Some(turn)) => {
                    let request = format!("{STEER_REQUEST}{id}");
                    steers
                        .requests
                        .insert(request.clone(), (id.to_owned(), text.to_owned()));
                    json!({"id":request,"method":"turn/steer","params":{
                        "threadId":thread,"expectedTurnId":turn,
                        "input":[{"type":"text","text":text}]
                    }})
                }
                _ => {
                    steers.queued.push_back((id.to_owned(), text.to_owned()));
                    return Ok(());
                }
            }
        };
        self.input.send(request).await
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

    /// The picker's Codex window is the one a turn reports: 272,000 at 95%
    /// is 258,400. A model with no window gets none.
    #[test]
    fn codex_windows_match_what_a_turn_reports() {
        let windows = catalog_windows(&json!({"models": [
            {"slug":"gpt-6-astra","context_window":272_000,"max_context_window":872_000,
             "effective_context_window_percent":95},
            {"slug":"plain","context_window":400_000},
            {"slug":"unsized"}
        ]}));
        assert_eq!(windows.get("gpt-6-astra"), Some(&258_400));
        assert_eq!(windows.get("plain"), Some(&400_000));
        assert_eq!(windows.get("unsized"), None);
    }

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
send({'method':'thread/tokenUsage/updated','params':{'tokenUsage':{'total':{'inputTokens':100,'cachedInputTokens':40,'outputTokens':10},'last':{'inputTokens':50,'cachedInputTokens':30,'outputTokens':4},'modelContextWindow':258400}}})
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
        assert_eq!(session.context_window, Some(258_400));
        let last = session.last_usage.expect("last request");
        assert_eq!(
            (
                last.input_tokens,
                last.cache_read_input_tokens,
                last.output_tokens
            ),
            (20, 30, 4)
        );
        assert_eq!(session.usage_total().input_tokens, 60);
        // The request itself, for the context gauge, as soon as it is known.
        assert!(matches!(session.next().await.unwrap(), Event::Step(step) if step == last));
        assert!(matches!(session.next().await.unwrap(),Event::Text(text) if text == "done"));
        assert!(matches!(session.next().await.unwrap(), Event::Done));
    }

    /// A steer joins the live turn through `turn/steer`; one that loses the
    /// race with the turn's end is not lost but starts the next turn on the
    /// same thread.
    #[tokio::test]
    async fn steers_join_the_turn_or_start_the_next() {
        let temp = tempfile::tempdir().unwrap();
        let script = r#"
import json,sys
read=lambda: json.loads(sys.stdin.readline())
def send(x): print(json.dumps(x),flush=True)
assert read()['method']=='initialize'
send({'id':1,'result':{}})
read(); read()
send({'id':4,'result':{'account':{'type':'chatgpt'}}})
read()
send({'id':5,'result':{'config':{}}})
read()
send({'id':2,'result':{'thread':{'id':'native'}}})
assert read()['method']=='turn/start'
send({'id':3,'result':{'turn':{'id':'turn-1'}}})
send({'method':'thread/tokenUsage/updated','params':{'tokenUsage':{'total':{'inputTokens':100,'cachedInputTokens':0,'outputTokens':10},'last':{'inputTokens':100,'cachedInputTokens':0,'outputTokens':10}}}})
steer=read()
assert steer['method']=='turn/steer', steer
assert steer['params']=={'threadId':'native','expectedTurnId':'turn-1','input':[{'type':'text','text':'darker'}]}, steer
send({'id':steer['id'],'result':{'turnId':'turn-1'}})
late=read()
assert late['method']=='turn/steer', late
send({'method':'turn/completed','params':{'turn':{'id':'turn-1','status':'completed'}}})
send({'id':late['id'],'error':{'code':-32600,'message':'no active turn'}})
start=read()
assert start['method']=='turn/start', start
assert start['params']['threadId']=='native' and start['params']['input']==[{'type':'text','text':'and blue'}], start
send({'id':start['id'],'result':{'turn':{'id':'turn-2'}}})
send({'method':'turn/completed','params':{'turn':{'id':'turn-2','status':'completed'}}})
"#;
        let mut command = tokio::process::Command::new("python3");
        command.args(["-c", script]);
        let request = super::super::tests::request(super::super::Engine::Codex, temp.path());
        let mut session = Session::connect(request, Process::start(command).unwrap())
            .await
            .unwrap();
        let steerer = session.steerer();
        assert!(matches!(
            session.next().await.unwrap(),
            Event::Session { .. }
        ));
        assert!(matches!(session.next().await.unwrap(), Event::Usage(_)));
        assert!(matches!(session.next().await.unwrap(), Event::Step(_)));
        steerer.steer("s1", "darker").await.unwrap();
        assert!(matches!(session.next().await.unwrap(), Event::Steered(ids) if ids == ["s1"]));
        steerer.steer("s2", "and blue").await.unwrap();
        assert!(matches!(session.next().await.unwrap(), Event::Steered(ids) if ids == ["s2"]));
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
        context_window: None,
        price: None,
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
                for model in page.iter().filter(|model| model["hidden"] != true) {
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
                        context_window: None,
                        price: None,
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
                    // `model/list` carries no window. A list without windows
                    // is still a list, so a failure here only costs the line.
                    match windows(cwd).await {
                        Ok(windows) => {
                            for model in &mut models {
                                model.context_window = model
                                    .resolved_model
                                    .as_deref()
                                    .and_then(|id| windows.get(id))
                                    .copied();
                            }
                        }
                        Err(error) => {
                            log::warn!("[agent] Codex model windows unavailable: {error}");
                        }
                    }
                    return Ok(models);
                }
            }
            _ => {}
        }
    }
}

/// Each model's window from `codex debug models`, the same catalog
/// `model/list` reads, run under the same config as the app server.
async fn windows(cwd: &std::path::Path) -> Result<HashMap<String, u32>, AgentError> {
    let mut cmd = command("codex", cwd);
    cmd.args(["debug", "models"]).kill_on_drop(true);
    for (key, value) in isolated_config().as_object().expect("config object") {
        cmd.arg("-c").arg(format!("{key}={value}"));
    }
    let output = cmd
        .output()
        .await
        .map_err(|error| protocol(format!("Codex debug models: {error}")))?;
    if !output.status.success() {
        return Err(protocol(format!(
            "Codex debug models exited with {}",
            output.status
        )));
    }
    let catalog: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| protocol(format!("Codex debug models: {error}")))?;
    Ok(catalog_windows(&catalog))
}

/// The window a Codex turn reports as `modelContextWindow`: the catalog's
/// `context_window` scaled by its `effective_context_window_percent`
/// (272,000 at 95% is the 258,400 a GPT-6-Astra turn reports), so the picker
/// and the usage ring show one number.
fn catalog_windows(catalog: &Value) -> HashMap<String, u32> {
    catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let slug = model["slug"].as_str()?;
            let window = model["context_window"].as_u64().filter(|w| *w > 0)?;
            let window = match model["effective_context_window_percent"].as_u64() {
                Some(percent) => window * percent / 100,
                None => window,
            };
            Some((slug.to_string(), u32::try_from(window).ok()?))
        })
        .collect()
}

pub(super) fn reply_frame(id: Value, outcome: ToolOutcome) -> Value {
    let (blocks, failed) = content(outcome);
    let items: Vec<_> = blocks
        .into_iter()
        .filter_map(|b| match b {
            ContentBlock::Text(text) => Some(json!({"type":"inputText","text":text})),
            ContentBlock::Image { media_type, data } => Some(
                json!({"type":"inputImage","imageUrl":format!("data:{media_type};base64,{data}")}),
            ),
            _ => None,
        })
        .collect();
    json!({"id":id,"result":{"contentItems":items,"success":!failed}})
}
