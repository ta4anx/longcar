// SPDX-License-Identifier: GPL-3.0-or-later

//! Mutter virtual display backend for Longcar.
//!
//! This crate owns the load-bearing private Mutter integration: a
//! `screen-cast` session recording a virtual output, the GStreamer keepalive
//! pipeline that makes the PipeWire node live, and the `DisplayConfig` calls
//! that place virtual outputs next to the physical monitors.

pub mod gst;
pub mod mutter;

pub use gst::{Keepalive, pipeline_description};
pub use mutter::{
    DISPLAY_BUS, DISPLAY_INTERFACE, DISPLAY_PATH, DisplayConfigClient, DisplayError, DisplayEvent,
    DisplaySpec, MutterDisplay, SCREENCAST_BUS, SCREENCAST_INTERFACE, SCREENCAST_PATH,
    SESSION_INTERFACE, STREAM_INTERFACE,
};
