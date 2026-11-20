// SPDX-License-Identifier: GPL-3.0-or-later

//! Live integration test for the Mutter virtual display backend.
//!
//! Requires a GNOME Wayland session with Mutter ScreenCast >= 4:
//!
//! ```text
//! cargo test -p longcar-display --test mutter -- --ignored --nocapture
//! ```
//!
//! The test adds a temporary 1280x720 virtual monitor and always removes it
//! again, including on the error path.

use std::collections::HashSet;
use std::fmt::Display;
use std::time::Duration;

use longcar_display::{DisplayConfigClient, DisplaySpec, MutterDisplay};

const OVERALL_TIMEOUT: Duration = Duration::from_secs(30);
const DISAPPEAR_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[tokio::test]
#[ignore = "requires a live GNOME Wayland session with Mutter ScreenCast >= 4"]
async fn virtual_display_lifecycle() {
    match tokio::time::timeout(OVERALL_TIMEOUT, run()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => panic!("virtual display lifecycle failed: {error}"),
        Err(_) => panic!(
            "virtual display lifecycle timed out after {} s",
            OVERALL_TIMEOUT.as_secs()
        ),
    }
}

async fn run() -> Result<(), String> {
    let client = DisplayConfigClient::new().await.map_err(text)?;
    let state = client.current_state().await.map_err(text)?;
    let existing: HashSet<String> = state
        .monitors
        .iter()
        .map(|monitor| monitor.connector.clone())
        .collect();

    let display = MutterDisplay::create(
        DisplaySpec {
            width: 1280,
            height: 720,
            refresh_hz: 60.0,
            cursor_mode: 1,
        },
        &existing,
    )
    .await
    .map_err(text)?;

    let connector = display.connector().to_string();
    let check = check_visible(&client, &display, &connector).await;

    // Always attempt teardown, even when the visibility check failed.
    let stop = display.stop().await;
    let gone = wait_until_gone(&client, &connector).await;

    check?;
    stop.map_err(text)?;
    gone
}

async fn check_visible(
    client: &DisplayConfigClient,
    display: &MutterDisplay,
    connector: &str,
) -> Result<(), String> {
    if display.node_id() == 0 {
        return Err("Mutter reported PipeWire node id 0".to_string());
    }
    let state = client.current_state().await.map_err(text)?;
    let monitor = state
        .monitors
        .iter()
        .find(|monitor| monitor.connector == connector)
        .ok_or_else(|| format!("connector {connector} is missing from DisplayConfig"))?;
    if !monitor
        .modes
        .iter()
        .any(|mode| mode.width == 1280 && mode.height == 720)
    {
        return Err(format!(
            "connector {connector} has no 1280x720 mode (modes: {:?})",
            monitor
                .modes
                .iter()
                .map(|mode| format!("{}x{}@{}", mode.width, mode.height, mode.refresh_hz))
                .collect::<Vec<_>>()
        ));
    }
    println!("ok: {connector} 1280x720@60");
    Ok(())
}

async fn wait_until_gone(client: &DisplayConfigClient, connector: &str) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + DISAPPEAR_TIMEOUT;
    loop {
        let state = client.current_state().await.map_err(text)?;
        if !state
            .monitors
            .iter()
            .any(|monitor| monitor.connector == connector)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "connector {connector} is still present {} s after stop",
                DISAPPEAR_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

fn text(error: impl Display) -> String {
    error.to_string()
}
