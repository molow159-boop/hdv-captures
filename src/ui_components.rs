//! Shared visual components.
use super::*;
pub(super) fn section_card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, STROKE))
        .inner_margin(egui::Margin::same(13))
        .corner_radius(egui::CornerRadius::same(9))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
}

pub(super) fn title(ui: &mut egui::Ui, name: &str, caption: &str) {
    ui.label(RichText::new(name).size(24.0).strong().color(TEXT));
    ui.label(RichText::new(caption).size(13.0).color(MUTED));
    ui.add_space(12.0);
}

pub(super) fn metric(ui: &mut egui::Ui, value: String, label: &str, color: Color32) {
    section_card(ui, |ui| {
        ui.set_min_height(58.0);
        ui.horizontal(|ui| {
            let kind = if label.starts_with("Fiches") {
                4
            } else if label.starts_with("Enregistr") {
                5
            } else if label.starts_with("À reprendre") {
                1
            } else if label.starts_with("En attente") {
                6
            } else if label.starts_with("Analyse") {
                7
            } else {
                8
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(35.0, 35.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(rect.center(), 19.0, color.gamma_multiply(0.08));
            draw_icon(
                ui.painter(),
                rect.min + egui::vec2(3.0, 3.0),
                29.0,
                kind,
                color,
            );
            ui.vertical(|ui| {
                ui.label(RichText::new(value).size(23.0).strong().color(color));
                ui.label(RichText::new(label).size(11.0).color(MUTED));
            });
        });
    });
}

pub(super) fn switch(
    ui: &mut egui::Ui,
    enabled: bool,
    color: Color32,
    label: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(94.0, 34.0), egui::Sense::click());
    let track = egui::Rect::from_min_size(rect.min, egui::vec2(58.0, 32.0));
    let painter = ui.painter();
    painter.rect_filled(
        track,
        16.0,
        if enabled {
            color.gamma_multiply(0.8)
        } else {
            STROKE
        },
    );
    painter.circle_filled(
        egui::pos2(
            if enabled {
                track.right() - 16.0
            } else {
                track.left() + 16.0
            },
            track.center().y,
        ),
        12.0,
        TEXT,
    );
    painter.text(
        egui::pos2(track.right() + 7.0, track.center().y),
        egui::Align2::LEFT_CENTER,
        if enabled { "ON" } else { "OFF" },
        egui::FontId::proportional(13.0),
        if enabled { color } else { MUTED },
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), enabled, label)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub(super) fn draw_icon(
    painter: &egui::Painter,
    origin: egui::Pos2,
    size: f32,
    kind: u8,
    color: Color32,
) {
    let point = |x: f32, y: f32| origin + egui::vec2(x * size, y * size);
    let stroke = Stroke::new(2.2_f32, color);
    match kind {
        0 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.06, 0.23), point(0.94, 0.87)),
                size * 0.1,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_stroke(point(0.5, 0.55), size * 0.19, stroke);
            painter.line_segment([point(0.28, 0.23), point(0.35, 0.1)], stroke);
            painter.line_segment([point(0.35, 0.1), point(0.65, 0.1)], stroke);
            painter.line_segment([point(0.65, 0.1), point(0.72, 0.23)], stroke);
        }
        1 => {
            painter.add(egui::Shape::closed_line(
                vec![point(0.5, 0.05), point(0.96, 0.91), point(0.04, 0.91)],
                stroke,
            ));
            painter.line_segment([point(0.5, 0.33), point(0.5, 0.59)], stroke);
            painter.circle_filled(point(0.5, 0.75), size * 0.035, color);
        }
        2 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.07, 0.07), point(0.93, 0.93)),
                size * 0.12,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.circle_stroke(point(0.31, 0.31), size * 0.07, stroke);
            painter.add(egui::Shape::line(
                vec![
                    point(0.12, 0.76),
                    point(0.44, 0.45),
                    point(0.65, 0.65),
                    point(0.78, 0.5),
                    point(0.91, 0.64),
                ],
                stroke,
            ));
        }
        4 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.2, 0.05), point(0.8, 0.95)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            for y in [0.38, 0.56, 0.74] {
                painter.line_segment([point(0.34, y), point(0.68, y)], stroke);
            }
        }
        5 => {
            painter.circle_stroke(point(0.5, 0.5), size * 0.41, stroke);
            painter.add(egui::Shape::line(
                vec![point(0.25, 0.5), point(0.44, 0.7), point(0.77, 0.29)],
                stroke,
            ));
        }
        6 => {
            painter.circle_stroke(point(0.5, 0.5), size * 0.41, stroke);
            painter.add(egui::Shape::line(
                vec![point(0.5, 0.23), point(0.5, 0.51), point(0.7, 0.62)],
                stroke,
            ));
        }
        7 => {
            for n in 0..4 {
                let x = 0.12 + n as f32 * 0.22;
                let y = 0.67 - n as f32 * 0.17;
                painter.rect_filled(
                    egui::Rect::from_min_max(point(x, y), point(x + 0.12, 0.9)),
                    2.0,
                    color,
                );
            }
        }
        8 => {
            painter.rect_stroke(
                egui::Rect::from_min_max(point(0.08, 0.2), point(0.92, 0.92)),
                3.0,
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.line_segment([point(0.08, 0.4), point(0.92, 0.4)], stroke);
            for x in [0.3, 0.7] {
                painter.line_segment([point(x, 0.08), point(x, 0.3)], stroke);
            }
            for x in [0.3, 0.5, 0.7] {
                painter.circle_filled(point(x, 0.64), size * 0.04, color);
            }
        }
        _ => {
            painter.circle_stroke(point(0.5, 0.5), size * 0.29, stroke);
            painter.circle_stroke(point(0.5, 0.5), size * 0.11, stroke);
            for n in 0..8 {
                let a = n as f32 * std::f32::consts::TAU / 8.0;
                let dir = egui::vec2(a.cos(), a.sin());
                painter.line_segment(
                    [
                        point(0.5, 0.5) + dir * size * 0.29,
                        point(0.5, 0.5) + dir * size * 0.43,
                    ],
                    stroke,
                );
            }
        }
    }
}
