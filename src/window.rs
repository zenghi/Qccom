//! Keep the root window's last visible placement in eframe's existing storage format.

use eframe::egui::{self, pos2, vec2};
use serde::{Deserialize, Serialize};

const KEY: &str = "window";
pub const DEFAULT_SIZE: [f32; 2] = [1300.0, 803.0];

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Placement {
    inner_position_pixels: Option<egui::Pos2>,
    outer_position_pixels: Option<egui::Pos2>,
    fullscreen: bool,
    maximized: bool,
    inner_size_points: Option<egui::Vec2>,
}

fn valid_size(size: egui::Vec2) -> bool {
    // eframe clamps older zero-sized minimized windows to 64 points before our builder hook.
    size.is_finite() && size.x > 64.0 && size.y > 64.0
}

fn valid_position(pos: egui::Pos2) -> bool {
    pos.x.is_finite() && pos.y.is_finite() && !(pos.x == -32000.0 && pos.y == -32000.0)
}

pub fn startup_builder(mut viewport: egui::ViewportBuilder) -> egui::ViewportBuilder {
    if !viewport.inner_size.is_some_and(valid_size) || viewport.position.is_some_and(|p| !valid_position(p)) {
        viewport.position = None;
        return viewport.with_inner_size(DEFAULT_SIZE).with_fullscreen(false).with_maximized(true);
    }
    viewport
}

impl Placement {
    pub fn load(storage: Option<&dyn eframe::Storage>) -> Option<Self> {
        let saved: Self = eframe::get_value(storage?, KEY)?;
        (saved.inner_size_points.is_some_and(valid_size) && saved.outer_position_pixels.is_none_or(valid_position) && saved.inner_position_pixels.is_none_or(valid_position)).then_some(saved)
    }

    pub fn save(self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, KEY, &self);
    }

    pub fn observe(input: &egui::InputState, previous: Option<Self>) -> Option<Self> {
        if input.viewport().minimized == Some(true) {
            return None;
        }
        Self::from_viewport(input.viewport(), input.raw.screen_rect.map(|rect| rect.size()), input.pixels_per_point(), previous)
    }

    fn from_viewport(view: &egui::ViewportInfo, screen: Option<egui::Vec2>, scale: f32, previous: Option<Self>) -> Option<Self> {
        if view.minimized == Some(true) || !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let size = view.inner_rect.map(|r| r.size()).or(screen).filter(|s| valid_size(*s))?;
        let pixels = |rect: Option<egui::Rect>| rect.map(|r| pos2((r.min.x * scale).round(), (r.min.y * scale).round()));
        let inner = pixels(view.inner_rect);
        let outer = pixels(view.outer_rect);
        if !inner.is_none_or(valid_position) || !outer.is_none_or(valid_position) {
            return None;
        }
        let fullscreen = view.fullscreen == Some(true);
        let maximized = view.maximized.unwrap_or_else(|| previous.is_some_and(|p| p.maximized));
        let mut geometry = Self {
            inner_position_pixels: inner,
            outer_position_pixels: outer,
            inner_size_points: Some(size.round()),
            fullscreen,
            maximized,
        };
        if maximized || fullscreen {
            // A maximized window reports its full monitor size. Keep its restore rectangle
            // if it is on this monitor, or use a normal rectangle on the new monitor.
            let on_monitor = |p: &Self| {
                view.outer_rect.is_none_or(|bounds| {
                    p.outer_position_pixels
                        .zip(p.inner_size_points)
                        .is_some_and(|(origin, size)| bounds.contains(origin / scale + size / 2.0))
                })
            };
            if let Some(p) = previous.filter(on_monitor) {
                geometry.inner_position_pixels = p.inner_position_pixels;
                geometry.outer_position_pixels = p.outer_position_pixels;
                geometry.inner_size_points = p.inner_size_points;
            } else {
                let monitor = view.monitor_size.filter(|s| valid_size(*s)).unwrap_or(size);
                let normal = vec2((monitor.x * 0.8).max(900.0).min(monitor.x), (monitor.y * 0.8).max(560.0).min(monitor.y));
                let shift = ((size - normal).max(egui::Vec2::ZERO) * (scale / 2.0)).round();
                geometry.inner_position_pixels = inner.map(|p| p + shift);
                geometry.outer_position_pixels = outer.map(|p| p + shift);
                geometry.inner_size_points = Some(normal.round());
            }
        }
        Some(geometry)
    }

    pub fn maximized(self) -> bool {
        self.maximized && !self.fullscreen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(x: f32, size: [f32; 2], maximized: bool) -> egui::ViewportInfo {
        let rect = egui::Rect::from_min_size(pos2(x, 0.0), size.into());
        egui::ViewportInfo {
            inner_rect: Some(rect),
            outer_rect: Some(rect),
            maximized: Some(maximized),
            ..Default::default()
        }
    }

    #[test]
    fn restores_last_visible_placement_after_minimizing() {
        let normal = Placement::from_viewport(&visible(200.0, [900.0, 580.0], false), None, 1.5, None).unwrap();
        let maximized = Placement::from_viewport(&visible(0.0, [1920.0, 1050.0], true), None, 1.5, Some(normal)).unwrap();
        assert_eq!(maximized.outer_position_pixels, normal.outer_position_pixels);
        assert_eq!(maximized.inner_size_points, normal.inner_size_points);
        assert!(maximized.maximized());

        let minimized = egui::ViewportInfo {
            minimized: Some(true),
            maximized: Some(false),
            ..Default::default()
        };
        assert!(Placement::from_viewport(&minimized, Some(vec2(0.0, 0.0)), 1.5, Some(maximized)).is_none());
    }

    #[test]
    fn startup_keeps_saved_normal_size_and_repairs_old_minimized_size() {
        let valid = egui::ViewportBuilder::default().with_inner_size([900.0, 620.0]).with_position([0.0, 120.0]).with_maximized(false);
        assert_eq!(startup_builder(valid.clone()), valid);
        let broken = egui::ViewportBuilder::default().with_inner_size([64.0, 64.0]).with_position([-32000.0, -32000.0]).with_maximized(false);
        let repaired = startup_builder(broken);
        assert_eq!(repaired.maximized, Some(true));
        assert_eq!(repaired.inner_size, Some(DEFAULT_SIZE.into()));
        assert!(repaired.position.is_none());
    }
}
