//! A venue's Perform set, changed in one place.
//!
//! The Perform tab (through [`crate::dispatch::handlers::perform`]) and an
//! agent's Python cell (through [`crate::agent_execution::perform_host`])
//! both change the set here, so both get the same checks, the same
//! transaction and the same recompile. A change reads the patterns and the
//! layout, edits them, and stores what differs: every changed pattern must
//! pass a score's checks, every cell must fit its page, and every fader cell
//! must move numbers and every color cell live colors. A pattern that is gone
//! takes its cell and every cell's hold on it with it, and a node that is gone
//! or changed kind leaves the cells that held it. Then the open set
//! recompiles.

use luma_patterns::clip_graph::LiveInputKind;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::database::local::perform_set as db;
use crate::database::local::venue_access::{
    AuthorizedVenue, Read, VenueAccess, VenueResource, Write,
};
use crate::error::CommandError;
use crate::models::perform::{CellContent, PerformLayout, PerformPattern};
use crate::perform::PerformSession;

/// A venue's patterns, the top of the set first, and its layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Set {
    pub patterns: Vec<PerformPattern>,
    pub layout: PerformLayout,
}

impl Set {
    pub fn pattern_mut(&mut self, id: &str) -> Result<&mut PerformPattern, CommandError> {
        self.patterns
            .iter_mut()
            .find(|pattern| pattern.id == id)
            .ok_or_else(no_such_pattern)
    }

    /// Add `pattern`, or replace the one with its id.
    pub fn put(&mut self, pattern: PerformPattern) {
        match self.patterns.iter_mut().find(|held| held.id == pattern.id) {
            Some(held) => *held = pattern,
            None => self.patterns.push(pattern),
        }
    }

    /// Take pattern `id` out of the set. Storing the change takes its cell
    /// and every fader cell's hold on it too.
    pub fn remove(&mut self, id: &str) -> Result<PerformPattern, CommandError> {
        let at = self
            .patterns
            .iter()
            .position(|pattern| pattern.id == id)
            .ok_or_else(no_such_pattern)?;
        Ok(self.patterns.remove(at))
    }

    /// The set as Python reads it: the patterns and the layout, and the name of
    /// every control they play under (`controls::perform`), so a program that
    /// binds or presses one names it as the bank does. A pattern's `pad` is
    /// the id of its pad node, one of its `nodes`, or null. A fader cell and
    /// a target have one control; a color cell a list, one per swatch.
    pub fn described(&self) -> Value {
        use crate::controls::perform as names;
        let patterns: serde_json::Map<String, Value> = self
            .patterns
            .iter()
            .map(|pattern| {
                let nodes: Vec<Value> = pattern
                    .graph
                    .live_inputs()
                    .into_iter()
                    .map(|input| {
                        json!({
                            "node": input.node,
                            "label": input.name,
                            "kind": match input.kind {
                                LiveInputKind::Fader => "fader",
                                LiveInputKind::Color => "color",
                                LiveInputKind::Button(_) => "button",
                            },
                            "control": names::node(&pattern.id, &input.node),
                        })
                    })
                    .collect();
                let controls = json!({
                    "pad": pattern.graph.pad().map(|pad| pad.node),
                    "level": names::level(&pattern.id),
                    "nodes": nodes,
                });
                (pattern.id.clone(), controls)
            })
            .collect();
        // A fader cell's or a target's control, or a color cell's swatches'
        // one by one.
        let cells: serde_json::Map<String, Value> = self
            .layout
            .pages
            .iter()
            .flat_map(|page| page.cells.iter())
            .filter_map(|cell| {
                let controls = match &cell.content {
                    CellContent::Fader { .. } | CellContent::Target { .. } => {
                        json!(names::cell(&cell.id))
                    }
                    CellContent::Color { swatches, .. } => json!((0..swatches.len())
                        .map(|index| names::swatch(&cell.id, index))
                        .collect::<Vec<_>>()),
                    _ => return None,
                };
                Some((cell.id.clone(), controls))
            })
            .collect();
        json!({
            "patterns": self.patterns,
            "layout": self.layout,
            "controls": {
                "patterns": patterns,
                "cells": cells,
                "master": names::MASTER,
                "tap": names::TAP,
            },
        })
    }

    /// The z a new pattern takes to go on top of the set.
    pub fn top(&self) -> i64 {
        self.patterns
            .iter()
            .map(|pattern| pattern.z_index + 1)
            .max()
            .unwrap_or(0)
    }
}

fn no_such_pattern() -> CommandError {
    CommandError::NotFound("No such Perform pattern in this venue".into())
}

/// The set as stored. A venue that never saved a layout has the first page
/// the Perform tab would make for it.
pub async fn read(access: &mut impl AuthorizedVenue) -> Result<Set, String> {
    let patterns = db::list(access).await?;
    let layout = db::layout(access)
        .await?
        .unwrap_or_else(|| PerformLayout::generated(&patterns));
    Ok(Set { patterns, layout })
}

/// One venue's Perform set, changed on someone's behalf.
pub struct PerformSet<'a> {
    pool: &'a SqlitePool,
    session: &'a PerformSession,
    venue_id: &'a str,
    /// Who the change is attributed to: a model or an MCP client. `None` is
    /// the person.
    actor: Option<&'a str>,
}

impl<'a> PerformSet<'a> {
    pub fn new(pool: &'a SqlitePool, session: &'a PerformSession, venue_id: &'a str) -> Self {
        Self {
            pool,
            session,
            venue_id,
            actor: None,
        }
    }

    #[must_use]
    pub fn acting_as(self, actor: Option<&'a str>) -> Self {
        Self { actor, ..self }
    }

    /// The set as stored; see [`read`].
    pub async fn read(&self) -> Result<Set, CommandError> {
        let mut access =
            VenueAccess::<Read>::read(self.pool, VenueResource::Venue(self.venue_id)).await?;
        Ok(read(&mut access).await?)
    }

    /// Change the set in one transaction, then recompile it if it is open.
    /// `change` edits the set as stored; what it leaves is checked and
    /// stored, or nothing is. Answers what `change` answered.
    pub async fn change<T>(
        &self,
        change: impl FnOnce(&mut Set) -> Result<T, CommandError>,
    ) -> Result<T, CommandError> {
        let mut access = VenueAccess::<Write>::write_as(
            self.pool,
            VenueResource::Venue(self.venue_id),
            self.actor,
        )
        .await?;
        let patterns = db::list(&mut access).await?;
        let stored = db::layout(&mut access).await?;
        let before = Set {
            layout: stored
                .clone()
                .unwrap_or_else(|| PerformLayout::generated(&patterns)),
            patterns,
        };
        let mut after = before.clone();
        let answer = change(&mut after)?;
        for pattern in &mut after.patterns {
            self.venue_id.clone_into(&mut pattern.venue_id);
        }
        let mut ids = std::collections::HashSet::new();
        if let Some(twice) = after
            .patterns
            .iter()
            .find(|pattern| !ids.insert(pattern.id.as_str()))
        {
            return Err(CommandError::Invalid(format!(
                "two Perform patterns cannot share the id {}",
                twice.id
            )));
        }
        for gone in &before.patterns {
            if !after.patterns.iter().any(|pattern| pattern.id == gone.id) {
                db::delete(&mut access, &gone.id).await?;
                after.layout.forget(&gone.id);
            }
        }
        for pattern in &after.patterns {
            if before.patterns.contains(pattern) {
                continue;
            }
            pattern.check().map_err(CommandError::Invalid)?;
            db::save(&mut access, pattern.clone())
                .await?
                .ok_or_else(no_such_pattern)?;
        }
        after.layout.tidy(&before.layout, &after.patterns);
        after
            .layout
            .check(&after.patterns)
            .map_err(CommandError::Invalid)?;
        if stored.as_ref() != Some(&after.layout) {
            db::save_layout(&mut access, &after.layout).await?;
        }
        access.commit().await?;
        // Its patterns, one-at-a-time rows and fader cells play in the bank.
        self.session.set_changed(self.venue_id).await;
        Ok(answer)
    }
}
