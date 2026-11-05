//! The agent: a turn loop on the library's reactor, its injectable model
//! and tools, and the conversations it has had.

use std::future::Future;
use std::sync::Arc;

use luma_lib::agent::model::ModelClient;
use luma_lib::agent::tools::ToolRegistry;
use luma_lib::agent::{AgentService, ThreadScope};
use luma_lib::dispatch::handlers;
use luma_lib::models::agent_threads::AgentThread;

use super::{Library, LibraryError};

impl Library {
    /// The agent loop, on the reactor its database needs.
    ///
    /// A turn outlives the call that starts it, so it cannot borrow this
    /// `Library`; [`AgentService`] owns a shared handle on the same services
    /// every command runs on, which is what keeps the agent's
    /// writes and the app's reads inside one transaction boundary.
    pub fn agent(&self) -> luma_chat::Agent {
        let mut service = AgentService::new(self.services.clone());
        if let Some(model) = &self.model {
            service = service.with_model(Arc::clone(model));
        }
        if let Some(tools) = &self.tools {
            service = service.with_tools(tools.clone());
        }
        luma_chat::Agent::new(service, self.runtime().handle().clone())
    }

    /// Drive agent turns with `model`. Call before the first turn — an
    /// [`AgentService`] is built per turn, so a later change simply takes
    /// effect from the next one.
    pub fn set_agent_model(&mut self, model: Arc<dyn ModelClient>) {
        self.model = Some(model);
    }

    /// Expose `tools` to the agent instead of its kind's own set.
    pub fn set_agent_tools(&mut self, tools: ToolRegistry) {
        self.tools = Some(tools);
    }

    /// Every conversation `scope` names, newest first.
    ///
    /// `agent_thread_list` filters on three of a scope's six fields, so the
    /// other three are narrowed here — through [`ThreadScope::matches`], which
    /// is the one statement of what "this thread is about that subject" means.
    /// A caller that filtered its own rows would be a second one.
    pub fn threads(
        &self,
        scope: &ThreadScope,
    ) -> impl Future<Output = Result<Vec<AgentThread>, LibraryError>> + use<> {
        let scope = scope.clone();
        let (agent_kind, subject_kind, subject_id) = (
            scope.agent_kind.as_str().to_owned(),
            scope.subject_kind.as_str().to_owned(),
            Some(scope.subject_id.clone()),
        );
        let listed = self.call("agent_thread_list", |services| async move {
            handlers::agent_threads::agent_thread_list(
                &services,
                Some(agent_kind),
                Some(subject_kind),
                subject_id,
            )
            .await
        });
        async move {
            let threads = listed.await?;
            Ok(threads
                .into_iter()
                .filter(|thread| scope.matches(thread))
                .collect())
        }
    }
}
