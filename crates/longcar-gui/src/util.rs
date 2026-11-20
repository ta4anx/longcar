// SPDX-License-Identifier: GPL-3.0-or-later

//! Small widget helpers shared by the pages.

use adw::prelude::*;

/// A tooltip-bearing icon button.
pub fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

/// A tooltip-bearing text button.
pub fn text_button(label: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

pub fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_xalign(0.0);
    label
}

/// CSS class for a screen state badge.
pub fn state_css_class(state: longcar_core::model::ScreenState) -> &'static str {
    use longcar_core::model::ScreenState;
    match state {
        ScreenState::Off => "longcar-state-off",
        ScreenState::Preparing
        | ScreenState::WaitingPortal
        | ScreenState::Starting
        | ScreenState::Ready
        | ScreenState::Streaming => "longcar-state-active",
        ScreenState::Stopping => "longcar-state-off",
        ScreenState::Error => "longcar-state-error",
    }
}

/// Human label for a screen state.
pub fn state_label(state: longcar_core::model::ScreenState) -> &'static str {
    use longcar_core::model::ScreenState;
    match state {
        ScreenState::Off => "Off",
        ScreenState::Preparing => "Preparing",
        ScreenState::WaitingPortal => "Waiting for approval",
        ScreenState::Starting => "Starting",
        ScreenState::Ready => "Ready",
        ScreenState::Streaming => "Streaming",
        ScreenState::Stopping => "Stopping",
        ScreenState::Error => "Error",
    }
}

/// Turn `camelCase` enum identifiers into a readable title (`Vaapi` is kept).
pub fn enum_label(value: &str) -> String {
    match value {
        "auto" => "Auto".to_string(),
        "off" => "Off".to_string(),
        "main" => "Main".to_string(),
        "main10" => "Main 10".to_string(),
        "vaapi" => "VAAPI".to_string(),
        "nvenc" => "NVENC".to_string(),
        "vulkan" => "Vulkan".to_string(),
        "software" => "Software".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "above" => "Above".to_string(),
        "below" => "Below".to_string(),
        "start" => "Start".to_string(),
        "center" => "Center".to_string(),
        "end" => "End".to_string(),
        "hidden" => "Hidden".to_string(),
        "embedded" => "Embedded".to_string(),
        "metadata" => "Metadata".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Select `value` in a dropdown built from `values`.
pub fn select_dropdown(dropdown: &gtk::DropDown, values: &[&str], value: &str) {
    if let Some(index) = values.iter().position(|candidate| *candidate == value) {
        dropdown.set_selected(index as u32);
    }
}

/// Escape a string for the diagnostics text files.
pub fn redact(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.contains("password") || lower.contains("pin=") || lower.contains("token") {
                "[redacted line]"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
