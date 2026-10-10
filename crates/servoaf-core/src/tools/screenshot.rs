// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! get-screen-shot.

use std::io::Cursor;
use std::rc::Rc;
use std::time::Duration;

use base64::Engine;
use euclid::{Box2D, Point2D};
use image::{DynamicImage, ImageFormat, RgbaImage, imageops};
use servo::{CSSPixel, WebViewRect};

use super::{TAB_ID_ARG, TAB_ID_DESCRIPTION, resolve_visible_tab};
use crate::browser::Browser;
use crate::mcp::registry::{Args, SchemaBuilder, ToolRegistry, ToolResult, int_arg, number_arg, string_arg};
use crate::runtime::{Slot, timeout};

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(30);

fn encode(image: RgbaImage, jpeg_quality: Option<u8>) -> Result<(Vec<u8>, &'static str), String> {
    let mut bytes = Cursor::new(Vec::new());
    match jpeg_quality {
        Some(quality) => {
            let rgb = DynamicImage::ImageRgba8(image).to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
                .encode_image(&rgb)
                .map_err(|error| error.to_string())?;
            Ok((bytes.into_inner(), "image/jpeg"))
        },
        None => {
            image.write_to(&mut bytes, ImageFormat::Png).map_err(|error| error.to_string())?;
            Ok((bytes.into_inner(), "image/png"))
        },
    }
}

async fn get_screenshot(browser: Rc<Browser>, args: Args) -> ToolResult {
    let result: Result<ToolResult, String> = async {
        let tab = resolve_visible_tab(&browser, &args)?;
        let webview = tab.webview()?;
        let (viewport_width, viewport_height) = browser.viewport_size(&tab);

        let clip = match (
            number_arg(&args, "x"),
            number_arg(&args, "y"),
            number_arg(&args, "width"),
            number_arg(&args, "height"),
        ) {
            (None, None, None, None) => None,
            (x, y, width, height) => {
                let x = x.unwrap_or(0.0).max(0.0);
                let y = y.unwrap_or(0.0).max(0.0);
                let right = (x + width.unwrap_or(viewport_width - x)).min(viewport_width);
                let bottom = (y + height.unwrap_or(viewport_height - y)).min(viewport_height);
                if right - x < 1.0 || bottom - y < 1.0 {
                    return Err("Clip rectangle is outside the viewport".into());
                }
                Some(WebViewRect::Page(Box2D::new(
                    Point2D::<f32, CSSPixel>::new(x as f32, y as f32),
                    Point2D::new(right as f32, bottom as f32),
                )))
            },
        };

        let slot = Slot::default();
        let filler = slot.clone();
        webview.take_screenshot(clip, move |result| filler.fill(result));
        browser.request_paint();
        let image = timeout(SCREENSHOT_TIMEOUT, slot.wait())
            .await
            .map_err(|_| "Timed out waiting for the page to render".to_owned())?
            .map_err(|error| format!("Screenshot failed: {error:?}"))?;

        let scale = number_arg(&args, "scale").unwrap_or(1.0).clamp(0.05, 1.0);
        let image = if scale < 1.0 {
            let width = ((image.width() as f64 * scale).round() as u32).max(1);
            let height = ((image.height() as f64 * scale).round() as u32).max(1);
            imageops::resize(&image, width, height, imageops::FilterType::Triangle)
        } else {
            image
        };
        let (width, height) = image.dimensions();
        let jpeg_quality = match string_arg(&args, "format") {
            Some("jpeg") | Some("jpg") => Some(int_arg(&args, "quality").unwrap_or(80).clamp(1, 100) as u8),
            Some("png") | None => None,
            Some(other) => return Err(format!("Unsupported format: {other}")),
        };
        let (bytes, mime_type) = encode(image, jpeg_quality)?;
        let mut result =
            ToolResult::image(base64::engine::general_purpose::STANDARD.encode(&bytes), mime_type);
        result.add_text(format!(
            "Screenshot {width}x{height} px of the viewport ({:.0}x{:.0} CSS px).",
            viewport_width, viewport_height
        ));
        Ok(result)
    }
    .await;
    result.into()
}

pub fn register(registry: &mut ToolRegistry<Browser>) {
    registry.register(
        "get-screen-shot",
        "Captures what the tab currently shows (the visible viewport) as an image. Use when HTML is \
         not enough to understand the page (canvas, images, complex layouts). A background tab is \
         brought to the front first.",
        SchemaBuilder::new()
            .integer(TAB_ID_ARG, TAB_ID_DESCRIPTION)
            .string_enum("format", "Image format (default png).", &["png", "jpeg"])
            .integer("quality", "JPEG quality 1-100 (default 80).")
            .number("scale", "Downscale factor 0.05-1 relative to device pixels (default 1).")
            .number("x", "Clip left, in viewport CSS px.")
            .number("y", "Clip top, in viewport CSS px.")
            .number("width", "Clip width, in viewport CSS px.")
            .number("height", "Clip height, in viewport CSS px.")
            .build(),
        get_screenshot,
    );
}
