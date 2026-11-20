// SPDX-License-Identifier: GPL-3.0-or-later

//! GStreamer pipeline that keeps a Mutter ScreenCast virtual output alive.
//!
//! Mutter creates the virtual monitor as soon as the ScreenCast session is
//! started, but the PipeWire node must have a consumer or the stream is
//! considered inactive. A `fakesink` consumer is enough: it pulls frames and
//! throws them away, so the monitor keeps producing frames for Sunshine's
//! portal capture.

use gstreamer as gst;
use gstreamer::prelude::*;

use crate::mutter::DisplayError;

/// Build the keepalive pipeline description for one virtual output.
///
/// The refresh rate is rendered as a millihertz fraction (`60000/1000`) so
/// `max-framerate` caps the stream at the requested rate without depending on
/// floating point parsing inside GStreamer.
pub fn pipeline_description(node_id: u32, width: u32, height: u32, refresh_hz: f64) -> String {
    let milli_hz = (refresh_hz * 1000.0).round().max(1.0) as u64;
    format!(
        "pipewiresrc path={node_id} do-timestamp=true \
         ! video/x-raw,width={width},height={height},max-framerate={milli_hz}/1000 \
         ! queue max-size-buffers=1 leaky=downstream \
         ! fakesink sync=false"
    )
}

/// Initialize GStreamer. Safe to call repeatedly; errors are ignored because an
/// already-initialized process returns a success value.
pub fn init() {
    let _ = gst::init();
}

/// Owns the fakesink pipeline that keeps one PipeWire node consumed.
pub struct Keepalive {
    pipeline: gst::Pipeline,
    playing: bool,
}

impl Keepalive {
    /// Parse `description`, wrap it in a pipeline and start playing.
    pub fn new(description: &str) -> Result<Self, DisplayError> {
        init();
        let element = gst::parse::launch(description)
            .map_err(|err| DisplayError::Pipeline(format!("cannot parse pipeline: {err}")))?;
        let pipeline = element
            .downcast::<gst::Pipeline>()
            .map_err(|_| DisplayError::Pipeline("parsed element is not a pipeline".to_string()))?;
        let mut keepalive = Self {
            pipeline,
            playing: false,
        };
        keepalive.play()?;
        Ok(keepalive)
    }

    /// Move the pipeline to PLAYING and wait for the state change to settle.
    pub fn play(&mut self) -> Result<(), DisplayError> {
        let change = self
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|err| DisplayError::Pipeline(format!("set_state(PLAYING) failed: {err}")))?;
        let (_, current, _) = self.pipeline.state(gst::ClockTime::from_seconds(5));
        if matches!(
            current,
            gst::State::Ready | gst::State::Paused | gst::State::Playing
        ) {
            self.playing = true;
            Ok(())
        } else {
            self.playing = false;
            Err(DisplayError::Pipeline(format!(
                "pipeline did not advance past NULL (returned {change:?}, state {current:?})"
            )))
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Non-blocking check for a pipeline error on the bus.
    pub fn bus_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        let message = bus.pop_filtered(&[gst::MessageType::Error])?;
        Some(message_error_text(&message))
    }

    /// Cloneable bus handle for an external error poller.
    pub fn bus(&self) -> Option<gst::Bus> {
        self.pipeline.bus()
    }

    /// Stop the pipeline and release the PipeWire consumer.
    pub fn shutdown(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        self.playing = false;
    }
}

impl Drop for Keepalive {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Human-readable text for a GStreamer error message.
pub(crate) fn message_error_text(message: &gst::Message) -> String {
    match message.view() {
        gst::MessageView::Error(error) => {
            let mut text = error.error().to_string();
            if let Some(debug) = error.debug() {
                text.push_str(" (");
                text.push_str(&debug);
                text.push(')');
            }
            text
        }
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_is_rendered_as_millihertz_fraction() {
        assert_eq!(
            pipeline_description(7, 1280, 720, 60.0),
            "pipewiresrc path=7 do-timestamp=true \
             ! video/x-raw,width=1280,height=720,max-framerate=60000/1000 \
             ! queue max-size-buffers=1 leaky=downstream \
             ! fakesink sync=false"
        );
    }

    #[test]
    fn high_refresh_is_capped() {
        let description = pipeline_description(42, 2560, 1600, 120.0);
        assert!(description.contains("max-framerate=120000/1000"));
        assert!(description.contains("width=2560,height=1600"));
    }

    #[test]
    fn fractional_refresh_rounds_to_millihertz() {
        let description = pipeline_description(3, 1920, 1080, 59.94);
        assert!(description.contains("max-framerate=59940/1000"));
    }
}
