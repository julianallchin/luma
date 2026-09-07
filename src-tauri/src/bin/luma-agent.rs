//! Run the same agent service as native chat, without opening a window.

use futures_util::StreamExt;
use luma_lib::{
    agent::{engine::Engine, AgentService, ThreadScope, TurnEvent, TurnOutcome},
    dispatch::{dispatch, SharedServices},
    headless_host::{boot, HostConfig},
};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("luma-agent: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut thread, mut scope, mut prompt, mut engine) = (None, None, None, None);
    let (mut track, mut venue, mut model) = (None, None, None);
    let mut host = Vec::new();
    let mut sync = false;
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!("luma-agent (--thread ID | --scope JSON | --track ID --venue ID) --prompt TEXT [--engine api|codex|claude] [--model NAME] [--sync] [host options]\n\n--scope creates a conversation from a serialized ThreadScope. --thread continues an existing one.\nHost options: --config-dir, --fixtures-root, --cache-dir, --fixture-principal.");
            return Ok(());
        }
        if flag == "--sync" {
            sync = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--thread" => thread = Some(value),
            "--track" => track = Some(value),
            "--venue" => venue = Some(value),
            "--model" => model = Some(value),
            "--scope" => {
                scope =
                    Some(serde_json::from_str::<ThreadScope>(&value).map_err(|e| e.to_string())?)
            }
            "--prompt" => prompt = Some(value),
            "--engine" => engine = Some(Engine::parse(&value).map_err(|e| e.to_string())?),
            "--config-dir" | "--fixtures-root" | "--cache-dir" | "--fixture-principal" => {
                host.extend([flag, value])
            }
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let prompt = prompt
        .filter(|p| !p.trim().is_empty())
        .ok_or("--prompt is required")?;
    if [thread.is_some(), scope.is_some(), track.is_some()]
        .into_iter()
        .filter(|v| *v)
        .count()
        != 1
        || track.is_some() != venue.is_some()
    {
        return Err("provide exactly one of --thread, --scope, or --track with --venue".into());
    }
    let services = boot(&HostConfig::parse_args(host.into_iter())?)
        .await?
        .into_shared();
    if sync {
        sync_library(&services).await?;
    }
    if let (Some(track), Some(venue)) = (track, venue) {
        let score = dispatch(&services, "create_score", &serde_json::json!({
            "requestId":uuid::Uuid::new_v4().to_string(),"trackId":track,"venueId":venue,"name":null
        })).await.map_err(|e| e.to_string())?;
        let score = score["id"].as_str().ok_or("create_score returned no id")?;
        eprintln!("score {score}");
        scope = Some(ThreadScope::track(track, venue, score));
    }
    let mut agent = AgentService::new(services.clone());
    if let Some(model) = model {
        agent = agent.with_model_name(model);
    }
    let thread = match thread {
        Some(id) => id,
        None => {
            agent
                .new_thread(&scope.expect("validated scope"))
                .await
                .map_err(|e| e.to_string())?
                .thread
                .id
        }
    };
    if let Some(engine) = engine {
        agent
            .set_thread_engine(&thread, engine)
            .await
            .map_err(|error| error.to_string())?;
    }
    eprintln!("thread {thread}");
    let mut turn = agent.turn(&thread, prompt.into());
    let mut outcome = Err("agent stream ended without an outcome".to_string());
    loop {
        tokio::select! {
            event = turn.next() => {
                let Some(event) = event else { break; };
                println!("{}", serde_json::to_string(&event).map_err(|e| e.to_string())?);
                if let TurnEvent::TurnEnded { outcome: ended } = event {
                    outcome = match ended {
                        TurnOutcome::Completed => Ok(()),
                        TurnOutcome::Failed {message} => Err(message),
                        TurnOutcome::Cancelled => Err("cancelled".into()),
                    };
                }
            }
            _ = tokio::signal::ctrl_c() => { outcome = Err("cancelled".into()); break; }
        }
    }
    drop(turn);
    if sync {
        if let Err(error) = sync_library(&services).await {
            if outcome.is_ok() {
                return Err(error);
            }
            eprintln!("{error}");
        }
    }
    outcome
}

async fn sync_library(services: &SharedServices) -> Result<(), String> {
    let report = dispatch(services, "sync_full", &serde_json::json!({}))
        .await
        .map_err(|e| e.to_string())?;
    if report["errors"]
        .as_array()
        .is_some_and(|errors| !errors.is_empty())
    {
        return Err(format!("sync did not finish: {}", report["errors"]));
    }
    eprintln!("sync {report}");
    Ok(())
}
