//! How a writer names itself.

/// A validated writer label, as `agent_threads.actor` stores it.
///
/// A label naming a model this build knows is canonicalized to that model's
/// key, so the several spellings the two agent loops accept
/// (`claude-opus-5`, `anthropic/claude-opus-5`) land on one actor. Anything
/// else — `user`, a client label, a retired model — is kept verbatim: an
/// unrecognized writer is still an honest one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor(String);

impl Actor {
    /// A human editing in the app.
    pub const USER: &'static str = "user";

    #[must_use]
    pub fn user() -> Self {
        Self(Self::USER.to_owned())
    }

    /// Read a stored or caller-supplied label.
    ///
    /// # Errors
    ///
    /// If the label is empty, over 256 bytes, or carries anything but
    /// `[A-Za-z0-9-_.:/]`.
    pub fn parse(label: &str) -> Result<Self, String> {
        if label.is_empty()
            || label.len() > 256
            || !label.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
            })
        {
            return Err(format!("invalid actor {label:?}"));
        }
        let label = crate::agent::model::ModelId::parse(label).map_or(label, |id| id.key());
        Ok(Self(label.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
