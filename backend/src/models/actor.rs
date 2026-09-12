//! How a writer names itself.

/// The open vocabulary the `changes` log's `actor` column carries, read once
/// here so every surface shows a label the same way rather than each slicing
/// the string.
///
/// Parsing never fails. An actor this build has no reading for is
/// [`Self::Named`] and shows verbatim — an unrecognized writer is still an
/// honest one, and inventing "unknown" for it would lose the only fact the row
/// has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorLabel<'a> {
    /// `user` — the human at the keyboard. Deliberately *nameless*: which
    /// human is the row's owner, which the actor does not record, so the
    /// surface names them ("You", a short uid) and this does not.
    User,
    /// `client:<name>/<version>[:<model>]` — an out-of-process MCP client.
    /// The version is parsed and dropped: it identifies a build of the client,
    /// not a writer, and no surface has room for it.
    Client {
        name: &'a str,
        model: Option<&'a str>,
    },
    /// Everything else: a model key the in-app loop wrote under, or a label
    /// from a producer this build does not know.
    Named(&'a str),
}

impl<'a> ActorLabel<'a> {
    #[must_use]
    pub fn parse(actor: &'a str) -> Self {
        if actor == "user" {
            return Self::User;
        }
        let Some(client) = actor.strip_prefix("client:") else {
            return Self::Named(actor);
        };
        // `name/version` then an optional `:model`. A client label missing its
        // version is still a client, and reading it as one beats falling back
        // to the raw string with the prefix still on it.
        let (name, rest) = client.split_once('/').unwrap_or((client, ""));
        let model = rest.split_once(':').map(|(_, model)| model);
        Self::Client { name, model }
    }
}

impl std::fmt::Display for ActorLabel<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => formatter.write_str("user"),
            Self::Client {
                name,
                model: Some(model),
            } => write!(formatter, "{name} · {model}"),
            Self::Client { name, model: None } => formatter.write_str(name),
            Self::Named(actor) => formatter.write_str(actor),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ActorLabel;

    #[test]
    fn every_shape_of_the_open_actor_vocabulary_reads() {
        assert_eq!(ActorLabel::parse("user"), ActorLabel::User);
        assert_eq!(
            ActorLabel::parse("client:claude-code/2.1.247:opus").to_string(),
            "claude-code · opus"
        );
        assert_eq!(
            ActorLabel::parse("client:author_score/0").to_string(),
            "author_score"
        );
        assert_eq!(
            ActorLabel::parse("claude-opus-5").to_string(),
            "claude-opus-5"
        );
        assert_eq!(ActorLabel::parse("agent").to_string(), "agent");
    }
}

/// A validated writer label, as the `changes` log and `agent_threads.actor`
/// store it.
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
