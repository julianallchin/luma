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
    context: String,
    session: NativeSession,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct NativeSession {
    pub id: String,
    pub usage: Usage,
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

    pub fn resume(
        &self,
        engine: Engine,
        model: &Option<String>,
        head: Option<&str>,
        context: &str,
    ) -> Result<Option<NativeSession>, AgentError> {
        let data = match std::fs::read(self.directory.join("session.json")) {
            Ok(data) => data,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(storage(e)),
        };
        let checkpoint: Checkpoint = serde_json::from_slice(&data).map_err(storage)?;
        Ok((checkpoint.context == context
            && checkpoint.engine == engine
            && &checkpoint.model == model
            && Some(checkpoint.head.as_str()) == head)
            .then_some(checkpoint.session))
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
        context: String,
        session: NativeSession,
    ) -> Result<(), AgentError> {
        let data = serde_json::to_vec(&Checkpoint {
            engine,
            model,
            head,
            context,
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
                "context".into(),
                NativeSession {
                    id: "session".into(),
                    usage: Usage::default(),
                },
            )
            .unwrap();
        assert_eq!(
            lease
                .resume(Engine::Codex, &None, Some("head"), "context")
                .unwrap()
                .map(|s| s.id),
            Some("session".into())
        );
        assert!(lease
            .resume(Engine::Claude, &None, Some("head"), "context")
            .unwrap()
            .is_none());
        assert!(lease
            .resume(Engine::Codex, &None, Some("other"), "context")
            .unwrap()
            .is_none());
        assert!(lease
            .resume(
                Engine::Codex,
                &Some("other-model".into()),
                Some("head"),
                "context"
            )
            .unwrap()
            .is_none());
        assert!(lease
            .resume(Engine::Codex, &None, Some("head"), "changed-tools")
            .unwrap()
            .is_none());
        lease.invalidate().unwrap();
        assert!(lease
            .resume(Engine::Codex, &None, Some("head"), "context")
            .unwrap()
            .is_none());
        drop(lease);
        assert!(RunLease::acquire(root.path(), "../thread", Some("a")).is_ok());
    }
}
