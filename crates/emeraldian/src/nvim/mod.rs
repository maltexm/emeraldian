//! Embedded Neovim as the note editor (`[editor] backend = "nvim"`).
//!
//! Design: `docs/superpowers/specs/2026-10-24-nvim-embed-design.md`.
//!
//! The main loop is synchronous, so there is no async runtime here: `rpc` owns
//! one reader thread per nvim process and the loop drains its channel.
//!
//! # Waking the main loop
//!
//! `Rpc::waker()` returns a [`rpc::Waker`], a shared flag the reader thread
//! raises whenever it queues a message (including `Exited`). crossterm's
//! `poll` cannot wait on a channel, so while nvim is active the loop polls
//! with a short timeout (about 4 ms), calls `Waker::take()` after each poll
//! and, when it returns `true`, drains `Rpc::try_recv()` and redraws. The 33 ms
//! tick stays for everything else.

// Nothing outside the tests consumes the client or the grid until the session
// wiring lands.
#[allow(dead_code)]
pub mod grid;
#[allow(dead_code)]
pub mod rpc;
