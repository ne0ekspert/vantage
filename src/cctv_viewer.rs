use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, ColorImage, Context, RichText, TextEdit, TextureHandle, TextureOptions, Ui,
};
use serde::Deserialize;
use serde_json::Value;

use crate::domain::Feature;

const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_millis(33);
const FALLBACK_ASPECT_RATIO: f32 = 9.0 / 16.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerTarget {
    pub feature_id: String,
    pub name: String,
    pub stream_url: String,
}

pub struct ItsCctvViewer {
    target: Option<ViewerTarget>,
    frame_rx: Option<Receiver<FrameMessage>>,
    stop_flag: Option<Arc<AtomicBool>>,
    texture: Option<TextureHandle>,
    status_message: String,
    last_frame_at: Option<Instant>,
    frame_interval: Duration,
    stream_info: Option<StreamInfo>,
}

#[derive(Clone, Debug)]
struct PreviewFrame {
    rgba: Vec<u8>,
    size: [usize; 2],
}

enum FrameMessage {
    StreamReady(StreamInfo),
    Frame(PreviewFrame),
    Error(String),
}

#[derive(Clone, Debug)]
struct StreamInfo {
    width: usize,
    height: usize,
    fps: Option<f32>,
}

#[derive(Deserialize)]
struct FfprobeOutput {
    streams: Vec<FfprobeStream>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
}

impl Default for ItsCctvViewer {
    fn default() -> Self {
        Self {
            target: None,
            frame_rx: None,
            stop_flag: None,
            texture: None,
            status_message: String::new(),
            last_frame_at: None,
            frame_interval: DEFAULT_FRAME_INTERVAL,
            stream_info: None,
        }
    }
}

impl ItsCctvViewer {
    pub fn set_target(&mut self, target: Option<ViewerTarget>) {
        if self
            .target
            .as_ref()
            .zip(target.as_ref())
            .is_some_and(|(current, next)| {
                current.feature_id == next.feature_id && current.stream_url == next.stream_url
            })
            || (self.target.is_none() && target.is_none())
        {
            if let Some(target) = target {
                self.target = Some(target);
            }
            return;
        }

        self.stop_worker();
        self.texture = None;
        self.last_frame_at = None;
        self.frame_interval = DEFAULT_FRAME_INTERVAL;
        self.stream_info = None;
        self.target = target.clone();

        if let Some(target) = target {
            self.status_message = format!("Connecting to {}", target.name);
            self.start_worker(target.stream_url);
        } else {
            self.status_message.clear();
        }
    }

    pub fn poll(&mut self, ctx: &Context) {
        let mut latest_message = None;
        if let Some(rx) = &self.frame_rx {
            while let Ok(message) = rx.try_recv() {
                latest_message = Some(message);
            }
        }

        match latest_message {
            Some(FrameMessage::StreamReady(info)) => {
                self.frame_interval = frame_interval_from_fps(info.fps);
                self.status_message = stream_status_message(&info);
                self.stream_info = Some(info);
                ctx.request_repaint();
            }
            Some(FrameMessage::Frame(frame)) => {
                self.upload_frame(ctx, frame);
                self.last_frame_at = Some(Instant::now());
                ctx.request_repaint();
            }
            Some(FrameMessage::Error(error)) => {
                self.status_message = format!("Preview unavailable: {error}");
                ctx.request_repaint();
            }
            None => {}
        }
    }

    pub fn is_active(&self) -> bool {
        self.target.is_some()
    }

    pub fn repaint_interval(&self) -> Duration {
        self.frame_interval
    }

    pub fn show_ui(&mut self, ui: &mut Ui, feature_id: &str) {
        let Some(target) = self.target.as_ref() else {
            return;
        };
        if target.feature_id != feature_id {
            return;
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Viewer").strong());
        ui.label("ITS CCTV HLS preview");

        let preview_width = ui.available_width().max(160.0);
        let preview_height = self
            .stream_info
            .as_ref()
            .map(|info| preview_width * info.height as f32 / info.width.max(1) as f32)
            .unwrap_or(preview_width * FALLBACK_ASPECT_RATIO);
        let preview_size = egui::vec2(preview_width, preview_height.max(90.0));
        egui::Frame::canvas(ui.style()).show(ui, |ui| {
            ui.set_width(preview_size.x);
            ui.set_min_height(preview_size.y);
            let (preview_rect, _) = ui.allocate_exact_size(preview_size, egui::Sense::hover());
            if let Some(texture) = &self.texture {
                ui.painter().image(
                    texture.id(),
                    preview_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else {
                ui.scope_builder(egui::UiBuilder::new().max_rect(preview_rect), |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space((preview_size.y - 48.0).max(0.0) * 0.5);
                        ui.add(egui::Spinner::new().size(28.0));
                        ui.label("Loading HLS preview…");
                    });
                });
            }
        });

        ui.small(&self.status_message);
        if let Some(last_frame_at) = self.last_frame_at {
            ui.small(format!(
                "Last frame {:.1}s ago",
                last_frame_at.elapsed().as_secs_f32()
            ));
        }
        if let Some(info) = &self.stream_info {
            ui.small(format!("Resolution {}x{}", info.width, info.height));
        }
        ui.label("Stream URL");
        let mut stream_url = target.stream_url.clone();
        ui.add_sized(
            [ui.available_width(), 44.0],
            TextEdit::multiline(&mut stream_url)
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width())
                .interactive(false),
        );
    }

    fn start_worker(&mut self, stream_url: String) {
        let (tx, rx) = mpsc::channel();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_for_thread = Arc::clone(&stop_flag);

        thread::spawn(move || {
            while !stop_flag_for_thread.load(Ordering::Relaxed) {
                match stream_preview_frames(&stream_url, &stop_flag_for_thread, &tx) {
                    Ok(()) => return,
                    Err(error) => {
                        if tx.send(FrameMessage::Error(error)).is_err() {
                            return;
                        }
                        if stop_flag_for_thread.load(Ordering::Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        });

        self.frame_rx = Some(rx);
        self.stop_flag = Some(stop_flag);
    }

    fn stop_worker(&mut self) {
        if let Some(stop_flag) = self.stop_flag.take() {
            stop_flag.store(true, Ordering::Relaxed);
        }
        self.frame_rx = None;
    }

    fn upload_frame(&mut self, ctx: &Context, frame: PreviewFrame) {
        let image = ColorImage::from_rgba_unmultiplied(frame.size, &frame.rgba);
        if let Some(texture) = self.texture.as_mut() {
            texture.set(image, TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("its-cctv-hls-preview", image, TextureOptions::LINEAR));
        }
    }
}

impl Drop for ItsCctvViewer {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

pub fn viewer_target_from_feature(feature: &Feature) -> Option<ViewerTarget> {
    let stream_url = its_cctv_hls_url(&feature.metadata_json)?.to_owned();
    Some(ViewerTarget {
        feature_id: feature.id.clone(),
        name: feature.name.clone(),
        stream_url,
    })
}

fn its_cctv_hls_url(metadata: &Value) -> Option<&str> {
    let source = metadata.get("source")?.as_str()?;
    if source != "its_cctv" {
        return None;
    }

    let format = metadata.get("cctvformat")?.as_str()?;
    if !format.eq_ignore_ascii_case("HLS") {
        return None;
    }

    metadata
        .get("stream_url")?
        .as_str()
        .map(str::trim)
        .filter(|url| !url.is_empty())
}

fn stream_preview_frames(
    stream_url: &str,
    stop_flag: &Arc<AtomicBool>,
    tx: &mpsc::Sender<FrameMessage>,
) -> Result<(), String> {
    let info = probe_stream(stream_url)?;
    if tx.send(FrameMessage::StreamReady(info.clone())).is_err() {
        return Ok(());
    }

    let mut child = Command::new("ffmpeg")
        .arg("-nostdin")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-rw_timeout")
        .arg("5000000")
        .arg("-re")
        .arg("-i")
        .arg(stream_url)
        .arg("-map")
        .arg("0:v:0")
        .arg("-an")
        .arg("-sn")
        .arg("-dn")
        .arg("-f")
        .arg("rawvideo")
        .arg("-pix_fmt")
        .arg("rgba")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Failed to launch ffmpeg: {error}"))?;

    let frame_len = info
        .width
        .checked_mul(info.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "Stream frame size is too large".to_string())?;

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg stdout pipe was unavailable".to_string())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "ffmpeg stderr pipe was unavailable".to_string())?;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }

        let mut rgba = vec![0; frame_len];
        match stdout.read_exact(&mut rgba) {
            Ok(()) => {
                if tx
                    .send(FrameMessage::Frame(PreviewFrame {
                        rgba,
                        size: [info.width, info.height],
                    }))
                    .is_err()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let mut stderr_text = String::new();
                let _ = stderr.read_to_string(&mut stderr_text);
                if stop_flag.load(Ordering::Relaxed) {
                    return Ok(());
                }
                if stderr_text.trim().is_empty() {
                    return Err(format!("Failed to read stream frame: {error}"));
                }
                return Err(first_error_line(&stderr_text));
            }
        }
    }
}

fn first_error_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("ffmpeg could not open the stream")
        .to_owned()
}

fn probe_stream(stream_url: &str) -> Result<StreamInfo, String> {
    let output = Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("stream=width,height,avg_frame_rate,r_frame_rate")
        .arg("-of")
        .arg("json")
        .arg(stream_url)
        .output()
        .map_err(|error| format!("Failed to launch ffprobe: {error}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(first_error_line(&stderr));
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("Failed to decode ffprobe output: {error}"))?;
    let stream = parsed
        .streams
        .into_iter()
        .next()
        .ok_or_else(|| "ffprobe found no video stream".to_string())?;

    let width = stream
        .width
        .filter(|value| *value > 0)
        .ok_or_else(|| "Stream width was unavailable".to_string())?;
    let height = stream
        .height
        .filter(|value| *value > 0)
        .ok_or_else(|| "Stream height was unavailable".to_string())?;
    let fps = stream
        .avg_frame_rate
        .as_deref()
        .and_then(parse_frame_rate)
        .or_else(|| stream.r_frame_rate.as_deref().and_then(parse_frame_rate));

    Ok(StreamInfo {
        width: width as usize,
        height: height as usize,
        fps,
    })
}

fn parse_frame_rate(value: &str) -> Option<f32> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some((numerator, denominator)) = trimmed.split_once('/') {
        let numerator: f32 = numerator.trim().parse().ok()?;
        let denominator: f32 = denominator.trim().parse().ok()?;
        if denominator <= 0.0 {
            return None;
        }
        let fps = numerator / denominator;
        return (fps.is_finite() && fps > 0.0).then_some(fps);
    }

    let fps: f32 = trimmed.parse().ok()?;
    (fps.is_finite() && fps > 0.0).then_some(fps)
}

fn frame_interval_from_fps(fps: Option<f32>) -> Duration {
    let fps = fps.unwrap_or(30.0).clamp(1.0, 60.0);
    Duration::from_secs_f32(1.0 / fps)
}

fn stream_status_message(info: &StreamInfo) -> String {
    match info.fps {
        Some(fps) => format!("Streaming at {fps:.2} fps"),
        None => "Streaming".into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{its_cctv_hls_url, parse_frame_rate};

    #[test]
    fn returns_stream_url_for_its_cctv_hls_metadata() {
        let metadata = json!({
            "source": "its_cctv",
            "cctvformat": "HLS",
            "stream_url": "https://example.com/live"
        });

        assert_eq!(
            its_cctv_hls_url(&metadata),
            Some("https://example.com/live")
        );
    }

    #[test]
    fn ignores_non_hls_or_non_its_metadata() {
        let non_hls = json!({
            "source": "its_cctv",
            "cctvformat": "MP4",
            "stream_url": "https://example.com/file.mp4"
        });
        let other_source = json!({
            "source": "wigle",
            "cctvformat": "HLS",
            "stream_url": "https://example.com/live"
        });

        assert_eq!(its_cctv_hls_url(&non_hls), None);
        assert_eq!(its_cctv_hls_url(&other_source), None);
    }

    #[test]
    fn parses_fractional_frame_rate() {
        let fps = parse_frame_rate("30000/1001").expect("frame rate should parse");
        assert!((fps - 29.97).abs() < 0.01);
        assert_eq!(parse_frame_rate("0/0"), None);
    }
}
