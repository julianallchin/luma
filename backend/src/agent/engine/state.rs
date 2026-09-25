//! Machine-local execution state. The synced transcript is authoritative: a
//! native session is reusable only at the exact transcript head it completed.

use super::{AgentError, Engine, Usage};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

pub(crate) struct RunLease {
    directory: PathBuf,
    lock: File,
}

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    engine: Engine,
    model: Option<String>,
    head: String,
    session: NativeSession,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct NativeSession {
    pub id: String,
    pub usage: Usage,
}

/// Why a checkpointed native session could not be resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumeMiss {
    /// No checkpoint on this machine — the thread's first turn, or one run
    /// somewhere else (this state is deliberately machine-local; see the
    /// module doc).
    NoCheckpoint,
    /// The checkpoint was left by a different engine. Luma does not convert
    /// a conversation between providers, so the caller must fail loudly
    /// rather than hydrate or resume — carries the engine that *was*
    /// checkpointed, for the error message.
    EngineChanged(Engine),
    ModelChanged,
    /// The transcript moved since the checkpoint completed: a steered
    /// message, a concurrent writer, or the checkpoint simply being stale.
    HeadMoved,
}

impl std::fmt::Display for ResumeMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCheckpoint => f.write_str("no checkpoint for this thread on this machine"),
            Self::EngineChanged(was) => {
                write!(f, "checkpointed for the {} engine", was.key())
            }
            Self::ModelChanged => f.write_str("model changed since the checkpoint"),
            Self::HeadMoved => f.write_str("transcript head moved since the checkpoint"),
        }
    }
}

impl RunLease {
    pub fn acquire(root: &Path, thread: &str, principal: Option<&str>) -> Result<Self, AgentError> {
        let identity = serde_json::to_vec(&(principal, thread)).map_err(storage)?;
        let key = format!("{:x}", Sha256::digest(identity));
        let directory = root.join("agent-sessions").join(key);
        std::fs::create_dir_all(&directory).map_err(storage)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("run.lock"))
            .map_err(storage)?;
        lock.try_lock()
            .map_err(|e| AgentError::Invalid(format!("cannot acquire thread execution: {e}")))?;
        Ok(Self { directory, lock })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// `Ok` names the session to resume; `Err` says why not, for the caller to
    /// log before it pays to hydrate a fresh native session from the
    /// transcript instead.
    ///
    /// The system prompt, tool descriptions and effort are deliberately not
    /// part of this match: the native CLI is handed a fresh `--system-prompt`
    /// on every call (resumed or not) and, with `--system-prompt-snapshot
    /// off`, actually uses it rather than a value frozen at the session's
    /// first turn — so none of those change what a resumed session does.
    /// Only what changes whether the *same underlying process transcript* is
    /// still the right one to continue does: the engine, the model, and
    /// whether the transcript has moved since this checkpoint completed.
    pub fn resume(
        &self,
        engine: Engine,
        model: &Option<String>,
        head: Option<&str>,
    ) -> Result<Result<NativeSession, ResumeMiss>, AgentError> {
        let data = match std::fs::read(self.directory.join("session.json")) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Err(ResumeMiss::NoCheckpoint))
            }
            Err(e) => return Err(storage(e)),
        };
        let checkpoint: Checkpoint = serde_json::from_slice(&data).map_err(storage)?;
        if checkpoint.engine != engine {
            return Ok(Err(ResumeMiss::EngineChanged(checkpoint.engine)));
        }
        if &checkpoint.model != model {
            return Ok(Err(ResumeMiss::ModelChanged));
        }
        if Some(checkpoint.head.as_str()) != head {
            return Ok(Err(ResumeMiss::HeadMoved));
        }
        Ok(Ok(checkpoint.session))
    }

    /// Invalidate before launching: an interrupted native session may contain
    /// tool calls that never reached a durable Luma assistant row.
    pub fn invalidate(&self) -> Result<(), AgentError> {
        match std::fs::remove_file(self.directory.join("session.json")) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage(e)),
        }
    }

    pub fn checkpoint(
        &self,
        engine: Engine,
        model: Option<String>,
        head: String,
        session: NativeSession,
    ) -> Result<(), AgentError> {
        let data = serde_json::to_vec(&Checkpoint {
            engine,
            model,
            head,
            session,
        })
        .map_err(storage)?;
        let pending = self.directory.join("session.pending");
        std::fs::write(&pending, data).map_err(storage)?;
        std::fs::rename(pending, self.directory.join("session.json")).map_err(storage)
    }
}
impl Drop for RunLease {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit this file before exec closes it.
        // Explicit unlock releases ownership without waiting for that child.
        if let Err(error) = self.lock.unlock() {
            eprintln!("[agent] could not unlock execution: {error}");
        }
    }
}

fn storage(e: impl std::fmt::Display) -> AgentError {
    AgentError::Storage(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_writer_and_exact_head_resume() {
        let root = tempfile::tempdir().unwrap();
        let lease = RunLease::acquire(root.path(), "../thread", Some("a")).unwrap();
        assert!(RunLease::acquire(root.path(), "../thread", Some("a")).is_err());
        assert!(RunLease::acquire(root.path(), "../thread", Some("b")).is_ok());
        lease
            .checkpoint(
                Engine::Codex,
                None,
                "head".into(),
                NativeSession {
                    id: "session".into(),
                    usage: Usage::default(),
                },
            )
            .unwrap();
        assert_eq!(
            lease
                .resume(Engine::Codex, &None, Some("head"))
                .unwrap()
                .map(|s| s.id),
            Ok("session".into())
        );
        assert_eq!(
            lease.resume(Engine::Claude, &None, Some("head")).unwrap(),
            Err(ResumeMiss::EngineChanged(Engine::Codex))
        );
        assert_eq!(
            lease.resume(Engine::Codex, &None, Some("other")).unwrap(),
            Err(ResumeMiss::HeadMoved)
        );
        assert_eq!(
            lease
                .resume(Engine::Codex, &Some("other-model".into()), Some("head"))
                .unwrap(),
            Err(ResumeMiss::ModelChanged)
        );
        lease.invalidate().unwrap();
        assert_eq!(
            lease.resume(Engine::Codex, &None, Some("head")).unwrap(),
            Err(ResumeMiss::NoCheckpoint)
        );
        drop(lease);
        assert!(RunLease::acquire(root.path(), "../thread", Some("a")).is_ok());
    }
}
