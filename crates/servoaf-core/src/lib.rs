// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! servoaf-core: the platform-independent part of servoaf, a Servo-based
//! browser with a built-in Model Context Protocol (MCP) server.
//!
//! An embedder (the Linux binary today, Android later) creates Servo with a
//! rendering context, builds a [`browser::Browser`] with [`tools::registry`],
//! starts [`mcp::http::start`], and calls [`browser::Browser::pump`] from its
//! event loop.

pub mod browser;
pub mod dom;
pub mod input;
pub mod mcp;
pub mod runtime;
pub mod tools;

pub use browser::{Browser, BrowserConfig};
