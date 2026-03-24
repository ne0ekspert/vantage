use std::sync::mpsc::{self, Receiver};
use std::thread;

use eframe::egui::{
    self, Color32, ColorImage, Context, Rect, RichText, TextureHandle, TextureOptions, Ui,
};
use image::imageops::FilterType;

use crate::domain::Feature;
use crate::evidence::{
    ensure_evidence_perspective_corners, evidence_image_line_segments, evidence_image_path,
    set_evidence_image_line_segments, EvidenceImageLineSegment,
};

const FALLBACK_ASPECT_RATIO: f32 = 3.0 / 4.0;
const MAX_PREVIEW_EDGE: u32 = 2048;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidencePreviewTarget {
    pub feature_id: String,
    pub image_path: String,
}

pub struct EvidenceImagePreview {
    target: Option<EvidencePreviewTarget>,
    preview_rx: Option<Receiver<PreviewMessage>>,
    texture: Option<TextureHandle>,
    status_message: String,
    image_size: Option<[usize; 2]>,
    draft_line: Option<EvidenceImageLineSegment>,
}

struct LoadedPreview {
    rgba: Vec<u8>,
    size: [usize; 2],
    source_size: [usize; 2],
}

enum PreviewMessage {
    Image(LoadedPreview),
    Error(String),
}

impl Default for EvidenceImagePreview {
    fn default() -> Self {
        Self {
            target: None,
            preview_rx: None,
            texture: None,
            status_message: String::new(),
            image_size: None,
            draft_line: None,
        }
    }
}

impl EvidenceImagePreview {
    pub fn set_target(&mut self, ctx: &Context, target: Option<EvidencePreviewTarget>) {
        if self.target.as_ref() == target.as_ref() {
            return;
        }

        self.preview_rx = None;
        self.texture = None;
        self.image_size = None;
        self.draft_line = None;
        self.target = target.clone();

        if let Some(target) = target {
            self.status_message = "Loading evidence image…".into();
            self.start_worker(ctx, target.image_path);
        } else {
            self.status_message.clear();
        }
    }

    pub fn poll(&mut self, ctx: &Context) {
        let mut latest = None;
        if let Some(rx) = &self.preview_rx {
            while let Ok(message) = rx.try_recv() {
                latest = Some(message);
            }
        }

        match latest {
            Some(PreviewMessage::Image(image)) => {
                self.upload_preview(ctx, image);
                self.preview_rx = None;
                ctx.request_repaint();
            }
            Some(PreviewMessage::Error(error)) => {
                self.texture = None;
                self.image_size = None;
                self.preview_rx = None;
                self.status_message = format!("Preview unavailable: {error}");
                ctx.request_repaint();
            }
            None => {}
        }
    }

    pub fn show_ui(&mut self, ui: &mut Ui, feature: &mut Feature) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        if target.feature_id != feature.id {
            return;
        }
        let image_path = target.image_path.clone();

        let preview_width = ui.available_width().max(160.0);
        let preview_height = self
            .image_size
            .map(|[width, height]| preview_width * height as f32 / width.max(1) as f32)
            .unwrap_or(preview_width * FALLBACK_ASPECT_RATIO)
            .max(120.0);
        let preview_size = egui::vec2(preview_width, preview_height);
        let line_color = Color32::from_rgb(251, 191, 36);

        egui::Frame::canvas(ui.style()).show(ui, |ui| {
            ui.set_width(preview_size.x);
            ui.set_min_height(preview_size.y);
            let (preview_rect, response) = ui.allocate_exact_size(
                preview_size,
                if self.texture.is_some() {
                    egui::Sense::click_and_drag()
                } else {
                    egui::Sense::hover()
                },
            );

            if let Some(texture) = &self.texture {
                ui.painter().image(
                    texture.id(),
                    preview_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                ui.scope_builder(egui::UiBuilder::new().max_rect(preview_rect), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space((preview_size.y - 48.0).max(0.0) * 0.5);
                        if self.preview_rx.is_some() {
                            ui.add(egui::Spinner::new().size(28.0));
                            ui.label("Loading image preview…");
                        } else {
                            ui.label(
                                RichText::new("Preview unavailable")
                                    .color(ui.visuals().warn_fg_color),
                            );
                        }
                    });
                });
            }

            if self.texture.is_some() {
                self.handle_line_drawing(feature, preview_rect, &response);
                paint_segments(
                    ui.painter(),
                    preview_rect,
                    &evidence_image_line_segments(feature),
                    line_color,
                    2.0,
                );
                if let Some(draft_line) = self.draft_line {
                    paint_segments(ui.painter(), preview_rect, &[draft_line], line_color, 2.0);
                }
            }
        });

        if !self.status_message.is_empty() {
            ui.small(&self.status_message);
        }
        if self.texture.is_some() {
            ui.small(
                "Drag on the image to draw a line. Saved lines project onto the map using the perspective quad below.",
            );
        }
        ui.label("Image path");
        let mut image_path = image_path;
        ui.add(
            egui::TextEdit::singleline(&mut image_path)
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width())
                .interactive(false),
        );
    }

    fn start_worker(&mut self, ctx: &Context, image_path: String) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let message = match load_preview(&image_path) {
                Ok(image) => PreviewMessage::Image(image),
                Err(error) => PreviewMessage::Error(error),
            };
            if tx.send(message).is_ok() {
                ctx.request_repaint();
            }
        });
        self.preview_rx = Some(rx);
    }

    fn upload_preview(&mut self, ctx: &Context, preview: LoadedPreview) {
        let image = ColorImage::from_rgba_unmultiplied(preview.size, &preview.rgba);
        let source_size = preview.source_size;
        let loaded_size = preview.size;
        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("evidence-image-preview", image, TextureOptions::LINEAR));
        }
        self.image_size = Some(loaded_size);
        self.status_message = if loaded_size == source_size {
            format!("Image {}x{}", source_size[0], source_size[1])
        } else {
            format!(
                "Preview {}x{} from original {}x{}",
                loaded_size[0], loaded_size[1], source_size[0], source_size[1]
            )
        };
    }

    fn handle_line_drawing(
        &mut self,
        feature: &mut Feature,
        preview_rect: Rect,
        response: &egui::Response,
    ) {
        if response.drag_started() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let start = normalize_point(preview_rect, pointer);
                self.draft_line = Some(EvidenceImageLineSegment { start, end: start });
            }
        }

        if response.dragged() {
            if let (Some(pointer), Some(draft_line)) =
                (response.interact_pointer_pos(), self.draft_line.as_mut())
            {
                draft_line.end = normalize_point(preview_rect, pointer);
            }
        }

        if response.drag_stopped() {
            if let Some(mut draft_line) = self.draft_line.take() {
                if let Some(pointer) = response.interact_pointer_pos() {
                    draft_line.end = normalize_point(preview_rect, pointer);
                }
                if segment_length_px(preview_rect, draft_line) >= 6.0 {
                    let _ = ensure_evidence_perspective_corners(feature);
                    let mut segments = evidence_image_line_segments(feature);
                    segments.push(draft_line);
                    set_evidence_image_line_segments(feature, &segments);
                }
            }
        }
    }
}

pub fn evidence_preview_target_from_feature(feature: &Feature) -> Option<EvidencePreviewTarget> {
    Some(EvidencePreviewTarget {
        feature_id: feature.id.clone(),
        image_path: evidence_image_path(feature)?.to_owned(),
    })
}

fn load_preview(path: &str) -> Result<LoadedPreview, String> {
    let image = image::ImageReader::open(path)
        .map_err(|error| format!("Failed to open image: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("Failed to inspect image format: {error}"))?
        .decode()
        .map_err(|error| format!("Failed to decode image: {error}"))?;

    let source_size = [image.width() as usize, image.height() as usize];
    let resized = resize_for_preview(image);
    let size = [resized.width() as usize, resized.height() as usize];
    let rgba = resized.into_rgba8().into_raw();

    Ok(LoadedPreview {
        rgba,
        size,
        source_size,
    })
}

fn resize_for_preview(image: image::DynamicImage) -> image::DynamicImage {
    let [width, height] = fit_within(image.width(), image.height(), MAX_PREVIEW_EDGE);
    if width == image.width() && height == image.height() {
        image
    } else {
        image.resize(width, height, FilterType::Triangle)
    }
}

fn fit_within(width: u32, height: u32, max_edge: u32) -> [u32; 2] {
    if width <= max_edge && height <= max_edge {
        return [width, height];
    }

    let scale = max_edge as f32 / width.max(height) as f32;
    [
        (width as f32 * scale).round().max(1.0) as u32,
        (height as f32 * scale).round().max(1.0) as u32,
    ]
}

fn paint_segments(
    painter: &egui::Painter,
    preview_rect: Rect,
    segments: &[EvidenceImageLineSegment],
    color: Color32,
    width: f32,
) {
    for segment in segments {
        painter.line_segment(
            [
                denormalize_point(preview_rect, segment.start),
                denormalize_point(preview_rect, segment.end),
            ],
            egui::Stroke::new(width, color),
        );
    }
}

fn normalize_point(preview_rect: Rect, point: egui::Pos2) -> [f32; 2] {
    let x = ((point.x - preview_rect.left()) / preview_rect.width().max(1.0)).clamp(0.0, 1.0);
    let y = ((point.y - preview_rect.top()) / preview_rect.height().max(1.0)).clamp(0.0, 1.0);
    [x, y]
}

fn denormalize_point(preview_rect: Rect, point: [f32; 2]) -> egui::Pos2 {
    egui::pos2(
        preview_rect.left() + preview_rect.width() * point[0].clamp(0.0, 1.0),
        preview_rect.top() + preview_rect.height() * point[1].clamp(0.0, 1.0),
    )
}

fn segment_length_px(preview_rect: Rect, segment: EvidenceImageLineSegment) -> f32 {
    denormalize_point(preview_rect, segment.start)
        .distance(denormalize_point(preview_rect, segment.end))
}

#[cfg(test)]
mod tests {
    use eframe::egui::{pos2, Rect};

    use super::{fit_within, normalize_point, segment_length_px};
    use crate::evidence::EvidenceImageLineSegment;

    #[test]
    fn fit_within_preserves_small_images() {
        assert_eq!(fit_within(1200, 800, 2048), [1200, 800]);
    }

    #[test]
    fn fit_within_scales_down_larger_edge() {
        assert_eq!(fit_within(4000, 2000, 2000), [2000, 1000]);
        assert_eq!(fit_within(1500, 4500, 2000), [667, 2000]);
    }

    #[test]
    fn normalize_point_clamps_to_preview_bounds() {
        let rect = Rect::from_min_max(pos2(10.0, 20.0), pos2(110.0, 220.0));
        assert_eq!(normalize_point(rect, pos2(60.0, 120.0)), [0.5, 0.5]);
        assert_eq!(normalize_point(rect, pos2(-50.0, 500.0)), [0.0, 1.0]);
    }

    #[test]
    fn segment_length_uses_preview_pixels() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0));
        let segment = EvidenceImageLineSegment {
            start: [0.0, 0.0],
            end: [1.0, 0.0],
        };
        assert_eq!(segment_length_px(rect, segment), 200.0);
    }
}
