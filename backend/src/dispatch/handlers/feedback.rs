use crate::database::local::feedback::{self as db, Feedback, Target, Verdict};
use crate::dispatch::AppServices;
use crate::error::CommandError;

/// Record the signed-in account's verdict on an agent's turn or on a clip.
/// `None` takes it back.
pub async fn set_feedback(
    services: &AppServices,
    target: Target,
    verdict: Option<Verdict>,
    note: Option<String>,
) -> Result<(), CommandError> {
    let owner = services.require_session().await?;
    Ok(db::set(&services.db.0, &target, verdict, note.as_deref(), &owner).await?)
}

/// The signed-in account's verdicts on a thread's turns.
pub async fn thread_feedback(
    services: &AppServices,
    thread_id: String,
) -> Result<Vec<Feedback>, CommandError> {
    let owner = services.require_session().await?;
    Ok(db::of_thread(&services.db.0, &thread_id, &owner).await?)
}

/// The signed-in account's verdicts on a score's clips.
pub async fn score_clip_feedback(
    services: &AppServices,
    score_id: String,
) -> Result<Vec<Feedback>, CommandError> {
    let owner = services.require_session().await?;
    Ok(db::of_score_clips(&services.db.0, &score_id, &owner).await?)
}
