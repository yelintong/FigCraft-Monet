//! 系统剪贴板桥接。
//!
//! 1:1 对照 `ui-egui` 的 `app/clipboard.rs`、`menus/clipboard.rs`、`menus.rs` 的 copy 分支：
//! 复制/剪切把图层镜像写进系统剪贴板；粘贴时「系统剪贴板上更新的内容优先」，
//! 没有更新的内容才用引擎自己的剪贴板。这样跨窗口复制粘贴才成立。
//!
//! 上游对应的改动：#26（图片粘贴 + newest-wins）、#375（写系统剪贴板）、
//! #693/#1723（图片型剪贴板的按键事件）、#2038（截断 PNG 解码兜底）、
//! #647（设计图层 HTML）、#808（文本图层镜像）、#1239（写入收口）。

use crate::app::CanvasApp;
use figcraft_doc::NodeId;
use serde_json::{Value, json};

/// 剪贴板 PNG 的字节上限（与 `ui-egui/src/services.rs` 同一个数）。
pub const MAX_CLIPBOARD_PNG_BYTES: usize = 256 * 1024 * 1024;
/// 粘贴图片的总像素上限（与 `ui-egui/src/menus/clipboard.rs` 同一个数）。
const MAX_PASTED_IMAGE_PIXELS: u64 = 100_000_000;

/// 平台剪贴板。原生实现与 `apps/figcraft/src/services.rs` 同源；`memory()` 只给测试用。
pub struct Clipboard {
    native: Option<arboard::Clipboard>,
    #[cfg(test)]
    test_text: Option<String>,
    #[cfg(test)]
    test_html: Option<String>,
    #[cfg(test)]
    test_image: Option<(u32, u32, Vec<u8>)>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Clipboard {
    pub fn new() -> Self {
        Self {
            native: arboard::Clipboard::new().ok(),
            #[cfg(test)]
            test_text: None,
            #[cfg(test)]
            test_html: None,
            #[cfg(test)]
            test_image: None,
        }
    }

    /// 无平台剪贴板的空实现：单元测试注入内容用。
    #[cfg(test)]
    pub fn memory() -> Self {
        Self { native: None, test_text: None, test_html: None, test_image: None }
    }

    #[cfg(test)]
    pub fn put_text(&mut self, text: &str) {
        self.test_text = Some(text.to_string());
    }

    #[cfg(test)]
    pub fn put_html(&mut self, html: &str) {
        self.test_html = Some(html.to_string());
    }

    #[cfg(test)]
    pub fn put_image(&mut self, width: u32, height: u32, rgba: Vec<u8>) {
        self.test_image = Some((width, height, rgba));
    }

    pub fn set_text(&mut self, text: &str) {
        #[cfg(test)]
        {
            self.test_text = Some(text.to_string());
        }
        if let Some(c) = self.native.as_mut()
            && c.set_text(text).is_err()
        {
            log::warn!("clipboard: couldn't write text");
        }
    }

    pub fn get_text(&mut self) -> Option<String> {
        #[cfg(test)]
        if let Some(t) = &self.test_text {
            return Some(t.clone());
        }
        self.native.as_ref()?;
        // Wayland 的读取没有超时，放到辅助线程上，2 秒后放弃（上游 #693）。
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut c = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(_) => return,
            };
            let _ = tx.send(c.get_text().ok());
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten()
    }

    pub fn get_html(&mut self) -> Option<String> {
        #[cfg(test)]
        if let Some(h) = &self.test_html {
            return Some(h.clone());
        }
        self.native.as_ref()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut c = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(_) => return,
            };
            let _ = tx.send(c.get().html().ok());
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten()
    }

    pub fn set_png(&mut self, width: u32, height: u32, rgba: &[u8]) {
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) * 4 != rgba.len() as u64 {
            return;
        }
        if let Some(c) = self.native.as_mut()
            && c.set_image(arboard::ImageData { width: width as usize, height: height as usize, bytes: std::borrow::Cow::Borrowed(rgba) }).is_err()
        {
            log::warn!("clipboard: couldn't write image");
        }
    }

    /// 系统剪贴板里的图片，作为 (宽, 高, 直 RGBA)。
    pub fn get_image(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        #[cfg(test)]
        if let Some(img) = &self.test_image {
            return Some(img.clone());
        }
        self.native.as_ref()?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut c = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(_) => return,
            };
            let got = c.get_image().ok().and_then(|img| {
                let (w, h) = (u32::try_from(img.width).ok()?, u32::try_from(img.height).ok()?);
                (w != 0 && h != 0 && u64::from(w) * u64::from(h) <= MAX_PASTED_IMAGE_PIXELS && img.bytes.len() == (w as usize) * (h as usize) * 4)
                    .then(|| (w, h, img.bytes.into_owned()))
            });
            let _ = tx.send(got);
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten()
    }

    /// 系统剪贴板里以字节保存的 PNG（macOS 的 public.png）。
    pub fn get_png(&mut self) -> Option<Vec<u8>> {
        let c = self.native.as_mut()?;
        #[cfg(target_os = "macos")]
        {
            use objc2_app_kit::NSPasteboard;
            use objc2_foundation::NSString;
            let board = NSPasteboard::generalPasteboard();
            let data = board.dataForType(&NSString::from_str("public.png"))?;
            (data.len() <= MAX_CLIPBOARD_PNG_BYTES).then(|| data.to_vec())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = c;
            None
        }
    }
}

/// 我们上次写进系统剪贴板的内容；generation 对不上说明引擎的副本已经换了。
#[derive(Default)]
pub struct State {
    pub last_copy: Option<String>,
    pub last_copy_generation: u64,
}

/// 我们放在系统剪贴板上的标记（复制的图层没有文本/SVG 镜像时用）。
fn is_clipboard_marker(text: &str) -> bool {
    text.starts_with("figcraft://clipboard/")
}

/// 命令分派：只有这几条需要 UI 层参与，其余照旧交给引擎。
pub fn invoke(app: &mut CanvasApp, id: &str, params: &Value) -> bool {
    match id {
        // Windows 的 Shift+Delete 在矢量编辑态只发 cut 命令：删点并愈合路径，不镜像图层。
        "edit.cut"
            if app
                .session
                .ds()
                .ok()
                .and_then(|d| d.vector_edit.clone())
                .is_some_and(|ve| ve.parts().iter().any(|p| !p.vertices.is_empty() || !p.segments.is_empty())) =>
        {
            let points =
                app.session.ds().ok().and_then(|d| d.vector_edit.clone()).is_some_and(|ve| ve.parts().iter().any(|p| !p.vertices.is_empty()));
            let cmd = if points { "vector.deleteHeal" } else { "vector.edit.delete" };
            return app.session.execute(cmd, &json!({})).is_ok();
        }
        "edit.copy" | "edit.cut" => {
            let ids = app.session.selection();
            let doc = app.session.doc().ok();
            // 取镜像必须在 cut 删除图层之前（上游 menus.rs 的同一句）。
            let mirror = doc.as_ref().and_then(|d| text_layers_text(d, &ids).or_else(|| figcraft_svg_export(d, &ids)));
            let ok = app.session.execute(id, params).is_ok();
            if !app.control_synthetic {
                let text = mirror.unwrap_or_else(|| format!("figcraft://clipboard/{}", ids.len()));
                app.clipboard.set_text(&text);
                app.clipboard_state.last_copy = Some(text);
                app.clipboard_state.last_copy_generation = app.session.clipboard.layers_generation;
            }
            ok
        }
        "edit.copyAsCss" => {
            let ids = app.session.selection();
            if let (Some(first), Ok(doc)) = (ids.first(), app.session.doc()) {
                let css = figcraft_engine::codegen::code_for(&doc, *first, figcraft_engine::codegen::CodeLang::Css);
                app.clipboard.set_text(&css);
                return true;
            }
            false
        }
        "edit.copyAsSvg" => {
            let ids = app.session.selection();
            if let Some(svg) = app.session.doc().ok().and_then(|d| figcraft_svg_export(&d, &ids)) {
                app.clipboard.set_text(&svg);
                return true;
            }
            false
        }
        "edit.copyAsPng" => {
            let ids = app.session.selection();
            if let Some((mut pix, profile)) =
                app.session.doc().ok().and_then(|d| Some((figcraft_engine::cmd::file::copy_png(&d, &ids)?, d.color_profile)))
            {
                figcraft_render::to_srgb(&mut pix, profile);
                let rgba = figcraft_render::to_rgba8(&pix);
                app.clipboard.set_png(pix.width(), pix.height(), &rgba);
                return true;
            }
            false
        }
        "edit.copyAsText" => {
            let doc = app.session.doc().ok();
            let text: Vec<String> = app
                .session
                .selection()
                .iter()
                .filter_map(|i| doc.as_ref().and_then(|d| d.node(*i)).and_then(|n| n.kind.text().map(|t| t.characters.clone())))
                .collect();
            app.clipboard.set_text(&text.join("\n"));
            true
        }
        "edit.copyLink" => {
            if let Some(id) = app.session.selection().first() {
                app.clipboard.set_text(&format!("figcraft://node/{}", id.0));
                return true;
            }
            false
        }
        "edit.paste" => {
            let at = point_from(params);
            paste(app, at)
        }
        _ => app.session.execute(id, params).is_ok(),
    }
}

fn point_from(params: &Value) -> Option<(f64, f64)> {
    let x = params.get("x").and_then(Value::as_f64)?;
    let y = params.get("y").and_then(Value::as_f64)?;
    Some((x, y))
}

/// 粘贴：系统剪贴板上比我们上次复制更新的内容优先，否则贴引擎自己的副本。
/// `at` 为 Some 时是右键「粘贴到这里」：图片与图层都以触点为左上角。
pub fn paste(app: &mut CanvasApp, at: Option<(f64, f64)>) -> bool {
    // 合成输入（控制通道）只贴引擎自己的副本，绝不读用户的系统剪贴板。
    if app.control_synthetic {
        let params = at.map_or(json!({}), |(x, y)| json!({"x": x, "y": y}));
        return app.session.execute("edit.paste", &params).is_ok();
    }

    // 另一个设计应用复制的图层：HTML 里带着 .fig 数据。
    if let Some(html) = app.clipboard.get_html().filter(|h| figcraft_engine::cmd::edit::design_clipboard_buffer(h).is_some()) {
        let mut params = json!({"html": html});
        if let Some((x, y)) = at {
            params["x"] = json!(x);
            params["y"] = json!(y);
        }
        let ok = app.session.execute("edit.pasteDesignLayers", &params).is_ok();
        if ok {
            // 贴进来的图层现在是我们的副本：以后别人放的文本才算更新。
            app.clipboard_state.last_copy = app.clipboard.get_text();
            app.clipboard_state.last_copy_generation = app.session.clipboard.layers_generation;
        }
        return ok;
    }

    let sys_text = app.clipboard.get_text().filter(|t| !t.is_empty());
    let ours = sys_text.is_some()
        && sys_text == app.clipboard_state.last_copy
        && app.clipboard_state.last_copy_generation == app.session.clipboard.layers_generation;
    let (cx, cy) = at.or_else(|| app.session.ds().ok().map(|d| (d.view.cx, d.view.cy))).unwrap_or((0.0, 0.0));

    if !ours {
        if let Some((png, w, h)) = clipboard_png(app) {
            use base64::Engine as _;
            let b64 = base64::engine::general_purpose::STANDARD.encode(png);
            // 只选中一个能接受填充的图层时，图片作为它的顶层填充。
            if let [target] = app.session.selection()[..]
                && at.is_none()
                && app.session.doc().ok().and_then(|d| d.node(target)).is_some_and(|n| figcraft_engine::cmd::create::accepts_image_fill(&n.kind))
            {
                return app.session.execute("image.place", &json!({"data": b64, "target": target})).is_ok();
            }
            let (x, y) = if at.is_some() { (cx, cy) } else { ((cx - w / 2.0).round(), (cy - h / 2.0).round()) };
            return app.session.execute("file.import", &json!({"data": b64, "name": "Pasted image.png", "x": x, "y": y})).is_ok();
        }
        // 除我们自己的标记外，文本都是别的应用刚放上去的。
        if let Some(text) = sys_text.filter(|t| !is_clipboard_marker(t)) {
            return paste_text(app, &text, (cx, cy), at.is_none());
        }
    }

    let params = at.map_or(json!({}), |(x, y)| json!({"x": x, "y": y}));
    app.session.execute("edit.paste", &params).is_ok()
}

/// 别的应用的文本：评论链接、SVG、画板表格、普通文本层。
fn paste_text(app: &mut CanvasApp, text: &str, (cx, cy): (f64, f64), center: bool) -> bool {
    let board = app.session.doc().is_ok_and(|d| figcraft_engine::board::is_board(&d));
    if text.trim().starts_with("figcraft://comment/") {
        return app.session.execute("comment.open", &json!({"link": text.trim()})).is_ok();
    }
    if let Some((cells, _more)) = board.then(|| figcraft_engine::board_table::spreadsheet_cells(text)).flatten() {
        let mut params = json!({"cells": cells});
        if !center {
            (params["x"], params["y"]) = (json!(cx), json!(cy));
        }
        return app.session.execute("board.table", &params).is_ok();
    }
    if figcraft_engine::looks_like_svg(text) {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        return app.session.execute("file.import", &json!({"data": b64, "name": "Pasted SVG.svg", "x": cx, "y": cy, "center": center})).is_ok();
    }
    app.session.execute("text.create", &json!({"text": text, "x": cx, "y": cy})).is_ok()
}

/// 系统剪贴板里的图片：先读 PNG 字节，再退回 RGBA 编码；尺寸与解码都要检查。
fn clipboard_png(app: &mut CanvasApp) -> Option<(Vec<u8>, f64, f64)> {
    let clipboard_png = app.clipboard.get_png().filter(|png| png.len() <= MAX_CLIPBOARD_PNG_BYTES);
    let (png, (w, h)) = match clipboard_png.and_then(with_pixel_size) {
        Some(readable) => readable,
        None => with_pixel_size(encoded_clipboard_image(app)?)?,
    };
    let density = figcraft_format::pixel_density(&png);
    let (w, h) = figcraft_format::placed_size(w, h, density, figcraft_engine::cmd::create::PLACED_IMAGE_MAX_SIDE);
    Some((png, w, h))
}

fn with_pixel_size(png: Vec<u8>) -> Option<(Vec<u8>, (u32, u32))> {
    use image::ImageDecoder as _;
    let decoder = image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format().ok()?.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PASTED_IMAGE_PIXELS {
        return None;
    }
    image::DynamicImage::from_decoder(decoder).ok()?;
    Some((png, (w, h)))
}

fn encoded_clipboard_image(app: &mut CanvasApp) -> Option<Vec<u8>> {
    let (w, h, rgba) = app.clipboard.get_image()?;
    let img = image::RgbaImage::from_raw(w, h, rgba)?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// 选中的纯文本图层的文字，一行一个（上游 `text_layers_text`）。
fn text_layers_text(doc: &figcraft_doc::Document, ids: &[NodeId]) -> Option<String> {
    let texts: Option<Vec<&str>> = ids.iter().map(|i| doc.node(*i).and_then(|n| n.kind.text()).map(|t| t.characters.as_str())).collect();
    texts.map(|t| t.join("\n")).filter(|t| !t.is_empty())
}

/// 选中图层导出为 SVG 文本（上游 `figcraft_svg_export`）。
fn figcraft_svg_export(doc: &figcraft_doc::Document, ids: &[NodeId]) -> Option<String> {
    let bytes = figcraft_engine::cmd::file::export_bytes(doc, ids, figcraft_doc::ExportFormat::Svg, 1.0).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use figcraft_doc::{Document, Node, NodeKind};

    fn app() -> CanvasApp {
        let mut doc = Document::new("clipboard");
        let page = doc.pages[0];
        let id = doc.alloc_id();
        let rect = doc.insert(Node::new(id, NodeKind::Rectangle, "Rectangle", 100.0, 50.0), page, None).unwrap();
        let mut app = CanvasApp::new(doc);
        app.session.set_selection(vec![rect]);
        app.clipboard = Clipboard::memory();
        app
    }

    #[test]
    fn copy_mirrors_the_layer_to_the_system_clipboard() {
        let mut app = app();
        assert!(invoke(&mut app, "edit.copy", &json!({})));
        let text = app.clipboard.get_text().unwrap();
        assert!(text.contains("<svg") || text.starts_with("figcraft://clipboard/"), "{text}");
        assert_eq!(app.clipboard_state.last_copy.as_deref(), Some(text.as_str()));
        assert_eq!(app.clipboard_state.last_copy_generation, app.session.clipboard.layers_generation);
    }

    #[test]
    fn our_own_clipboard_pastes_the_copied_layers() {
        let mut app = app();
        assert!(invoke(&mut app, "edit.copy", &json!({})));
        let before = app.session.doc().unwrap().node_count();
        assert!(invoke(&mut app, "edit.paste", &json!({})));
        assert!(app.session.doc().unwrap().node_count() > before);
    }

    #[test]
    fn a_newer_image_on_the_system_clipboard_pastes_as_a_layer() {
        let mut app = app();
        assert!(invoke(&mut app, "edit.copy", &json!({})));
        let mut img = image::RgbaImage::new(2, 2);
        for pix in img.pixels_mut() {
            *pix = image::Rgba([255, 0, 0, 255]);
        }
        app.clipboard.put_image(2, 2, img.into_raw());
        app.clipboard.put_text("https://example.com".into());
        // 有选中的矩形时图片会成为它的填充；清空选择才走「新建图片图层」这条路。
        app.session.set_selection(vec![]);
        let before = app.session.doc().unwrap().node_count();
        assert!(invoke(&mut app, "edit.paste", &json!({})));
        let doc = app.session.doc().unwrap();
        assert_eq!(doc.node_count(), before + 1);
        assert!(doc.nodes().any(|n| n.name.contains("Pasted image")));
    }

    #[test]
    fn a_newer_text_on_the_system_clipboard_pastes_as_text() {
        let mut app = app();
        assert!(invoke(&mut app, "edit.copy", &json!({})));
        app.clipboard.put_text("hello from another app".into());
        let before = app.session.doc().unwrap().node_count();
        assert!(invoke(&mut app, "edit.paste", &json!({})));
        let doc = app.session.doc().unwrap();
        assert_eq!(doc.node_count(), before + 1);
        assert!(doc.nodes().any(|n| n.kind.text().is_some_and(|t| t.characters == "hello from another app")));
    }

    #[test]
    fn a_synthetic_copy_does_not_touch_the_system_clipboard() {
        let mut app = app();
        app.clipboard.put_text("user text".into());
        app.control_synthetic = true;
        assert!(invoke(&mut app, "edit.copy", &json!({})));
        assert_eq!(app.clipboard.get_text().as_deref(), Some("user text"));
    }
}
