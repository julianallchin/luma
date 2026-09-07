//! Skills, for a host that has no filesystem.
//!
//! The webview cannot walk `resources/skills`, so it reads the same registry
//! the Rust loop does through these two commands. Deliberately not a
//! `Vec<SkillMeta>`: the only two things any consumer needs are the
//! `<available_skills>` block to paste into a system prompt and one skill's
//! envelope to hand back as a tool result. Exporting the parsed struct as well
//! would be a second shape of the same data, and the frontend would be the
//! place someone re-implemented the listing.

use crate::agent::skills;
use crate::dispatch::AppServices;
use crate::dispatch::CommandError;

/// The `<available_skills>` block for a system prompt. Empty when the bundle
/// carries no readable skill.
pub async fn skills_listing(_services: &AppServices) -> Result<String, CommandError> {
    Ok(skills::bundled().listing().to_string())
}

/// One skill, in the `<skill …>` envelope a model reads it in.
pub async fn get_skill(_services: &AppServices, name: String) -> Result<String, CommandError> {
    let registry = skills::bundled();
    registry
        .get(&name)
        .map(skills::Skill::envelope)
        .ok_or_else(|| {
            CommandError::NotFound(format!(
                "unknown skill '{name}'. Available: {}",
                registry.names().join(", ")
            ))
        })
}
