// SPDX-License-Identifier: GPL-3.0-or-later

//! Layout planner: turns physical monitors plus active virtual screens into a
//! `LogicalMonitorConfig` list for Mutter's `ApplyMonitorsConfig`.
//!
//! Physical monitors keep their current arrangement (position, scale,
//! transform, primary flag). Each virtual screen is placed relative to a
//! reference logical rect, then every logical rect is normalized so that the
//! minimum x and y are zero. Virtual screens may never be primary and must not
//! overlap any other logical monitor.

use std::collections::HashMap;

use crate::config::is_valid_id;
use crate::model::{
    Alignment, LogicalMonitorConfig, MonitorMode, MonitorRef, Position, Rect, warnings,
};

/// A physical monitor as reported by DisplayConfig's current state.
#[derive(Debug, Clone)]
pub struct PhysicalMonitor {
    pub connector: String,
    pub name: String,
    pub rect: Rect,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub mode_id: String,
    pub physical_width: u32,
    pub physical_height: u32,
    pub supported_scales: Vec<f64>,
    pub modes: Vec<MonitorMode>,
}

/// An active virtual screen to place in the logical layout.
#[derive(Debug, Clone)]
pub struct VirtualMonitor {
    pub screen_id: String,
    pub connector: String,
    pub mode_id: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub position: Position,
    pub relative_to: String,
    pub alignment: Alignment,
    pub physical_width: u32,
    pub physical_height: u32,
    pub supported_scales: Vec<f64>,
    pub modes: Vec<MonitorMode>,
}

/// Result of planning a layout.
#[derive(Debug, Clone)]
pub struct LayoutPlan {
    /// Apply-ready configs: physical monitors first (input order), then
    /// virtual screens sorted by `screen_id`.
    pub monitors: Vec<LogicalMonitorConfig>,
    /// `(screen_id, normalized logical rect)` for every virtual screen.
    pub virtual_rects: Vec<(String, Rect)>,
    /// Non-fatal issues, using [`crate::model::warnings`] constants.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("virtual screen '{a}' overlaps logical monitor '{b}'")]
    Overlap { a: String, b: String },
    #[error("relative placement cycle involving screen '{0}'")]
    Cycle(String),
    #[error("scale {requested} is not supported")]
    UnsupportedScale { requested: f64, supported: Vec<f64> },
    #[error("no mode matches {width}x{height}@{refresh_hz} Hz")]
    UnsupportedMode {
        width: u32,
        height: u32,
        refresh_hz: f64,
        available: Vec<String>,
    },
    #[error("unknown reference monitor '{0}'")]
    UnknownReference(String),
    #[error("no physical monitor is marked primary")]
    NoPrimary,
}

/// Plan a logical layout for the given physical monitors and virtual screens.
pub fn plan_layout(
    physical: &[PhysicalMonitor],
    virtuals: &[VirtualMonitor],
) -> Result<LayoutPlan, LayoutError> {
    // Deterministic order: output and warning order follow `screen_id`.
    let mut ordered: Vec<&VirtualMonitor> = virtuals.iter().collect();
    ordered.sort_by(|a, b| a.screen_id.cmp(&b.screen_id));

    let primary = physical.iter().find(|m| m.primary).map(|m| m.rect);

    let mut index: HashMap<&str, usize> = HashMap::with_capacity(ordered.len());
    for (i, v) in ordered.iter().enumerate() {
        index.insert(v.screen_id.as_str(), i);
    }

    let mut resolver = Resolver {
        physical,
        virtuals: &ordered,
        index,
        primary,
        state: vec![Visit::Unvisited; ordered.len()],
        rects: vec![None; ordered.len()],
        warnings: Vec::new(),
    };
    for i in 0..ordered.len() {
        resolver.resolve(i)?;
    }

    let mut virtual_rects = Vec::with_capacity(ordered.len());
    for (i, v) in ordered.iter().enumerate() {
        let rect = resolver.rects[i].expect("every virtual rect resolved above");
        virtual_rects.push((v.screen_id.clone(), rect));
    }

    // Overlap: a virtual screen may not intersect a physical or another
    // virtual rect. Shared edges are not an intersection (see `Rect::intersects`).
    for (i, (label, rect)) in virtual_rects.iter().enumerate() {
        for monitor in physical {
            if rect.intersects(&monitor.rect) {
                return Err(LayoutError::Overlap {
                    a: label.clone(),
                    b: monitor.connector.clone(),
                });
            }
        }
        for (other, other_rect) in &virtual_rects[i + 1..] {
            if rect.intersects(other_rect) {
                return Err(LayoutError::Overlap {
                    a: label.clone(),
                    b: other.clone(),
                });
            }
        }
    }

    // Normalize all logical rects so the minimum x and y are zero.
    let min_x = physical
        .iter()
        .map(|m| m.rect.x)
        .chain(virtual_rects.iter().map(|(_, r)| r.x))
        .min()
        .unwrap_or(0);
    let min_y = physical
        .iter()
        .map(|m| m.rect.y)
        .chain(virtual_rects.iter().map(|(_, r)| r.y))
        .min()
        .unwrap_or(0);

    let mut monitors = Vec::with_capacity(physical.len() + ordered.len());
    for monitor in physical {
        monitors.push(LogicalMonitorConfig {
            x: monitor.rect.x - min_x,
            y: monitor.rect.y - min_y,
            scale: monitor.scale,
            transform: monitor.transform,
            primary: monitor.primary,
            monitors: vec![MonitorRef {
                connector: monitor.connector.clone(),
                mode_id: monitor.mode_id.clone(),
            }],
        });
    }
    for (i, v) in ordered.iter().enumerate() {
        let rect = virtual_rects[i].1;
        monitors.push(LogicalMonitorConfig {
            x: rect.x - min_x,
            y: rect.y - min_y,
            scale: v.scale,
            transform: 0,
            primary: false,
            monitors: vec![MonitorRef {
                connector: v.connector.clone(),
                mode_id: v.mode_id.clone(),
            }],
        });
    }
    for (_, rect) in &mut virtual_rects {
        rect.x -= min_x;
        rect.y -= min_y;
    }

    Ok(LayoutPlan {
        monitors,
        virtual_rects,
        warnings: resolver.warnings,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Visit {
    Unvisited,
    Visiting,
    Done,
}

struct Resolver<'a> {
    physical: &'a [PhysicalMonitor],
    virtuals: &'a [&'a VirtualMonitor],
    index: HashMap<&'a str, usize>,
    primary: Option<Rect>,
    state: Vec<Visit>,
    rects: Vec<Option<Rect>>,
    warnings: Vec<String>,
}

impl Resolver<'_> {
    fn resolve(&mut self, idx: usize) -> Result<Rect, LayoutError> {
        match self.state[idx] {
            Visit::Done => return Ok(self.rects[idx].expect("resolved rect cached")),
            Visit::Visiting => {
                return Err(LayoutError::Cycle(self.virtuals[idx].screen_id.clone()));
            }
            Visit::Unvisited => {}
        }
        self.state[idx] = Visit::Visiting;
        let v = self.virtuals[idx];
        let reference = self.reference_rect(&v.relative_to)?;
        let width = logical_size(v.width, v.scale);
        let height = logical_size(v.height, v.scale);
        let rect = place(reference, width, height, v.position, v.alignment);
        self.state[idx] = Visit::Done;
        self.rects[idx] = Some(rect);
        Ok(rect)
    }

    /// Resolve a `relative_to` value to a logical rect.
    ///
    /// * empty or `"primary"` -> the primary physical monitor
    /// * a physical connector -> that monitor's rect
    /// * another active virtual's `screen_id` -> that virtual (topological)
    /// * an id-shaped but inactive screen -> primary plus
    ///   [`warnings::RELATIVE_TO_INACTIVE`]
    /// * anything else -> [`LayoutError::UnknownReference`]
    fn reference_rect(&mut self, reference: &str) -> Result<Rect, LayoutError> {
        if reference.is_empty() || reference == "primary" {
            return self.primary.ok_or(LayoutError::NoPrimary);
        }
        if let Some(monitor) = self.physical.iter().find(|m| m.connector == reference) {
            return Ok(monitor.rect);
        }
        if let Some(&idx) = self.index.get(reference) {
            return self.resolve(idx);
        }
        if is_valid_id(reference) {
            if !self
                .warnings
                .iter()
                .any(|w| w == warnings::RELATIVE_TO_INACTIVE)
            {
                self.warnings
                    .push(warnings::RELATIVE_TO_INACTIVE.to_string());
            }
            return self.primary.ok_or(LayoutError::NoPrimary);
        }
        Err(LayoutError::UnknownReference(reference.to_string()))
    }
}

fn logical_size(physical: u32, scale: f64) -> i32 {
    (f64::from(physical) / scale).round() as i32
}

/// Place a `width`x`height` rect relative to `reference`.
fn place(
    reference: Rect,
    width: i32,
    height: i32,
    position: Position,
    alignment: Alignment,
) -> Rect {
    // Alignment along the y axis (used by Left/Right placement).
    let aligned_y = match alignment {
        Alignment::Start => reference.y,
        Alignment::Center => reference.y + (reference.height - height) / 2,
        Alignment::End => reference.bottom() - height,
    };
    // Alignment along the x axis (used by Above/Below placement).
    let aligned_x = match alignment {
        Alignment::Start => reference.x,
        Alignment::Center => reference.x + (reference.width - width) / 2,
        Alignment::End => reference.right() - width,
    };
    match position {
        Position::Left => Rect::new(reference.x - width, aligned_y, width, height),
        Position::Right => Rect::new(reference.right(), aligned_y, width, height),
        Position::Above => Rect::new(aligned_x, reference.y - height, width, height),
        Position::Below => Rect::new(aligned_x, reference.bottom(), width, height),
    }
}

/// Pick the supported scale nearest to `requested`, within 0.05.
pub fn choose_scale(supported: &[f64], requested: f64) -> Result<f64, LayoutError> {
    let mut best: Option<(f64, f64)> = None;
    for &candidate in supported {
        let diff = (candidate - requested).abs();
        // Inclusive tolerance; the epsilon absorbs binary float representation
        // error such as |1.25 - 1.3| == 0.05000000000000004.
        if !diff.is_finite() || diff > 0.05 + 1e-9 {
            continue;
        }
        let replace = match best {
            None => true,
            // Exact ties pick the smaller value deterministically.
            Some((best_diff, best_value)) => {
                diff < best_diff || (diff == best_diff && candidate < best_value)
            }
        };
        if replace {
            best = Some((diff, candidate));
        }
    }
    match best {
        Some((_, value)) => Ok(value),
        None => Err(LayoutError::UnsupportedScale {
            requested,
            supported: supported.to_vec(),
        }),
    }
}

/// Pick the mode with exact `width`x`height` and the nearest refresh rate
/// within 1 Hz.
pub fn choose_mode(
    modes: &[MonitorMode],
    width: u32,
    height: u32,
    refresh_hz: f64,
) -> Result<MonitorMode, LayoutError> {
    let mut best: Option<(f64, &MonitorMode)> = None;
    for mode in modes
        .iter()
        .filter(|m| m.width == width && m.height == height)
    {
        let diff = (mode.refresh_hz - refresh_hz).abs();
        if !diff.is_finite() || diff > 1.0 {
            continue;
        }
        let replace = match best {
            None => true,
            Some((best_diff, best_mode)) => {
                diff < best_diff
                    || (diff == best_diff
                        && (mode.refresh_hz < best_mode.refresh_hz
                            || (mode.refresh_hz == best_mode.refresh_hz
                                && mode.id.as_str() < best_mode.id.as_str())))
            }
        };
        if replace {
            best = Some((diff, mode));
        }
    }
    match best {
        Some((_, mode)) => Ok(mode.clone()),
        None => Err(LayoutError::UnsupportedMode {
            width,
            height,
            refresh_hz,
            available: modes
                .iter()
                .map(|m| format!("{}x{}@{}", m.width, m.height, format_refresh(m.refresh_hz)))
                .collect(),
        }),
    }
}

/// Format a refresh rate with at most three decimals, trailing zeros trimmed.
fn format_refresh(hz: f64) -> String {
    let text = format!("{hz:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(id: &str, width: u32, height: u32, refresh_hz: f64) -> MonitorMode {
        MonitorMode {
            id: id.to_string(),
            width,
            height,
            refresh_hz,
            preferred_scale: 1.0,
            supported_scales: vec![1.0],
        }
    }

    fn physical(connector: &str, rect: Rect, primary: bool) -> PhysicalMonitor {
        PhysicalMonitor {
            connector: connector.to_string(),
            name: connector.to_string(),
            rect,
            scale: 1.0,
            transform: 0,
            primary,
            mode_id: format!("{connector}-mode"),
            physical_width: rect.width as u32,
            physical_height: rect.height as u32,
            supported_scales: vec![1.0],
            modes: vec![mode(
                &format!("{connector}-mode"),
                rect.width as u32,
                rect.height as u32,
                60.0,
            )],
        }
    }

    fn virtual_monitor(
        screen_id: &str,
        width: u32,
        height: u32,
        scale: f64,
        position: Position,
        relative_to: &str,
        alignment: Alignment,
    ) -> VirtualMonitor {
        VirtualMonitor {
            screen_id: screen_id.to_string(),
            connector: format!("Meta-{}", screen_id[..1].to_uppercase()),
            mode_id: format!("{screen_id}-mode"),
            width,
            height,
            scale,
            position,
            relative_to: relative_to.to_string(),
            alignment,
            physical_width: width,
            physical_height: height,
            supported_scales: vec![scale],
            modes: vec![mode(&format!("{screen_id}-mode"), width, height, 60.0)],
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "{actual} is not {expected}"
        );
    }

    #[test]
    fn places_all_positions_and_alignments() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let cases = [
            (Position::Right, Alignment::Start, (1920, 0)),
            (Position::Right, Alignment::Center, (1920, 180)),
            (Position::Right, Alignment::End, (1920, 360)),
            // Left places at x = -1280, then normalization shifts to 0.
            (Position::Left, Alignment::Start, (0, 0)),
            (Position::Left, Alignment::Center, (0, 180)),
            (Position::Left, Alignment::End, (0, 360)),
            // Above places at y = -720, then normalization shifts to 0.
            (Position::Above, Alignment::Start, (0, 0)),
            (Position::Above, Alignment::Center, (320, 0)),
            (Position::Above, Alignment::End, (640, 0)),
            (Position::Below, Alignment::Start, (0, 1080)),
            (Position::Below, Alignment::Center, (320, 1080)),
            (Position::Below, Alignment::End, (640, 1080)),
        ];
        for (position, alignment, (x, y)) in cases {
            let v = virtual_monitor("tab", 1280, 720, 1.0, position, "primary", alignment);
            let plan = plan_layout(&physicals, &[v])
                .unwrap_or_else(|e| panic!("{position:?}/{alignment:?} failed: {e}"));
            assert_eq!(
                plan.virtual_rects,
                vec![("tab".to_string(), Rect::new(x, y, 1280, 720))],
                "{position:?}/{alignment:?}"
            );
        }
    }

    #[test]
    fn normalization_shifts_everything() {
        let physicals = [physical("eDP-1", Rect::new(100, 50, 1920, 1080), true)];
        // 2560 logical px left of the reference at x=100 -> x = -2460.
        let v = virtual_monitor(
            "tab",
            2560,
            720,
            1.0,
            Position::Left,
            "primary",
            Alignment::Center,
        );
        let plan = plan_layout(&physicals, &[v]).expect("layout");
        assert_eq!(plan.monitors.len(), 2);
        assert_eq!((plan.monitors[0].x, plan.monitors[0].y), (2560, 0));
        assert_eq!((plan.monitors[1].x, plan.monitors[1].y), (0, 180));
        assert_eq!(
            plan.virtual_rects,
            vec![("tab".to_string(), Rect::new(0, 180, 2560, 720))]
        );
    }

    #[test]
    fn virtual_reference_is_topological() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let a = virtual_monitor(
            "a",
            1280,
            720,
            1.0,
            Position::Right,
            "primary",
            Alignment::Center,
        );
        let b = virtual_monitor("b", 640, 360, 1.0, Position::Right, "a", Alignment::Center);
        // Out-of-order input must still resolve `b` after `a`.
        let plan = plan_layout(&physicals, &[b, a]).expect("layout");
        assert_eq!(
            plan.virtual_rects,
            vec![
                ("a".to_string(), Rect::new(1920, 180, 1280, 720)),
                ("b".to_string(), Rect::new(3200, 360, 640, 360)),
            ]
        );
    }

    #[test]
    fn virtual_can_reference_physical_connector() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), false)];
        let v = virtual_monitor(
            "tab",
            1280,
            720,
            1.0,
            Position::Right,
            "eDP-1",
            Alignment::Center,
        );
        let plan = plan_layout(&physicals, &[v]).expect("layout");
        assert_eq!(
            plan.virtual_rects,
            vec![("tab".to_string(), Rect::new(1920, 180, 1280, 720))]
        );
    }

    #[test]
    fn rejects_cycle() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let a = virtual_monitor("a", 1280, 720, 1.0, Position::Right, "b", Alignment::Center);
        let b = virtual_monitor("b", 1280, 720, 1.0, Position::Right, "a", Alignment::Center);
        for virtuals in [vec![a.clone(), b.clone()], vec![b, a]] {
            match plan_layout(&physicals, &virtuals) {
                Err(LayoutError::Cycle(id)) => assert_eq!(id, "a"),
                other => panic!("expected cycle, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_self_cycle() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let v = virtual_monitor(
            "loop",
            1280,
            720,
            1.0,
            Position::Right,
            "loop",
            Alignment::Center,
        );
        match plan_layout(&physicals, &[v]) {
            Err(LayoutError::Cycle(id)) => assert_eq!(id, "loop"),
            other => panic!("expected self cycle, got {other:?}"),
        }
    }

    #[test]
    fn rejects_overlap_with_physical() {
        let physicals = [
            physical("eDP-1", Rect::new(0, 0, 1920, 1080), true),
            physical("HDMI-1", Rect::new(1920, 0, 1920, 1080), false),
        ];
        let v = virtual_monitor(
            "tab",
            1280,
            720,
            1.0,
            Position::Right,
            "primary",
            Alignment::Center,
        );
        match plan_layout(&physicals, &[v]) {
            Err(LayoutError::Overlap { a, b }) => {
                assert_eq!(a, "tab");
                assert_eq!(b, "HDMI-1");
            }
            other => panic!("expected overlap, got {other:?}"),
        }
    }

    #[test]
    fn rejects_overlap_between_virtuals() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let a = virtual_monitor(
            "a",
            1280,
            720,
            1.0,
            Position::Right,
            "primary",
            Alignment::Center,
        );
        let b = virtual_monitor(
            "b",
            1280,
            720,
            1.0,
            Position::Right,
            "primary",
            Alignment::Center,
        );
        match plan_layout(&physicals, &[b, a]) {
            Err(LayoutError::Overlap { a, b }) => {
                assert_eq!(a, "a");
                assert_eq!(b, "b");
            }
            other => panic!("expected overlap, got {other:?}"),
        }
    }

    #[test]
    fn inactive_reference_warns_and_uses_primary() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let v = virtual_monitor(
            "tab",
            1280,
            720,
            1.0,
            Position::Right,
            "ghost",
            Alignment::Center,
        );
        let plan = plan_layout(&physicals, &[v]).expect("layout");
        assert_eq!(
            plan.virtual_rects,
            vec![("tab".to_string(), Rect::new(1920, 180, 1280, 720))]
        );
        assert_eq!(
            plan.warnings,
            vec![warnings::RELATIVE_TO_INACTIVE.to_string()]
        );
    }

    #[test]
    fn duplicate_inactive_references_warn_once() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let a = virtual_monitor(
            "a",
            1280,
            720,
            1.0,
            Position::Right,
            "ghost",
            Alignment::Center,
        );
        let b = virtual_monitor(
            "b",
            1280,
            720,
            1.0,
            Position::Below,
            "ghost",
            Alignment::Center,
        );
        let plan = plan_layout(&physicals, &[a, b]).expect("layout");
        assert_eq!(
            plan.warnings,
            vec![warnings::RELATIVE_TO_INACTIVE.to_string()]
        );
    }

    #[test]
    fn unknown_reference_is_error() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true)];
        let v = virtual_monitor(
            "tab",
            1280,
            720,
            1.0,
            Position::Right,
            "DP-9",
            Alignment::Center,
        );
        match plan_layout(&physicals, &[v]) {
            Err(LayoutError::UnknownReference(name)) => assert_eq!(name, "DP-9"),
            other => panic!("expected unknown reference, got {other:?}"),
        }
    }

    #[test]
    fn missing_primary_is_error() {
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), false)];
        let v = virtual_monitor(
            "tab",
            1280,
            720,
            1.0,
            Position::Right,
            "primary",
            Alignment::Center,
        );
        match plan_layout(&physicals, &[v]) {
            Err(LayoutError::NoPrimary) => {}
            other => panic!("expected NoPrimary, got {other:?}"),
        }
    }

    #[test]
    fn scale_selection() {
        let supported = [1.0, 1.25, 1.5];
        assert_close(choose_scale(&supported, 1.25).expect("exact"), 1.25);
        assert_close(choose_scale(&supported, 1.04).expect("nearest"), 1.0);
        // Beyond 0.05 of every supported scale there is no answer.
        assert!(choose_scale(&supported, 1.1).is_err());
        assert_close(choose_scale(&supported, 1.46).expect("nearest"), 1.5);
        // Tolerance boundary is inclusive.
        assert_close(choose_scale(&supported, 1.3).expect("boundary"), 1.25);
        // Exact tie picks the smaller scale (within tolerance on both sides).
        assert_close(choose_scale(&[1.0, 1.0625], 1.03125).expect("tie"), 1.0);
    }

    #[test]
    fn scale_tolerance_and_empty() {
        match choose_scale(&[2.0], 1.0) {
            Err(LayoutError::UnsupportedScale {
                requested,
                supported,
            }) => {
                assert_close(requested, 1.0);
                assert_eq!(supported, vec![2.0]);
            }
            other => panic!("expected UnsupportedScale, got {other:?}"),
        }
        match choose_scale(&[], 1.0) {
            Err(LayoutError::UnsupportedScale { supported, .. }) => {
                assert!(supported.is_empty());
            }
            other => panic!("expected UnsupportedScale, got {other:?}"),
        }
    }

    #[test]
    fn mode_selection() {
        let modes = vec![
            mode("1080p60", 1920, 1080, 60.0),
            mode("1080p5994", 1920, 1080, 59.94),
            mode("1080p120", 1920, 1080, 120.0),
            mode("720p60", 1280, 720, 60.0),
        ];
        let picked = choose_mode(&modes, 1920, 1080, 61.0).expect("nearest");
        assert_eq!(picked.id, "1080p60");
        let picked = choose_mode(&modes, 1920, 1080, 60.5).expect("nearest");
        assert_eq!(picked.id, "1080p60");
        let picked = choose_mode(&modes, 1920, 1080, 120.9).expect("within 1 Hz");
        assert_eq!(picked.id, "1080p120");
    }

    #[test]
    fn mode_errors_list_available() {
        let modes = vec![
            mode("1080p60", 1920, 1080, 60.0),
            mode("1080p5994", 1920, 1080, 59.94),
            mode("1080p120", 1920, 1080, 120.0),
            mode("720p60", 1280, 720, 60.0),
        ];
        match choose_mode(&modes, 1920, 1080, 121.1) {
            Err(LayoutError::UnsupportedMode {
                width,
                height,
                available,
                ..
            }) => {
                assert_eq!((width, height), (1920, 1080));
                assert_eq!(
                    available,
                    vec![
                        "1920x1080@60".to_string(),
                        "1920x1080@59.94".to_string(),
                        "1920x1080@120".to_string(),
                        "1280x720@60".to_string(),
                    ]
                );
            }
            other => panic!("expected UnsupportedMode, got {other:?}"),
        }
        // Size match is exact: no 1280x1080 mode.
        match choose_mode(&modes, 1280, 1080, 60.0) {
            Err(LayoutError::UnsupportedMode { available, .. }) => {
                assert_eq!(available.len(), 4);
            }
            other => panic!("expected UnsupportedMode, got {other:?}"),
        }
    }

    #[test]
    fn primary_preserved_and_virtuals_not_primary() {
        let mut hdmi = physical("HDMI-1", Rect::new(1920, 0, 2560, 1440), false);
        hdmi.transform = 1;
        let physicals = [physical("eDP-1", Rect::new(0, 0, 1920, 1080), true), hdmi];
        let v = virtual_monitor(
            "tab",
            1280,
            720,
            1.0,
            Position::Left,
            "primary",
            Alignment::Center,
        );
        let plan = plan_layout(&physicals, &[v]).expect("layout");
        assert_eq!(plan.monitors.len(), 3);
        // Physical monitors first, in input order.
        assert_eq!(plan.monitors[0].monitors[0].connector, "eDP-1");
        assert!(plan.monitors[0].primary);
        assert_eq!((plan.monitors[0].x, plan.monitors[0].y), (1280, 0));
        assert_eq!(plan.monitors[1].monitors[0].connector, "HDMI-1");
        assert!(!plan.monitors[1].primary);
        assert_eq!(plan.monitors[1].transform, 1);
        assert_eq!((plan.monitors[1].x, plan.monitors[1].y), (3200, 0));
        // The virtual monitor is last and never primary.
        assert!(plan.monitors[2].monitors[0].connector.starts_with("Meta-"));
        assert!(!plan.monitors[2].primary);
        assert_eq!(plan.monitors.iter().filter(|m| m.primary).count(), 1);
        assert_eq!(
            plan.virtual_rects,
            vec![("tab".to_string(), Rect::new(0, 180, 1280, 720))]
        );
    }

    #[test]
    fn empty_input_produces_empty_plan() {
        let plan = plan_layout(&[], &[]).expect("layout");
        assert!(plan.monitors.is_empty());
        assert!(plan.virtual_rects.is_empty());
        assert!(plan.warnings.is_empty());
    }
}
