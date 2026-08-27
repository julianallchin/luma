//! Delegation: one turn run on a thread of its own.
//!
//! A subagent is not a second kind of agent. It is [`AgentService::turn`](super::AgentService::turn) run
//! against a child `agent_threads` row, with the same registry, the same
//! reducer and the same durability protocol — the only differences are that
//! the child's row names its parent and that its writes land on a private
//! workspace head rather than on the live document. Both of those are facts
//! about the *thread*, resolved by `create_thread_with_authored_state` and
//! `prepare_turn`, so nothing in this file re-decides them.
//!
//! What this file owns is the part that is genuinely new: the limits, the
//! progress stream, and the publish. Everything else is delegation in the
//! literal sense — call the loop and wait.
//!
//! # Cancellation
//!
//! There is none to write. The child's [`TurnStream`](super::TurnStream) is
//! awaited *inside* the tool call, which is awaited inside the parent's turn
//! future; dropping the parent stream drops the whole chain, and each turn
//! cancels the way it always did. The one thing a drop cannot do is `await`,
//! so a cancelled child leaves its workspace active — the same case an app
//! that dies mid-turn leaves, and both are retired by
//! [`recover_threads`](crate::agent_execution::thread_cleanup::recover_threads),
//! which reads [`SubagentRegistry::is_running`] to tell a stranded workspace
//! from a live one.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use super::tools::ToolContext;
use super::{transcript, AgentChatMessage, Role, Transcript, TurnEvent, TurnOutcome, UserPrompt};
use crate::database::local::agent_threads as db;
use crate::models::agent_threads::{AgentThread, CreateAgentThreadInput};
use crate::models::authored_state::{AuthoredMergeConflict, AuthoredWorkspaceMerge};

/// How many threads may stand between a delegated turn and the conversation a
/// person started. A child may delegate; a grandchild may not.
///
/// Enforced here rather than by withholding the tool from a deep registry:
/// [`AgentService::with_tools`](super::AgentService::with_tools) can hand any
/// surface to any turn, so a registry arm is a convention while a check
/// against the thread's own ancestry is the fact.
pub const MAX_DEPTH: usize = 2;

/// How many children one thread may have in flight. Matches Pi's per-call
/// worker pool. A refused start is an error the model can read and re-plan
/// around; a queued one is a turn that looks hung.
pub const MAX_CONCURRENT: usize = 4;

/// How much of a child's answer the parent model is shown.
const MAX_RESULT_CHARS: usize = 16_000;

/// How many conflicts travel back to the parent model. Four children merging
/// one document can each produce a full path list, and the parent's context is
/// not where a diff belongs — the proposal revision is.
const MAX_REPORTED_CONFLICTS: usize = 8;

/// What a subagent is doing right now.
///
/// Live state only. It reaches the host as [`TurnEvent::Subagent`] and is
/// never persisted: everything durable about a child is already a row in its
/// own thread.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubagentSnapshot {
    pub child_thread_id: String,
    /// The parent tool call that spawned it — the id of the start chip, so a
    /// pill and a transcript row can find each other without an index.
    pub call_id: String,
    /// The model's own 3–5 word label. The only text a reader sees at a glance.
    pub description: String,
    pub phase: SubagentPhase,
    /// One line of what the child is doing, for a surface with no room for a
    /// transcript. `None` before its first tool call.
    pub activity: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SubagentPhase {
    Running,
    /// The turn is over and its workspace is being published. An abort that
    /// arrives now is ignored, which is what stops a merged workspace being
    /// reported as cancelled.
    Merging,
    Completed,
    Failed,
}

/// What the parent's transcript stores about one delegation.
///
/// The child's transcript is deliberately absent: it is a thread id away, and
/// copying it here would be the second transcript store this design exists to
/// avoid.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SubagentReport {
    pub child_thread_id: String,
    /// The child's final assistant text — the whole of what the parent model
    /// reads on success.
    pub text: String,
    pub outcome: SubagentOutcome,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SubagentOutcome {
    Merged {
        revision_id: String,
    },
    /// The child's work could not be applied. Its proposal revision stands and
    /// its thread is still readable; the conflicts are the handoff.
    Conflicted {
        conflicts: Vec<AuthoredMergeConflict>,
    },
    /// The child produced nothing to publish. Its workspace is retired.
    Failed {
        message: String,
    },
}

/// What one lease occupies while its delegation runs: a slot against its
/// parent's [`MAX_CONCURRENT`] limit, and — once the child's thread row
/// exists — that child's id.
#[derive(Default)]
struct Running {
    slots: HashMap<String, usize>,
    children: HashSet<String>,
}

/// Children in flight, per parent thread.
///
/// In memory because "running" is not durable state — the durable facts are
/// the child's thread row and its workspace row, both of which outlive the
/// run. A lease releases on drop, so a cancelled parent turn frees its slot
/// without a cleanup path anyone could skip.
///
/// The set of running children is the other half of that: the durable rows
/// cannot distinguish a workspace whose turn is still writing from one a drop
/// stranded, and this registry can.
#[derive(Default)]
pub struct SubagentRegistry {
    running: Mutex<Running>,
}

impl SubagentRegistry {
    /// Claim one of `thread_id`'s [`MAX_CONCURRENT`] slots.
    ///
    /// # Errors
    ///
    /// A message naming the limit when the thread is already at it.
    pub(crate) fn acquire(self: &Arc<Self>, thread_id: &str) -> Result<SubagentLease, String> {
        let mut running = self
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let count = running.slots.entry(thread_id.to_owned()).or_default();
        if *count >= MAX_CONCURRENT {
            return Err(format!(
                "this thread already has {MAX_CONCURRENT} subagents running; wait for one to finish"
            ));
        }
        *count += 1;
        Ok(SubagentLease {
            registry: Arc::clone(self),
            thread_id: thread_id.to_owned(),
            child_thread_id: None,
        })
    }

    /// Whether a turn is still writing `child_thread_id`'s workspace.
    ///
    /// The only honest reading of "in flight": a child's rows say a workspace
    /// is active whether the turn that opened it is alive or was dropped, and
    /// a live lease is the difference.
    pub(crate) fn is_running(&self, child_thread_id: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .children
            .contains(child_thread_id)
    }
}

pub(crate) struct SubagentLease {
    registry: Arc<SubagentRegistry>,
    thread_id: String,
    child_thread_id: Option<String>,
}

impl SubagentLease {
    /// Name the child this lease is running.
    ///
    /// Called as soon as the child's thread row exists, which is also when its
    /// workspace row becomes visible: before that there is nothing durable for
    /// a sweep to find, and after it the lease is what says the workspace is
    /// still being written.
    pub(crate) fn attach(&mut self, child_thread_id: &str) {
        self.registry
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .children
            .insert(child_thread_id.to_owned());
        self.child_thread_id = Some(child_thread_id.to_owned());
    }
}

impl Drop for SubagentLease {
    fn drop(&mut self) {
        let mut running = self
            .registry
            .running
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(count) = running.slots.get_mut(&self.thread_id) {
            *count -= 1;
            if *count == 0 {
                running.slots.remove(&self.thread_id);
            }
        }
        if let Some(child) = &self.child_thread_id {
            running.children.remove(child);
        }
    }
}

/// What one `subagent` call asks for.
pub(super) struct Delegation {
    pub description: String,
    pub task: String,
}

/// Run one delegated turn to completion and publish what it wrote.
///
/// # Errors
///
/// A message for a delegation that never started — depth, concurrency, or a
/// thread that could not be created. A child that started and then failed is
/// **not** an error here: it is a [`SubagentReport`] whose outcome says so, so
/// that the child's thread stays findable from the parent's transcript.
pub(super) async fn delegate(
    ctx: &ToolContext<'_>,
    delegation: Delegation,
) -> Result<SubagentReport, String> {
    let services = ctx.services();
    let pool = services.db().0.clone();
    let principal = services
        .admitted_principal()
        .await
        .map_err(|error| error.to_string())?;

    let parent = db::get_thread(&pool, ctx.thread_id, principal.as_deref())
        .await
        .map_err(|error| format!("this thread is not available: {error}"))?
        .thread;
    if depth_of(&pool, principal.as_deref(), &parent).await? + 1 > MAX_DEPTH {
        return Err(format!(
            "subagents may be nested {MAX_DEPTH} deep; do this work yourself"
        ));
    }
    let mut lease = Arc::clone(&services.subagents).acquire(ctx.thread_id)?;

    let child = services
        .authored()
        .create_thread_with_authored_state(
            &pool,
            child_thread_input(&parent, ctx.call_id, &delegation.description),
            principal.as_deref(),
        )
        .await
        .map_err(|error| format!("could not open a thread for this subagent: {error}"))?;
    lease.attach(&child.id);

    let mut snapshot = SubagentSnapshot {
        child_thread_id: child.id.clone(),
        call_id: ctx.call_id.to_owned(),
        description: delegation.description,
        phase: SubagentPhase::Running,
        activity: None,
    };
    ctx.progress.subagent(&snapshot);

    let (result, transcript) = run_child(ctx, &child.id, delegation.task, &mut snapshot).await;
    let text = final_assistant_text(&transcript);

    let outcome = match result {
        Err(message) => {
            discard(ctx, &child.id).await;
            SubagentOutcome::Failed { message }
        }
        Ok(()) => {
            snapshot.phase = SubagentPhase::Merging;
            snapshot.activity = None;
            ctx.progress.subagent(&snapshot);
            publish(ctx, &child.id, &text).await
        }
    };
    snapshot.phase = match &outcome {
        SubagentOutcome::Merged { .. } => SubagentPhase::Completed,
        _ => SubagentPhase::Failed,
    };
    snapshot.activity = None;
    ctx.progress.subagent(&snapshot);

    Ok(SubagentReport {
        child_thread_id: child.id,
        text,
        outcome,
    })
}

/// Drive the child's turn, forwarding its live state to the parent's host.
///
/// The child's events are folded through the same reducer the hosts use, so
/// "what the child finally said" is read off a [`Transcript`] rather than
/// reassembled from deltas by a second parser.
async fn run_child(
    ctx: &ToolContext<'_>,
    child_thread_id: &str,
    task: String,
    snapshot: &mut SubagentSnapshot,
) -> (Result<(), String>, Transcript) {
    let mut stream = ctx.agent.turn(child_thread_id, UserPrompt::from(task));
    let mut transcript = Transcript::default();
    let mut result = Err("the subagent's turn ended without a verdict".to_string());
    while let Some(event) = stream.next().await {
        transcript::apply(&mut transcript, &event);
        match &event {
            TurnEvent::ToolCallStarted { name, .. } => {
                snapshot.activity = Some(format!("using {name}…"));
                ctx.progress.subagent(snapshot);
            }
            TurnEvent::ToolCallEnded { .. } => {
                snapshot.activity = None;
                ctx.progress.subagent(snapshot);
            }
            TurnEvent::TurnEnded { outcome } => {
                result = match outcome {
                    TurnOutcome::Completed => Ok(()),
                    TurnOutcome::Cancelled => Err("the subagent was cancelled".into()),
                    TurnOutcome::Failed { message } => Err(message.clone()),
                };
            }
            _ => {}
        }
    }
    (result, transcript)
}

/// Merge the child's workspace into whatever head its parent writes.
async fn publish(ctx: &ToolContext<'_>, child_thread_id: &str, text: &str) -> SubagentOutcome {
    let services = ctx.services();
    let principal = match services.admitted_principal().await {
        Ok(principal) => principal,
        Err(error) => {
            return SubagentOutcome::Failed {
                message: error.to_string(),
            }
        }
    };
    let merged = services
        .authored()
        .merge_subagent(
            &services.db().0,
            principal.as_deref(),
            child_thread_id,
            revision_subject(text),
            &format!("subagent-merge-{child_thread_id}"),
        )
        .await;
    match merged {
        Ok(AuthoredWorkspaceMerge::Merged { revision_id, .. }) => {
            SubagentOutcome::Merged { revision_id }
        }
        Ok(AuthoredWorkspaceMerge::Conflicted { mut conflicts }) => {
            conflicts.truncate(MAX_REPORTED_CONFLICTS);
            SubagentOutcome::Conflicted { conflicts }
        }
        Err(error) => SubagentOutcome::Failed {
            message: error.to_string(),
        },
    }
}

/// Retire a failed child's workspace. A failure to retire is not worth failing
/// the parent's turn over — the thread deletion sweep retires it later — but it
/// must not be silent either.
async fn discard(ctx: &ToolContext<'_>, child_thread_id: &str) {
    let services = ctx.services();
    let Ok(principal) = services.admitted_principal().await else {
        return;
    };
    if let Err(error) = services
        .authored()
        .discard_subagent(&services.db().0, principal.as_deref(), child_thread_id)
        .await
    {
        eprintln!("[subagent] could not retire {child_thread_id}'s workspace: {error}");
    }
}

/// How many threads stand between `thread` and the conversation a person
/// started.
///
/// Walked rather than queried: the answer is bounded by [`MAX_DEPTH`], so the
/// walk is two reads at worst and needs no recursive statement of its own.
async fn depth_of(
    pool: &sqlx::SqlitePool,
    principal: Option<&str>,
    thread: &AgentThread,
) -> Result<usize, String> {
    let mut depth = 0;
    let mut parent = thread.parent_thread_id.clone();
    while let Some(id) = parent {
        depth += 1;
        if depth > MAX_DEPTH {
            return Ok(depth);
        }
        parent = db::get_thread(pool, &id, principal)
            .await
            .map_err(|error| format!("this thread's parent is not available: {error}"))?
            .thread
            .parent_thread_id;
    }
    Ok(depth)
}

/// A child thread is its parent's scope plus the relationship.
///
/// Every scope field is copied rather than re-derived, because the database
/// trigger `agent_thread_parent_shares_its_scope` requires exactly that — and
/// because a subagent that could pick its own subject would be a second way to
/// open a conversation.
fn child_thread_input(
    parent: &AgentThread,
    call_id: &str,
    description: &str,
) -> CreateAgentThreadInput {
    CreateAgentThreadInput {
        request_id: uuid::Uuid::new_v4().to_string(),
        agent_kind: parent.agent_kind.clone(),
        subject_kind: parent.subject_kind.clone(),
        subject_id: parent.subject_id.clone(),
        implementation_id: parent.implementation_id.clone(),
        venue_id: parent.venue_id.clone(),
        score_id: parent.score_id.clone(),
        title: Some(description.to_owned()),
        parent_thread_id: Some(parent.id.clone()),
        parent_call_id: Some(call_id.to_owned()),
    }
}

/// The child's last word, which is the whole of what the parent model reads.
fn final_assistant_text(transcript: &Transcript) -> String {
    transcript
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
        .map(AgentChatMessage::text)
        .unwrap_or_default()
}

/// One line naming the revision a child produced, within the 240 bytes a
/// revision subject allows.
fn revision_subject(text: &str) -> &str {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Subagent result");
    let mut end = line.len().min(200);
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    match &line[..end] {
        "" => "Subagent result",
        subject => subject,
    }
}

/// What the parent model is shown: the child's answer, then the fate of its
/// work — the same `<authored_merge/>` envelope the TypeScript stack used, so
/// a prompt that taught one loop to read it teaches both.
pub(super) fn report_for_model(report: &SubagentReport) -> Result<String, String> {
    let text =
        super::tools::clamp_for_model(&report.text, MAX_RESULT_CHARS, "subagent result", 0.4);
    match &report.outcome {
        SubagentOutcome::Merged { revision_id } => Ok(format!(
            "{text}\n\n<authored_merge status=\"merged\" revision_id=\"{revision_id}\"/>"
        )),
        SubagentOutcome::Conflicted { conflicts } => Err(format!(
            "{text}\n\n<authored_merge status=\"conflicted\" conflicts=\"{}\"/>\n\
             The subagent's work is kept as a proposal on thread {} and was not applied. \
             Conflicting paths: {}",
            conflicts.len(),
            report.child_thread_id,
            conflict_paths(conflicts),
        )),
        SubagentOutcome::Failed { message } => Err(format!(
            "The subagent failed and nothing was applied: {message}\n\
             Its thread is {} if you want to read what it did.",
            report.child_thread_id
        )),
    }
}

fn conflict_paths(conflicts: &[AuthoredMergeConflict]) -> String {
    conflicts
        .iter()
        .map(|conflict| {
            conflict
                .path
                .iter()
                .map(|segment| serde_json::to_string(segment).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_is_released_when_it_drops() {
        let registry = Arc::new(SubagentRegistry::default());
        let leases: Vec<_> = (0..MAX_CONCURRENT)
            .map(|_| registry.acquire("thread").expect("slot"))
            .collect();
        let Err(refused) = registry.acquire("thread") else {
            panic!("the {MAX_CONCURRENT}th slot must be refused");
        };
        assert!(refused.contains(&MAX_CONCURRENT.to_string()), "{refused}");
        // A sibling thread has its own slots.
        drop(registry.acquire("other").expect("slot"));
        drop(leases);
        drop(registry.acquire("thread").expect("slot after release"));
        assert!(registry.running.lock().expect("poisoned").slots.is_empty());
    }

    /// A lease releases both halves of what it holds: the parent's slot and
    /// the child it named.
    #[test]
    fn a_lease_releases_the_child_it_named() {
        let registry = Arc::new(SubagentRegistry::default());
        let mut lease = registry.acquire("parent").expect("slot");
        assert!(!registry.is_running("child"));
        lease.attach("child");
        assert!(registry.is_running("child"));
        drop(lease);
        assert!(!registry.is_running("child"));
        let running = registry.running.lock().expect("poisoned");
        assert!(running.slots.is_empty() && running.children.is_empty());
    }

    #[test]
    fn a_revision_subject_is_one_bounded_line() {
        assert_eq!(
            revision_subject("\n\n  Raised the ramp  \nand more"),
            "Raised the ramp"
        );
        assert_eq!(revision_subject("   "), "Subagent result");
        let long = "é".repeat(300);
        assert!(revision_subject(&long).len() <= 200);
    }
}
