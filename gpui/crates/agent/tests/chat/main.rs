//! The chat suite: the agent panel, driven by a scripted model.
//!
//! Its own binary because its tests are coupled to *wall clock*, not just to
//! each other. They stream a reply at a fixed cadence and assert the
//! transcript grew between frames, so they need CPU on the schedule the
//! cadence assumes. Run alongside the other 38 headless tests, the scheduler
//! starves the streaming thread and "grew between frames" reads two identical
//! frames — the suite failed exactly that way before this split.
//!
//! They are also already serialized among themselves: `support::chat` seeds
//! one library for the whole binary and hands out exclusive turns with it, so
//! there is nothing to gain from sharing a process with anything else.
//!
//! `cargo test --test chat`.

mod agent_chat;
mod context_gauge;
mod dialog_keyboard;
mod subagents;
