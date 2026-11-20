// SPDX-License-Identifier: GPL-3.0-or-later

//! Cairo preview of the logical desktop: physical monitors plus virtual screens.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use longcar_core::state::{PhysicalEntry, ScreenEntry};

#[derive(Debug, Clone)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    label: String,
    virtual_screen: bool,
    primary: bool,
}

#[derive(Debug, Clone, Default)]
struct PreviewData {
    rects: Vec<Rect>,
}

/// Custom `GtkDrawingArea` showing the current monitor arrangement.
pub struct LayoutPreview {
    area: gtk::DrawingArea,
    data: Rc<RefCell<PreviewData>>,
}

impl LayoutPreview {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height(190)
            .hexpand(true)
            .valign(gtk::Align::Start)
            .tooltip_text("Logical monitor arrangement")
            .build();
        area.add_css_class("layout-preview");
        area.update_property(&[gtk::accessible::Property::Label(
            "Layout preview of physical monitors and virtual screens",
        )]);

        let data: Rc<RefCell<PreviewData>> = Rc::new(RefCell::new(PreviewData::default()));
        let draw_data = data.clone();
        area.set_draw_func(move |area, cr, width, height| {
            draw(area, cr, width, height, &draw_data.borrow());
        });

        Self { area, data }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Replace the preview contents from the state document.
    pub fn update(&self, physical: &[PhysicalEntry], screens: &[ScreenEntry]) {
        let mut data = self.data.borrow_mut();
        data.rects.clear();
        for monitor in physical {
            data.rects.push(Rect {
                x: f64::from(monitor.x),
                y: f64::from(monitor.y),
                width: f64::from(monitor.width),
                height: f64::from(monitor.height),
                label: monitor.connector.clone(),
                virtual_screen: false,
                primary: monitor.primary,
            });
        }
        for screen in screens {
            if let Some(logical) = screen.logical {
                data.rects.push(Rect {
                    x: f64::from(logical.x),
                    y: f64::from(logical.y),
                    width: f64::from(logical.width),
                    height: f64::from(logical.height),
                    label: screen.name.clone(),
                    virtual_screen: true,
                    primary: false,
                });
            }
        }
        drop(data);
        self.area.queue_draw();
    }
}

impl Default for LayoutPreview {
    fn default() -> Self {
        Self::new()
    }
}

fn draw(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    data: &PreviewData,
) {
    let width = f64::from(width);
    let height = f64::from(height);
    let foreground = area.color();
    let accent = adw::StyleManager::default().accent_color_rgba();
    let fg_red = f64::from(foreground.red());
    let fg_green = f64::from(foreground.green());
    let fg_blue = f64::from(foreground.blue());
    let accent_red = f64::from(accent.red());
    let accent_green = f64::from(accent.green());
    let accent_blue = f64::from(accent.blue());

    if data.rects.is_empty() {
        cr.set_source_rgba(fg_red, fg_green, fg_blue, 0.7);
        cr.set_font_size(12.0);
        cr.move_to(16.0, height / 2.0);
        let _ = cr.show_text("No monitors reported by the display server yet");
        return;
    }

    let min_x = data
        .rects
        .iter()
        .map(|rect| rect.x)
        .fold(f64::INFINITY, f64::min);
    let min_y = data
        .rects
        .iter()
        .map(|rect| rect.y)
        .fold(f64::INFINITY, f64::min);
    let max_x = data
        .rects
        .iter()
        .map(|rect| rect.x + rect.width)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_y = data
        .rects
        .iter()
        .map(|rect| rect.y + rect.height)
        .fold(f64::NEG_INFINITY, f64::max);
    let span_x = (max_x - min_x).max(1.0);
    let span_y = (max_y - min_y).max(1.0);

    let padding = 14.0;
    let scale = ((width - 2.0 * padding) / span_x).min((height - 2.0 * padding) / span_y);
    let offset_x = padding + ((width - 2.0 * padding) - span_x * scale) / 2.0;
    let offset_y = padding + ((height - 2.0 * padding) - span_y * scale) / 2.0;

    for rect in &data.rects {
        let x = offset_x + (rect.x - min_x) * scale;
        let y = offset_y + (rect.y - min_y) * scale;
        let w = (rect.width * scale).max(2.0);
        let h = (rect.height * scale).max(2.0);

        let (red, green, blue, fill_alpha, stroke_alpha) = if rect.virtual_screen {
            (accent_red, accent_green, accent_blue, 0.28, 0.95)
        } else {
            (fg_red, fg_green, fg_blue, 0.10, 0.45)
        };

        cr.rectangle(x, y, w, h);
        cr.set_source_rgba(red, green, blue, fill_alpha);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(red, green, blue, stroke_alpha);
        cr.set_line_width(if rect.virtual_screen { 2.0 } else { 1.0 });
        let _ = cr.stroke();

        if w > 44.0 && h > 22.0 {
            cr.set_source_rgba(red, green, blue, 0.95);
            cr.set_font_size(11.0);
            let text_width = cr.text_extents(&rect.label).map(|extents| extents.width());
            let label = if let Ok(text_width) = text_width {
                if text_width + 8.0 > w {
                    // Keep the connector as a suffix when the full label is too wide.
                    rect.label
                        .chars()
                        .take((w / 7.0) as usize)
                        .collect::<String>()
                } else {
                    rect.label.clone()
                }
            } else {
                rect.label.clone()
            };
            cr.move_to(x + 5.0, y + 14.0);
            let _ = cr.show_text(&label);
        }

        if rect.primary && w > 60.0 && h > 38.0 {
            cr.set_font_size(10.0);
            cr.move_to(x + 5.0, y + 28.0);
            cr.set_source_rgba(fg_red, fg_green, fg_blue, 0.6);
            let _ = cr.show_text("primary");
        }
    }
}
