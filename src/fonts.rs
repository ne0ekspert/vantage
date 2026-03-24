use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

pub fn install_cjk_fallbacks(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut added_font = false;

    for (index, path) in candidate_font_paths().into_iter().enumerate() {
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };

        let font_name = format!("system-cjk-fallback-{index}");
        let mut font_data = FontData::from_owned(bytes);
        font_data.index = 0;

        fonts
            .font_data
            .insert(font_name.clone(), Arc::new(font_data));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push(font_name.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(font_name);
        added_font = true;
    }

    if added_font {
        ctx.set_fonts(fonts);
    }
}

fn candidate_font_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    #[cfg(target_os = "linux")]
    {
        candidates.extend(
            [
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSansCJKkr-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSansCJKjp-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSansCJKsc-Regular.otf",
                "/usr/share/fonts/truetype/noto/NotoSansKR-Regular.otf",
                "/usr/share/fonts/adobe-source-han-sans/SourceHanSans-Regular.otf",
                "/usr/share/fonts/opentype/source-han-sans/SourceHanSans-Regular.otf",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
    }

    #[cfg(target_os = "macos")]
    {
        candidates.extend(
            [
                "/System/Library/Fonts/AppleSDGothicNeo.ttc",
                "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
                "/System/Library/Fonts/PingFang.ttc",
                "/System/Library/Fonts/STHeiti Light.ttc",
                "/System/Library/Fonts/Supplemental/Songti.ttc",
            ]
            .into_iter()
            .map(PathBuf::from),
        );
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(windir) = std::env::var_os("WINDIR") {
            let fonts_dir = PathBuf::from(windir).join("Fonts");
            candidates.extend(
                [
                    "malgun.ttf",
                    "meiryo.ttc",
                    "msgothic.ttc",
                    "msyh.ttc",
                    "msjh.ttc",
                    "simsun.ttc",
                    "YuGothM.ttc",
                ]
                .into_iter()
                .map(|name| fonts_dir.join(name)),
            );
        }
    }

    let mut unique = Vec::new();
    for path in candidates {
        if path.is_file() && !unique.contains(&path) {
            unique.push(path);
        }
    }

    unique
}
