//! 右栏各节的**真实行控件**，逐节照搬 `ui-egui`：
//! - `paints.rs::paint_row` → paint 列表（`ColorField` 颜色输入框 + 绑定 chip/按钮 + 显隐/删除）
//! - `effects.rs::effects` / `grids` → effect / grid 列表
//! - `selection_colors.rs::section` → 选区颜色组（`ColorField` + 绑定按钮）
//! - `export_panel.rs::setting_row` → 导出设置列表
//! - `paints.rs::variable_modes` / `section_panel.rs::section` / `right.rs::header` / `page_section`
//!
//! 所有写回都通过 `Cmd` 通道（`sync_panel` 统一 `execute`），与 egui 的 `app.run/set_props` 同构。
//! 动态列表的输入框由父模块的**编辑器池**提供（retained tree 不能每帧新建 `Entity`）。

use figcraft_i18n::tr_noop;
use figcraft_i18n::{tr, trf, trn};
use super::data::{EffectRow, ExportRow, LocalStyleRow, PaintRow, PanelData, ViewData};
use crate::canvas::geom::Color32;
use crate::motion::{Act, Queue};
use crate::panels::{Cmd, ROW_EDITORS, RightFields, RowEditCtx, visibility};
use crate::widgets::{
    Checkbox, ColorField, ColorText, Dropdown, HEADER_H, Icon, IconButton, MenuRow, PAD, PanelSection, SECTION_BOTTOM, SectionHeader,
    SectionHeaderVariant, Segmented, TITLE_PT, block_row, field_row, field_row_full, field_row_full_trailing, pt, rgba, sublabel_row, sublabel_row2,
    swatch, tokens, variable_button,
};
use figcraft_color::PaintKind;
use figcraft_doc::NodeId;
use figcraft_doc::anim::motion::AnimProperty;
use gpui::{
    App, Bounds, Div, ImageSource, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, SharedString, Window, canvas, div, img,
    prelude::*, px, size,
};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

// ───────────────────────────────────────────────────────────────────────────────────────────────
// 公共小工具
// ───────────────────────────────────────────────────────────────────────────────────────────────

/// 推一条命令（等同 egui 的 `app.run(cmd, {"ids": ids, …})`）。
fn push(f: &RightFields, id: &'static str, params: Value) {
    f.cmds.push(Cmd { id, params: Some(params) });
}

/// 右键菜单入口：与 `pick_dropdown` 共用 `MenuRow` + 根视图 `open_menu` 这套通用控件，
/// 只是锚点取当前指针而不是触发按钮（`ui-egui/right/menu.rs::context_popup`）。
/// `entries` 是 (显示名, 命令, 参数)；`extra` 由调用方先把「剪贴板里有没有可粘贴项」算进去。
fn context_menu_at(f: &RightFields, at: Point<Pixels>, width: f32, entries: Vec<(String, &'static str, Value)>, cx: &mut App) {
    let Some(root) = f.root.clone() else { return };
    let rows: Vec<MenuRow> = entries.iter().map(|(label, _, _)| MenuRow::new(label.clone())).collect();
    let cmds = f.cmds.clone();
    let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
        if let Some((_, cmd, params)) = entries.iter().find(|(l, _, _)| l == label) {
            cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
        }
    });
    let anchor = Bounds { origin: at, size: size(px(1.0), px(0.0)) };
    root.update(cx, |cv, cx| cv.open_menu(anchor, Some(width), rows, on_pick, cx));
}

/// The header's trailing icon row, with the same right-click menu as egui's section header.
/// Returns a fresh element so callers with two rendering branches do not need a clonable `Div`.
fn context_header_row(
    f: &RightFields,
    icons: Vec<gpui::AnyElement>,
    width: f32,
    entries: Vec<(String, &'static str, Value)>,
) -> gpui::Div {
    let f = f.clone();
    let menu_entries = entries.clone();
    icon_row(icons).on_mouse_down(MouseButton::Right, move |e, _, cx: &mut App| {
        if !menu_entries.is_empty() {
            context_menu_at(&f, e.position, width, menu_entries.clone(), cx);
            cx.stop_propagation();
        }
    })
}

/// 右栏一节（Fill / Stroke / Effects / Layout grid / Export …）的两种呈现。
///
/// **空节只有一行置灰头部**：没有 body、没有提示文字、没有底边距 —— 视觉上就是「这一节
/// 死在那里」，只剩标题和右边那排图标（`+` 仍然可点，功能没丢）。
///
/// 为什么空节不能走 `PanelSection`：它的 body 恒为
/// `div().w_full().pb(pt(SECTION_BOTTOM))`（`widgets/panel_section.rs:234`，该文件 FROZEN
/// 改不了），哪怕一行都没有也会垫出 12 pt 空白 —— 于是空节看着「没内容却多一截」。
/// 这里对空态直接返回裸 `SectionHeader`，天然没有 body。
pub(crate) enum Section {
    /// 有内容的完整节：头部 + body 网格。
    Full(PanelSection),
    /// 空节：只有置灰头部。
    Empty(SectionHeader),
}

impl Section {
    /// 空节：置灰标题 + 与有内容时**同一套** header 图标。
    fn empty(title: impl Into<SharedString>, trail: impl IntoElement) -> Self {
        Section::Empty(SectionHeader::new(title).variant(SectionHeaderVariant::Dim).trail(trail))
    }
}

impl IntoElement for Section {
    type Element = gpui::Div;

    fn into_element(self) -> gpui::Div {
        match self {
            Section::Full(sec) => sec.into_element(),
            // ⛔ 空节必须**钉死**成标准 40 pt，且禁止收缩（`flex_none()`）。
            //
            // 右栏 body 是 `flex_1().min_h(px(0.0)).flex_col().overflow_y_scroll()`
            // （`panels.rs::right_panel`）。`min_h(0)` 关掉了自动最小高度，于是直接塞进
            // body 的裸 `SectionHeader` 会被当成**可收缩的 flex 项**：它只有 `height: 40`、
            // 内容只有一行字，内容型最小尺寸 ≈ 文字行高（十几 pt），内容一长就被压扁。
            // `PanelSection` 那层之所以没事，是因为它没有指定高度、高度由内容撑开，
            // 自动最小高度 = 全部内容 → 收缩不了。这里把外层拿掉了，就得自己补上这个保护。
            Section::Empty(header) => div().flex_none().w_full().h(pt(HEADER_H)).child(header).into_element(),
        }
    }
}

/// 认领一个池槽位；超出时夹到最后一个，保证不会 panic。
pub(crate) fn slot(n: &mut usize) -> usize {
    let s = (*n).min(ROW_EDITORS - 1);
    *n += 1;
    s
}

/// 一个「点开菜单选中即推命令」的下拉（egui `menu_popup` + `menu_item`）。
/// `rows`: (显示名, 选中后推的命令 id, 命令额外参数)。`cmds` 句柄 clone 进 `on_pick`。
pub(crate) fn pick_dropdown(f: &RightFields, label: impl Into<SharedString>, width: f32, rows: Vec<(String, &'static str, Value)>) -> Dropdown {
    let menu: Vec<MenuRow> = rows.iter().map(|(l, _, _)| MenuRow::new(l.clone())).collect();
    let mut d = Dropdown::new(label, width, None).fill();
    if let Some(root) = f.root.clone() {
        let cmds = f.cmds.clone();
        d = d.on_click(move |anchor, _window, cx: &mut App| {
            let rows = rows.clone();
            let cmds = cmds.clone();
            let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
                if let Some((_, cmd, params)) = rows.iter().find(|(l, _, _)| l == label) {
                    cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
                }
            });
            root.update(cx, |cv, cx| cv.open_menu(anchor, Some(width.max(120.0)), menu.clone(), on_pick, cx));
        });
    }
    d
}

/// 字体族选择按钮：外观沿用 `Dropdown`，但点击打开一个带字体预览的窗口（每项用它自己的
/// 字体渲染名称），而不是纯文本菜单。
fn font_picker_button(f: &RightFields, label: impl Into<SharedString>, width: f32, current: Option<String>) -> Dropdown {
    let popups = f.popups();
    let queue = f.queue();
    let mut d = Dropdown::new(label, width, None).fill();
    d = d.on_click(move |anchor: Bounds<Pixels>, _window, cx: &mut App| {
        open_detached_with(popups.clone(), "font-picker", Some(anchor.origin.y), cx, |pos, cx| {
            crate::popovers::WindowState::font_picker(queue.clone(), cx, current.as_deref(), pos)
        });
    });
    d
}

/// Zoom / Show 菜单的一行：显示/隐藏开关带 `menu_check` 的勾选列，分组用分隔线。
#[derive(Clone)]
struct ViewMenuRow {
    label: String,
    cmd: &'static str,
    params: Value,
    checked: Option<bool>,
    separator: bool,
}

impl ViewMenuRow {
    fn plain(label: &str, cmd: &'static str) -> Self {
        Self { label: label.into(), cmd, params: json!({}), checked: None, separator: false }
    }

    fn toggle(label: &str, cmd: &'static str, params: Value, on: bool, separator: bool) -> Self {
        Self { label: label.into(), cmd, params, checked: Some(on), separator }
    }
}

/// 右栏头部的 Zoom 按钮（egui `right.rs` 的 Zoom `menu_popup` + `menu_check`）。
///
/// 与通用 `Dropdown` 不同：这里**宽度由内容决定**（hug，不传固定 width），靠 view_bar 的
/// `justify_between` 推到最右。菜单仍用按钮自身 rect 作锚点（跨帧探针，按下即对位），
/// 勾选行与命令写入和 `checked_dropdown` 完全一致。
fn zoom_button(f: &RightFields, label: String, rows: Vec<ViewMenuRow>) -> gpui::AnyElement {
    let menu: Vec<MenuRow> = rows
        .iter()
        .map(|r| {
            let mut m = MenuRow::new(r.label.clone());
            if r.separator {
                m = m.separator();
            }
            if let Some(on) = r.checked {
                m = m.checked(Some(on));
            }
            m
        })
        .collect();
    let hit = Rc::new(Cell::new(None));
    let hit_click = hit.clone();
    // 文字与 chevron 间距 0（用户：缩放时若留间隙会出问题）。间距靠 icon 自身字形留白。
    let mut el = div()
        .flex()
        .flex_row()
        .items_center()
        .cursor_pointer()
        .text_size(pt(crate::widgets::TEXT_PT))
        .text_color(rgba(tokens().text))
        .child(label)
        .child(crate::widgets::icon::paint(Icon::ChevronDown, rgba(tokens().text_secondary), 12.0));
    if let Some(root) = f.root.clone() {
        let cmds = f.cmds.clone();
        el = el.on_mouse_down(MouseButton::Left, move |_e, _window, cx: &mut App| {
            let anchor = hit_click.get().unwrap_or_else(|| Bounds { origin: gpui::point(px(0.0), px(0.0)), size: size(px(1.0), px(0.0)) });
            let rows = rows.clone();
            let menu = menu.clone();
            let cmds = cmds.clone();
            let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
                if let Some(r) = rows.iter().find(|r| r.label == label) {
                    cmds.push(Cmd { id: r.cmd, params: Some(r.params.clone()) });
                }
            });
            root.update(cx, |cv, cx| cv.open_menu(anchor, Some(200.0), menu, on_pick, cx));
        });
    }
    crate::popovers::with_button_bounds(el, hit).into_any_element()
}

/// 带勾选状态的下拉（egui `right.rs` 的 Zoom `menu_popup` + `menu_check`）。
/// `MenuRow` 自己负责 `✓ ` 前缀与对齐，`on_pick` 按**纯文本**匹配，所以这里只存原始 label。
fn checked_dropdown(f: &RightFields, label: impl Into<SharedString>, width: f32, rows: Vec<ViewMenuRow>) -> Dropdown {
    let menu: Vec<MenuRow> = rows
        .iter()
        .map(|r| {
            let mut m = MenuRow::new(r.label.clone());
            if r.separator {
                m = m.separator();
            }
            if let Some(on) = r.checked {
                m = m.checked(Some(on));
            }
            m
        })
        .collect();
    let mut d = Dropdown::new(label, width, None);
    if let Some(root) = f.root.clone() {
        let cmds = f.cmds.clone();
        d = d.on_click(move |anchor, _window, cx: &mut App| {
            let rows = rows.clone();
            let menu = menu.clone();
            let cmds = cmds.clone();
            let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
                if let Some(r) = rows.iter().find(|r| r.label == label) {
                    cmds.push(Cmd { id: r.cmd, params: Some(r.params.clone()) });
                }
            });
            root.update(cx, |cv, cx| cv.open_menu(anchor, Some(width.max(200.0)), menu, on_pick, cx));
        });
    }
    d
}

/// 右栏顶部工具行（egui `right.rs` 的 tabs row + Zoom 菜单）。
/// 这里只接**有引擎命令**的「显示 / 隐藏」开关；Share / Design / Prototype 是弹窗或纯 UI
/// 状态，没有对应命令，按用户约定继续留白（源码见 `reference/right.rs.txt`）。
fn view_rows(v: &ViewData) -> Vec<ViewMenuRow> {
    let mut rows = Vec::new();
    for (label, cmd) in [
        (tr("Zoom in"), "view.zoomIn"),
        (tr("Zoom out"), "view.zoomOut"),
        (tr("Zoom to fit"), "view.zoomFit"),
        (tr("Zoom to 50%"), "view.zoom50"),
        (tr("Zoom to 100%"), "view.zoom100"),
        (tr("Zoom to 200%"), "view.zoom200"),
        (tr("Zoom to selection"), "view.zoomSelection"),
    ] {
        rows.push(ViewMenuRow::plain(label, cmd));
    }
    // egui `pixel_preview`: `{"on": !on, "scale": scale}`，1x / 2x 各自打勾。
    for scale in [1u8, 2u8] {
        let on = v.pixel_preview && v.pixel_scale == scale;
        rows.push(ViewMenuRow::toggle(&format!("Pixel preview {scale}x"), "view.pixelPreview", json!({ "on": !on, "scale": scale }), on, scale == 1));
    }
    // egui `toggles`：这些命令不传参数，引擎 `flag()` 自己按当前值取反。
    for (label, cmd, on, separator) in [
        (tr("Pixel grid"), "view.pixelGrid", v.pixel_grid, true),
        (tr("Snap to pixel grid"), "view.snapPixel", v.snap_pixel, false),
        (tr("Layout grids"), "view.layoutGrids", v.layout_grids, false),
        (tr("Rulers"), "view.rulers", v.rulers, false),
        (tr("Outlines"), "view.outlines", v.outlines, false),
    ] {
        rows.push(ViewMenuRow::toggle(label, cmd, json!({}), on, separator));
    }
    rows
}

pub(crate) fn view_bar(f: &RightFields, d: &PanelData, motion: bool) -> gpui::Div {
    let t = tokens();
    let zoom_label = format!("{}%", (d.view.zoom * 100.0).round() as i64);
    // Zoom 靠最右，宽度由内容（百分比数字 + chevron）决定，不设定宽。
    let zoom = zoom_button(f, zoom_label, view_rows(&d.view));
    // egui `right.rs` 的 tabs 行（Design / Prototype）：激活的那个上底色 `hover`、圆角 5、
    // 半粗 11 pt；未激活的 `text_secondary`、无底色。整行 40 pt 高、下边框。
    // 纯 UI 状态（`app.ui.right_tab`），不进文档，所以只动 `f.right_tab` 并唤醒一帧。
    let tab = |label: &'static str, idx: u8| -> gpui::Stateful<gpui::Div> {
        // 先取值再进闭包，避免 `borrow()` 与 `borrow_mut()` 同语句重叠。
        let active = *f.right_tab.borrow() == idx;
        let tabs = f.right_tab.clone();
        let cmds = f.cmds.clone();
        div()
            .id(SharedString::from(match idx {
                0 => "rtab-design",
                1 => "rtab-prototype",
                _ => "rtab-inspect",
            }))
            .h(pt(24.0))
            .px(pt(8.0))
            .flex()
            .items_center()
            .rounded(pt(5.0))
            .cursor_pointer()
            .when(active, |d| d.bg(rgba(t.hover)))
            .hover(|s| s.bg(rgba(t.hover)))
            .on_mouse_down(MouseButton::Left, move |_e, _w, _cx| {
                {
                    let mut cur = tabs.borrow_mut();
                    if *cur != idx {
                        *cur = idx;
                    }
                }
                cmds.wake();
            })
            .child(
                div()
                    .text_size(pt(11.0))
                    .font_weight(crate::panels::UI_FONT_REGULAR)
                    .text_color(rgba(if active { t.text } else { t.text_secondary }))
                    .child(label),
            )
    };
    // 2026-11-10 用户要求：动画 / 开发模式下，这一栏不显示 Design / Prototype 两个标签，
    // 原位置直接换成文本——动画写「动画」、开发写「开发」；其余模式仍是可点的 Design / Prototype。
    // 开发模式由右栏标签下标 2（set_right_tab(2)）判定；动画模式由 motion 判定。
    let mode_label = |s: &'static str| -> gpui::Div {
        div()
            .h(pt(24.0))
            .px(pt(8.0))
            .flex()
            .items_center()
            .rounded(pt(5.0))
            .text_size(pt(11.0))
            .font_weight(crate::panels::UI_FONT_REGULAR)
            .text_color(rgba(t.text))
            .child(s)
    };
    // 相邻控件间距统一 4 pt（参考 Frame 38：Design → Prototype = 4 pt）。
    let tabs = if motion {
        div().flex().flex_row().items_center().gap(pt(crate::panels::HEADER_GAP)).child(mode_label("动画"))
    } else if f.right_tab() == 2 {
        div().flex().flex_row().items_center().gap(pt(crate::panels::HEADER_GAP)).child(mode_label("开发"))
    } else {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(pt(crate::panels::HEADER_GAP))
            // 标签文案仍由 naming::right_tab_label 统一提供（a11y / 命名回归测试继续用它）。
            .child(tab(tr(crate::naming::right_tab_label(0)), 0))
            .child(tab(tr(crate::naming::right_tab_label(1)), 1))
    };
    // Zoom 靠最右：tabs 后放弹簧，宽度由 zoom 内容自己决定（用户明确要求）。
    div()
        .flex_none()
        .w_full()
        .h(pt(40.0))
        .flex()
        .flex_row()
        .items_center()
        .px(pt(8.0))
        .border_b_1()
        .border_color(rgba(tokens().border))
        .child(tabs)
        .child(div().flex_1())
        .child(zoom)
}

/// 一个图标按钮，点击推命令（egui `icons::button` + `app.run`）。
pub(crate) fn icon_cmd(icon: Icon, cmd_id: &'static str, params: Value, f: &RightFields, active: bool) -> IconButton {
    let cmds = f.cmds.clone();
    IconButton::new(icon).size(24.0).active(active).on_click(move |_e, _w, _cx| {
        cmds.push(Cmd { id: cmd_id, params: Some(params.clone()) });
    })
}

/// 一个**可绑定变量**的数值字段（`variable_ui.rs::bindable` / `variable_button` 的 GPUI 版）。
///
/// 原版只有三条规则，这里逐条照搬：
/// 1. **不是所有框都能绑**：只有 `variable.bind` 白名单里的字段（`width` / `height` /
///    `opacity` / `cornerRadius` / `gap` / `padding*` / `fontSize` …）才用这个包装；
///    X / Y / Rotation / 矢量点的坐标在原版里是 `widgets::number_field_mixed`（`binding`
///    传 `None`），**永远不出变量按钮**，调用方不要把它们包进来。
/// 2. **未绑定**：字段右端**只在指针停在这一行时**出现 18 pt 的「应用变量」按钮
///    （`variable_button` 第一句就是 `if !ui.rect_contains_pointer(rect) { return false }`），
///    点开的是变量列表窗口。
/// 3. **已绑定**：字段整个换成显示变量名的 chip（点开换绑），行尾悬停出解绑图标
///    （`bound_field`：名字在中间、`Detach` 在尾槽）。
///
/// 调用方注意：右端那个槽在原文里是**两个互斥的东西**（`widgets.rs::number_field_impl` 末尾）。
/// `menu`（sizing 菜单）优先——W/H 在 auto layout / auto layout 子层 / 文本上时右端是 chevron 或
/// `Hug`/`Fill` 字样，此时**不画变量按钮**；只有 `menu` 为空（普通形状的 W/H、opacity、gap…）
/// 才有上面第 2 条的变量按钮。已绑定时是函数最前面的早退分支，整格换 chip，与 auto layout 无关。
pub(crate) fn bindable(f: &RightFields, field: &'static str, bound: Option<&str>, vars: &[String], el: gpui::AnyElement) -> gpui::AnyElement {
    let t = tokens();
    let hovered = *f.bind_hover.borrow() == Some(field);
    let hover = f.bind_hover.clone();
    let root = f.root.clone();

    let inner: gpui::AnyElement = match bound {
        // ── 已绑定：变量名 chip（点开换绑）+ 尾部解绑 ─────────────────────────────────
        Some(name) => {
            let short: SharedString = name.rsplit('/').next().unwrap_or(name).to_string().into();
            let popups = f.popups();
            let queue = f.queue();
            let list: Vec<String> = vars.to_vec();
            let cmds = f.cmds.clone();
            let bind_key = format!("bind-{field}");
            let hit = crate::popovers::window_trigger_top(&bind_key);
            let anchor_key = bind_key.clone();
            let mut chip = div()
                .w_full()
                .h(pt(crate::widgets::FIELD_H))
                .rounded(pt(4.0))
                .bg(rgba(t.hover))
                .border_1()
                .border_color(rgba(t.border))
                .flex()
                .flex_row()
                .items_center()
                .overflow_hidden()
                .pl(pt(8.0))
                .pr(pt(4.0))
                .text_size(pt(crate::widgets::TEXT_PT))
                .text_color(rgba(t.text))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_e, window, cx: &mut App| {
                    let rows =
                        list.iter()
                            .map(|v| (v.clone(), "variable.bind", json!({ "field": field, "variable": v })))
                            .collect::<Vec<(String, &'static str, Value)>>();
                    let top = crate::popovers::anchored_top(&anchor_key, window);
                    open_list_window_at(
                        popups.clone(),
                        queue.clone(),
                        cx,
                        window.mouse_position(),
                        Some(top),
                        bind_key.clone(),
                        ListWindow::Variables,
                        SharedString::from(tr("Variables")),
                        rows,
                    );
                })
                .child(div().flex_1().min_w(px(0.0)).overflow_hidden().child(short));
            if hovered {
                chip = chip.child(
                    div()
                        .size(pt(18.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(pt(4.0))
                        .hover(|s| s.bg(rgba(t.selected_row)))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, move |_e, _w, cx: &mut App| {
                            cx.stop_propagation();
                            cmds.push(Cmd { id: "variable.bind", params: Some(json!({ "field": field, "variable": Value::Null })) });
                        })
                        .child(crate::widgets::icon::paint(Icon::Detach, rgba(t.text_secondary), 12.0)),
                );
            }
            crate::popovers::with_button_bounds(chip, hit)
        }
        // ── 未绑定：字段本身 + 悬停才出现的「应用变量」按钮 ────────────────────────────
        None => {
            let mut cell = div().w_full().h(pt(crate::widgets::FIELD_H)).relative().child(el);
            if hovered {
                let popups = f.popups();
                let queue = f.queue();
                let list: Vec<String> = vars.to_vec();
                let bind_key = format!("bind-{field}");
                let hit = crate::popovers::window_trigger_top(&bind_key);
                let anchor_key = bind_key.clone();
                cell =
                    cell.child(crate::popovers::with_absolute_bounds(
                        div()
                            .absolute()
                            .right(px(0.0))
                            .top(px(0.0))
                            .w(pt(24.0))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_e, window, cx: &mut App| {
                                cx.stop_propagation();
                                let rows = list
                                    .iter()
                                    .map(|v| (v.clone(), "variable.bind", json!({ "field": field, "variable": v })))
                                    .collect::<Vec<(String, &'static str, Value)>>();
                                let top = crate::popovers::anchored_top(&anchor_key, window);
                                open_list_window_at(
                                    popups.clone(),
                                    queue.clone(),
                                    cx,
                                    window.mouse_position(),
                                    Some(top),
                                    bind_key.clone(),
                                    ListWindow::Variables,
                                    SharedString::from(tr("Variables")),
                                    rows,
                                );
                            })
                            .child(crate::widgets::variable_button()),
                        hit,
                    ));
            }
            cell.into_any_element()
        }
    };

    // 悬停态只驱动这一行自己的重绘：`bind_hover` 一变就 notify（与 `text_field.rs` 的
    // `trail_hover` 同一套做法，不用全局状态）。
    div()
        .id(gpui::ElementId::Name(SharedString::from(format!("bind-hover-{field}"))))
        .relative()
        .w_full()
        .h_full()
        .flex()
        .items_center()
        .child(inner)
        .on_hover(move |h: &bool, _w, cx: &mut App| {
            let cur = *hover.borrow();
            let next = if *h {
                Some(field)
            } else if cur == Some(field) {
                None
            } else {
                cur
            };
            if next != cur {
                *hover.borrow_mut() = next;
                if let Some(r) = root.clone() {
                    r.update(cx, |_v, cx| cx.notify());
                }
            }
        })
        .into_any_element()
}

/// 把 sizing 菜单挂到 W/H 字段的 **trail 槽**（`layout_panel.rs::dimensions` 的 sizing 下拉）。
///
/// **原版不是「什么时候都是下拉」**：`dimensions()` 先在 `!(has_al || parent_al || is_text)`
/// 时直接 `return`，sizing 行根本不画；菜单项也只有 `Fixed`、`Hug contents`（`has_al || is_text`）、
/// `Fill container`（`can_fill`）、`Add min/max` / `Remove min and max`（`can_limit`）。
/// 这份条件由 [`super::data::DimensionsData`] 的 `can_hug` / `can_fill` / `can_limit` 带下来，
/// 三者皆否时调用方必须走 [`dim_trail_off`]——W/H 就是纯数值框。
pub(crate) fn dim_trail_menu(
    f: &RightFields,
    cx: &mut App,
    field: &gpui::Entity<crate::widgets::TextField>,
    horizontal: bool,
    dims: &super::data::DimensionsData,
) {
    let cmds = f.cmds.clone();
    let root = f.root.clone();
    let limit_open = f.limit_open.clone();
    let sizing_key: &'static str = if horizontal { "sizingH" } else { "sizingV" };
    let min_key: &'static str = if horizontal { "minWidth" } else { "minHeight" };
    let max_key: &'static str = if horizontal { "maxWidth" } else { "maxHeight" };
    // 尺寸本身：`Add min/max` 用当前 W/H 当初始值（egui `set_props(json!({axis.min: current}))`）。
    let current = if horizontal { dims.w } else { dims.h };
    let (has_min, has_max) = if horizontal { (dims.min_w.is_some(), dims.max_w.is_some()) } else { (dims.min_h.is_some(), dims.max_h.is_some()) };
    let any_limit = has_min || has_max;
    let (can_hug, can_fill, can_limit) = (dims.can_hug, dims.can_fill, dims.can_limit);
    // egui `dim_field`：Fill/Hug 时 trail 显示对应单词，否则一个 chevron。
    let cur = if horizontal { dims.sizing_w } else { dims.sizing_h };
    let trail_slot = match cur {
        Some("fill") => crate::widgets::Slot::Text(SharedString::from(tr("Fill"))),
        Some("hug") => crate::widgets::Slot::Text(SharedString::from(tr("Hug"))),
        _ => crate::widgets::Slot::Icon(Icon::ChevronDown),
    };
    // egui `dim_field`：`dim`（Hug / Fill，尺寸由布局算出来）时数值以 50% 墨色读，普通 Fixed 全色。
    let dim_value = matches!(cur, Some("fill") | Some("hug"));
    field.update(cx, move |tf, _| {
        tf.set_dim(dim_value);
        tf.set_trail(trail_slot);
        let cmds = cmds.clone();
        let root = root.clone();
        let limit_open = limit_open.clone();
        tf.set_on_trail(move |pos: Point<Pixels>, _w, cx: &mut App| {
            let Some(root) = root.clone() else { return };
            // 菜单项照 egui `dimensions`：自己能不能 Hug / Fill / 加限值决定列哪些。
            let mut rows: Vec<MenuRow> = vec![MenuRow::new(tr("Fixed"))];
            if can_hug {
                rows.push(MenuRow::new(tr("Hug contents")));
            }
            if can_fill {
                rows.push(MenuRow::new(tr("Fill container")));
            }
            if can_limit {
                let mut grouped = false;
                if !has_min {
                    rows.push(MenuRow::new(tr("Add min")).separator());
                    grouped = true;
                }
                if !has_max {
                    let row = MenuRow::new(tr("Add max"));
                    rows.push(if grouped { row } else { row.separator() });
                }
                if any_limit {
                    rows.push(MenuRow::new(tr("Remove min and max")).separator());
                }
            }
            let cmds = cmds.clone();
            let limit_open = limit_open.clone();
            let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
                let mut push = |cmd: &'static str, params: Value| {
                    cmds.push(Cmd { id: cmd, params: Some(params) });
                };
                match label {
                    "Fixed" => push("node.set", json!({ "props": { sizing_key: "fixed" } })),
                    "Hug contents" => push("node.set", json!({ "props": { sizing_key: "hug" } })),
                    "Fill container" => push("node.set", json!({ "props": { sizing_key: "fill" } })),
                    "Add min" => {
                        // CodeRabbit PR#11: mixed-width selection has `current == None`; writing 0.0
                        // would pin an explicit 0 on every layer. Only open the input row for an
                        // explicit value instead.
                        if let Some(c) = current {
                            push("node.set", json!({ "props": { min_key: c } }));
                        }
                        limit_open.borrow_mut()[if horizontal { 0 } else { 2 }] = true;
                    }
                    "Add max" => {
                        if let Some(c) = current {
                            push("node.set", json!({ "props": { max_key: c } }));
                        }
                        limit_open.borrow_mut()[if horizontal { 1 } else { 3 }] = true;
                    }
                    "Remove min and max" => {
                        push("node.set", json!({ "props": { min_key: Value::Null, max_key: Value::Null } }));
                        let (a, b) = if horizontal { (0, 1) } else { (2, 3) };
                        limit_open.borrow_mut()[a] = false;
                        limit_open.borrow_mut()[b] = false;
                    }
                    _ => {}
                }
                // 仅翻转 `limit_open` 的项没有推命令时也要重绘，否则 min/max 行要等下一次重绘。
                cmds.wake();
            });
            let anchor = Bounds { origin: pos, size: size(px(0.0), px(0.0)) };
            root.update(cx, |cv, cx| cv.open_menu(anchor, Some(200.0), rows, on_pick, cx));
        });
    });
}

/// 摘掉 W/H 的 sizing 菜单（`layout_panel.rs::dimensions` 提前 `return` 的那条路）。
///
/// 图层既不能 Hug 也不能 Fill（普通矩形 / 椭圆 / 分组）时，W/H 在原版里就是两个**纯数值框**：
/// 没有 chevron、没有 `Hug`/`Fill` 文字、点右端不弹菜单。
pub(crate) fn dim_trail_off(cx: &mut App, field: &gpui::Entity<crate::widgets::TextField>) {
    field.update(cx, |tf, _| {
        tf.set_dim(false);
        tf.clear_trail();
    });
}

/// 一个图标按钮，点击打开菜单，选中项推对应命令（egui `icons::button` + `menu_popup`）。
fn menu_icon(f: &RightFields, icon: Icon, width: f32, rows: Vec<(String, &'static str, Value)>, active: bool) -> IconButton {
    let menu: Vec<MenuRow> = rows.iter().map(|(l, _, _)| MenuRow::new(l.clone())).collect();
    let cmds = f.cmds.clone();
    let root = f.root.clone();
    IconButton::new(icon).size(24.0).active(active).on_click(move |_e, window, cx: &mut App| {
        let Some(root) = root.clone() else { return };
        let p = window.mouse_position();
        let anchor = Bounds { origin: p, size: size(px(0.0), px(0.0)) };
        let rows = rows.clone();
        let cmds = cmds.clone();
        let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
            if let Some((_, cmd, params)) = rows.iter().find(|(l, _, _)| l == label) {
                cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
            }
        });
        root.update(cx, |cv, cx| cv.open_menu(anchor, Some(width.max(160.0)), menu.clone(), on_pick, cx));
    })
}

/// 带勾选 / 单选列的菜单按钮：`MenuRow::checked` 是通用的「勾选列」（egui `ui.selectable_label`）。
/// `rows` = (标签, 勾选态, 命令, 参数, 是否在此行前画分隔线)。行用下标当 key，保证同一命令的
/// 多行（size / 单选组）不会互相串参数。
fn checked_menu_icon(
    f: &RightFields,
    icon: Icon,
    width: f32,
    rows: Vec<(String, Option<bool>, &'static str, Value, bool)>,
    active: bool,
) -> IconButton {
    let menu: Vec<MenuRow> = rows
        .iter()
        .enumerate()
        .map(|(i, (label, on, _, _, sep))| {
            let row = MenuRow::new(label.clone()).key(i.to_string()).checked(*on);
            if *sep { row.separator() } else { row }
        })
        .collect();
    let cmds = f.cmds.clone();
    let root = f.root.clone();
    IconButton::new(icon).size(24.0).active(active).on_click(move |_e, window, cx: &mut App| {
        let Some(root) = root.clone() else { return };
        let p = window.mouse_position();
        let anchor = Bounds { origin: p, size: size(px(0.0), px(0.0)) };
        let menu = menu.clone();
        let rows = rows.clone();
        let cmds = cmds.clone();
        let on_pick = Rc::new(move |key: &str, _w: &mut Window, _cx: &mut App| {
            if let Some((_, _, cmd, params, _)) = key.parse::<usize>().ok().and_then(|i| rows.get(i)) {
                cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
            }
        });
        root.update(cx, |cv, cx| cv.open_menu(anchor, Some(width.max(160.0)), menu, on_pick, cx));
    })
}

/// 整行描边按钮（egui `wide_button`）：Design 区底部导出与页面导出共用。
/// 用户 2026-11-19 明确允许这一颗按钮与预览自己写，其余控件必须复用通用组件。
fn wide_button(f: &RightFields, label: SharedString, cmd: &'static str, params: Value) -> Div {
    let t = tokens();
    let cmds = f.cmds.clone();
    div()
        .w_full()
        .h(px(crate::widgets::FIELD_H))
        .rounded(px(crate::widgets::RADIUS))
        .border_1()
        .border_color(rgba(t.border_strong))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_size(pt(11.0))
        .text_color(rgba(t.text))
        .hover(|s| s.bg(rgba(t.hover)))
        .child(label)
        .on_mouse_down(MouseButton::Left, move |_e, _w, _cx| {
            cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
        })
}

/// 右栏所有「打开 detached 小窗」的 24pt 图标按钮的**唯一通用封装**（egui 侧的
/// `icons::button` + `Popup::from_toggle_button_response`）。
///
/// 三件事必须成组，缺一件就复现「第一次对位正确、第二次鼠标移出按钮后整个窗口往下跑」：
/// 1. **稳定 id**：按钮必须带 `trigger` 派生的 id。`IconButton` 不给 id 时用帧内序号
///    （`next_stable_id`），开窗改变元素树后序号漂移 → GPUI 把按钮当新元素重挂 → 探针量到
///    新位置；自动布局的 dots 一直带 `.id("layout-settings-dots")` 所以从不出问题。
/// 2. **跨帧探针**：`with_window_trigger(trigger)` 把按钮真实顶部写进按 trigger 稳定的 cell。
/// 3. **按下即对位**：`anchored_top(trigger)` 读同一个 cell 把卡片顶部对到按钮顶部。
///
/// 所有右栏开窗按钮都走这里 —— 与 eGUI 一样，定位能力全项目只有这一份。
pub(crate) fn window_open_button(
    trigger: &str,
    icon: Icon,
    active: bool,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> gpui::AnyElement {
    crate::popovers::with_window_trigger(
        IconButton::new(icon)
            .id(SharedString::from(format!("window-open-{trigger}")))
            .size(24.0)
            .active(active)
            .on_click(move |_e, window, cx| on_click(window, cx)),
        trigger,
    )
}

/// 右栏 detached 窗口的**唯一**打开入口。
///
/// 所有 24×24 按钮 / 色块 / 下拉都调它，行为因此只有一套：
/// - `trigger` 唯一标识触发按钮 → 选中态（`is_open`）与「再点一次关闭」都按它；
/// - `top` 是触发控件顶部的窗口坐标（探针 `button_top` 或 Dropdown 的 `bounds.origin.y`）；
/// - 内部统一 `anchor_to_button_top` + `toggle`，调用方不再自己拼这两步。
///
/// 传 `x = 0` 即可：`anchored_open` 会把 x 改成 Design 面板左缘。
pub(crate) fn open_detached_with(
    popups: crate::panels::PopupReqs,
    trigger: impl Into<String>,
    top: Option<Pixels>,
    cx: &mut App,
    build: impl FnOnce(Point<Pixels>, &mut App) -> crate::popovers::WindowState,
) {
    let anchor_y = top.unwrap_or_else(|| px(0.0));
    let mut w = build(Point { x: px(0.0), y: anchor_y }, cx);
    if let Some(top) = top {
        w.anchor_to_button_top(top);
    }
    popups.toggle(trigger, w);
}

/// [`open_detached_with`] 的便捷版：直接从右栏字段取弹窗句柄。
pub(crate) fn open_detached(
    f: &RightFields,
    trigger: impl Into<String>,
    top: Option<Pixels>,
    cx: &mut App,
    build: impl FnOnce(Point<Pixels>, &mut App) -> crate::popovers::WindowState,
) {
    open_detached_with(f.popups(), trigger, top, cx, build);
}

/// 变量 / 文本样式这类「列表选择」弹窗：统一用 `widgets::popover` 的通用窗口承载，
/// 不再退化成一个小下拉菜单（egui 里它们本来就是带标题栏的 detached panel）。
#[derive(Clone, Copy)]
enum ListWindow {
    Variables,
    /// 样式窗口：携带 slot 类型（`text`/`paint`/`effect`/`grid`），决定标题栏「新建样式」
    /// 推的 `style.create {type}` 与默认名（egui `StyleSlot::create_type/default_name`）。
    Styles(&'static str),
}

fn open_list_window(
    popups: crate::panels::PopupReqs,
    queue: crate::panels::Cmds,
    cx: &mut App,
    anchor: Point<Pixels>,
    trigger: String,
    kind: ListWindow,
    title: SharedString,
    rows: Vec<(String, &'static str, Value)>,
) {
    open_list_window_at(popups, queue, cx, anchor, None, trigger, kind, title, rows);
}

/// Same as [`open_list_window`] with the trigger's exact top (when it reported one).
fn open_list_window_at(
    popups: crate::panels::PopupReqs,
    queue: crate::panels::Cmds,
    cx: &mut App,
    anchor: Point<Pixels>,
    exact_top: Option<Pixels>,
    trigger: String,
    kind: ListWindow,
    title: SharedString,
    rows: Vec<(String, &'static str, Value)>,
) {
    let trail = match kind {
        ListWindow::Variables => tr("Variable"),
        ListWindow::Styles(_) => tr("Style"),
    };
    let list: Vec<crate::popovers::Row> =
        rows.into_iter().map(|(label, cmd, params)| crate::popovers::Row::new(label, cmd, params).group(tr("This file")).trail(trail)).collect();
    // 统一入口：精确顶部 + 选中态 + 再点关闭全部由 `open_detached_with` 处理。
    // 没有精确顶部时用传入 anchor 的 y，否则 `open_detached_with` 会把窗口放到视口顶
    // （选中色绑定点 y=500 却弹在 y=0，CodeRabbit PR#28）。measured-top 路径不变。
    let top = exact_top.or(Some(anchor.y));
    open_detached_with(popups, trigger, top, cx, |anchor, cx| match kind {
        ListWindow::Variables => crate::popovers::WindowState::variables(queue, cx, anchor, list),
        // 四类样式窗口（Color/Effect/Grid/Text）是**同一类**弹窗（egui `style_picker::open`），
        // 只有 slot 类型不同；它决定标题栏「新建样式」的 `style.create {type}` 与默认名。
        ListWindow::Styles(ty) => crate::popovers::WindowState::styles(queue, cx, anchor, title, ty, list),
    });
}

/// A header icon that opens the detached Variables window (egui `variable_ui::picker`) instead
/// of a little dropdown. These are the visible buttons in the Appearance / Typography headers —
/// wiring the window only to the small in-row binding button left them looking unconnected.
fn variables_window_button(f: &RightFields, trigger: &'static str, icon: Icon, rows: Vec<(String, &'static str, Value)>) -> gpui::AnyElement {
    let root = f.popups();
    let queue = f.queue();
    let active = root.is_open(trigger);
    window_open_button(trigger, icon, active, move |window, cx| {
        let top = crate::popovers::anchored_top(trigger, window);
        open_list_window_at(
            root.clone(),
            queue.clone(),
            cx,
            window.mouse_position(),
            Some(top),
            trigger.to_string(),
            ListWindow::Variables,
            SharedString::from(tr("Variables")),
            rows.clone(),
        );
    })
}

/// 一行的 24 pt 图标按钮组（header 的 trailing 槽）。
pub(crate) fn icon_row<E: gpui::IntoElement>(items: Vec<E>) -> gpui::Div {
    let mut d = div().flex().flex_row().items_center().gap(pt(4.0));
    for i in items {
        d = d.child(i);
    }
    d
}

/// 某类样式的「Styles」菜单（egui `style_picker::open` 的等价：列出该类样式并 `style.apply`）。
fn style_menu(f: &RightFields, d: &PanelData, ty: &'static str, target: Option<&'static str>, icon: Icon) -> gpui::AnyElement {
    // egui `style_picker::apply`: a fill/stroke slot also sends `target` (`"fill"` / `"stroke"`),
    // so the engine knows which paint list the style applies to. Text/effect/grid slots do not.
    let rows: Vec<(String, &'static str, Value)> = d
        .styles
        .iter()
        .filter(|s| s.ty == ty)
        .map(|s| {
            let mut q = json!({ "style": s.id });
            if let Some(t) = target {
                q["target"] = json!(t);
            }
            (s.name.clone(), "style.apply", q)
        })
        .collect();
    // egui 的 style_picker 是一个带标题栏的 detached panel（不是下拉菜单）：
    // 这里用同一个通用窗口恢复它，标题栏的插槽里放「新建样式」按钮。
    let title = match ty {
        "text" => tr("Text styles"),
        "paint" => tr("Color styles"),
        "effect" => tr("Effect styles"),
        "grid" => tr("Grid styles"),
        _ => tr("Styles"),
    };
    let root = f.popups();
    let queue = f.queue();
    // Fill / Stroke 两个样式按钮不能共用 trigger，否则点 Stroke 只会关掉已开的 Fill 窗口、
    // 两个按钮还同时亮（CodeRabbit PR#28）。paint 菜单的 trigger 带上 target 区分。
    let trigger = match target {
        Some(t) => format!("styles-{ty}-{t}"),
        None => format!("styles-{ty}"),
    };
    let active = root.is_open(&trigger);
    let btn_key = trigger.clone();
    window_open_button(&btn_key, icon, active, move |window, cx| {
        let top = crate::popovers::anchored_top(&trigger, window);
        open_list_window_at(
            root.clone(),
            queue.clone(),
            cx,
            window.mouse_position(),
            Some(top),
            trigger.clone(),
            ListWindow::Styles(ty),
            SharedString::from(title),
            rows.clone(),
        );
    })
}

/// egui `style_picker::chip`：已链接共享样式时，在节的**标题下面**多画一行
/// 「类型图标 + 样式名 + Detach」。`None` = 该 slot 没链接样式 → 整行不画
/// （egui 里是 `_ => return`，不占位）。
pub(crate) fn style_chip(f: &RightFields, name: Option<&String>, slot: &'static str) -> Option<crate::widgets::panel_section::FieldRow> {
    let name = name?;
    let t = tokens();
    let cmds = f.cmds.clone();
    let body = div()
        .w_full()
        .h_full()
        .flex()
        .flex_row()
        .items_center()
        .gap(pt(8.0))
        .child(div().flex_none().w(pt(14.0)).h(pt(14.0)).flex().items_center().justify_center().child(crate::widgets::icon::paint(
            Icon::Dot,
            rgba(t.text_secondary),
            14.0,
        )))
        .child(div().flex_1().min_w(px(0.0)).overflow_hidden().text_size(pt(11.0)).text_color(rgba(t.text)).child(SharedString::from(name.clone())));
    let detach = IconButton::new(Icon::Detach).size(24.0).on_click(move |_e, _w, _cx| {
        cmds.push(Cmd { id: "style.detach", params: Some(json!({ "which": slot })) });
    });
    Some(field_row_full_trailing(body, vec![detach]))
}

/// `Field-color / Disabled`（= 变量绑定态）的**全部**绑定行为都收在 `ColorField` 里：
/// 整框点击 → 打开变量菜单换绑；尾插槽那个 24 × 24 按钮 → 解绑（原先是 "%" 单元）。
/// 调用方只给解绑要推的命令，不再在行里另挂一个绑定/解绑 chip。
/// `selection_targets = Some(..)` 时（选区颜色组）换绑推 `selection.bindColorVariable`，
/// 解绑推 `selection.detachColor` —— egui `selection_colors.rs` 用的是这一组专用命令。
fn bound_color_field(
    f: &RightFields,
    cf: ColorField,
    name: &str,
    field: &str,
    vars: &[String],
    selection_targets: Option<Value>,
    detach: (&'static str, Value),
) -> ColorField {
    let root = f.popups();
    let queue = f.queue();
    let vars = vars.to_vec();
    let field = field.to_string();
    let cmds = f.cmds.clone();
    cf.bound(name.to_string(), move |pos, _window, cx| {
        let rows = vars
            .iter()
            .map(|v| {
                if let Some(targets) = &selection_targets {
                    (v.clone(), "selection.bindColorVariable", json!({ "targets": targets, "variable": v }))
                } else {
                    (v.clone(), "variable.bind", json!({ "field": field, "variable": v }))
                }
            })
            .collect();
        open_list_window(
            root.clone(),
            queue.clone(),
            cx,
            pos,
            format!("bind-{field}"),
            ListWindow::Variables,
            SharedString::from(tr("Variables")),
            rows,
        );
    })
    .on_detach(move |_window, _cx| {
        cmds.push(Cmd { id: detach.0, params: Some(detach.1.clone()) });
    })
}

/// Selection colors 的绑定按钮（`selection_colors.rs::section`）：未绑定时一个 Styles 菜单
/// 列出颜色样式（`selection.applyColorStyle`）与颜色变量（`selection.bindColorVariable`）；
/// 已绑定时解绑按钮在 `ColorField` 的 24 pt 尾插槽里，这里保留一个空的 24 占位，
/// 让绑定/未绑定两种行的两个 24 槽严格同位（egui 的这一格本来就是图标列）。
fn selection_bind(f: &RightFields, row: &super::data::SelectionColorRow, d: &PanelData) -> gpui::Div {
    let t = tokens();
    let targets = row.targets.clone();
    if row.linked.is_some() {
        return div().size(pt(24.0));
    }
    let mut rows: Vec<(String, &'static str, Value)> = d
        .styles
        .iter()
        .filter(|s| s.ty == "paint")
        .map(|s| (s.name.clone(), "selection.applyColorStyle", json!({ "targets": targets, "style": s.id })))
        .collect();
    // egui `section`: 只列作用域内可绑定的变量，并提交变量 **id**（不是显示名）。
    rows.extend(row.variables.iter().map(|(name, id)| (name.clone(), "selection.bindColorVariable", json!({ "targets": targets, "variable": id }))));
    let root = f.popups();
    let queue = f.queue();
    div()
        .size(pt(24.0))
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(rgba(t.hover)))
        .child(variable_button())
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_e, window, cx: &mut App| {
            open_list_window(
                root.clone(),
                queue.clone(),
                cx,
                window.mouse_position(),
                format!("sel-bind-{targets}"),
                ListWindow::Variables,
                SharedString::from(tr("Variables")),
                rows.clone(),
            );
        })
}

/// 颜色行的 swatch 点击 → 打开取色器（egui `toggle_picker`）。选色写进 `key` 后推 `cmd`。
/// `fill.set`/`stroke.set`/`page.setBackground` 用 `color`；`selection.setPaint` 必须提交
/// **完整 Paint**：`base_paint = Some(group.paint)` 时只把 `color` 换掉，保留 opacity / blend /
/// visible，否则一个 `"#RRGGBB"` 会把整组的透明度重置成 1。
fn color_pick(
    f: &RightFields,
    cf: ColorField,
    current: Color32,
    cmd: &'static str,
    params: Value,
    key: &'static str,
    base_paint: Option<Value>,
    trigger: String,
    styles: Vec<(String, &'static str, Value)>,
) -> (ColorField, Rc<Cell<Option<Bounds<Pixels>>>>) {
    let popups = f.popups();
    let cmds = f.cmds.clone();
    // 打开取色器时带上当前选区的 AA 对比上下文（背景色 + 文字判定）。
    let contrast = f.contrast();
    // 跨帧稳定探针：色块每帧重建，cell 必须按 trigger 存活，否则点下去那一帧量不到顶部。
    let hit = crate::popovers::window_trigger_top(&trigger);
    let anchor_key = trigger.clone();
    let cf = cf.on_swatch(move |pos: Point<Pixels>, window, cx: &mut App| {
        let top = crate::popovers::anchored_top(&anchor_key, window);
        let style_rows: Vec<crate::popovers::Row> =
            styles.iter().map(|(label, cmd, params)| crate::popovers::Row::new(label.clone(), cmd, params.clone()).group(tr("This file"))).collect();
        let on_pick = Rc::new({
            let cmds = cmds.clone();
            let params = params.clone();
            let base_paint = base_paint.clone();
            move |c: Color32, _w: &mut Window, _cx: &mut App| {
                let mut p = params.clone();
                if let Some(base) = &base_paint {
                    let mut paint = base.clone();
                    paint["color"] = json!(c.to_hex());
                    p["paint"] = paint;
                } else {
                    p[key] = json!(c.to_hex());
                }
                cmds.push(Cmd { id: cmd, params: Some(p) });
            }
        });
        // The paint the row edits, so the picker can switch its kind / blend in place.
        let picker = base_paint.as_ref().and_then(|v| serde_json::from_value::<figcraft_color::Paint>(v.clone()).ok()).map(|paint| {
            let write_cmds = cmds.clone();
            let write_params = params.clone();
            crate::popovers::PaintPicker {
                paint,
                on_write: Rc::new(move |np, _w, _cx| {
                    let mut p = write_params.clone();
                    p["paint"] = serde_json::to_value(&np).unwrap_or(Value::Null);
                    write_cmds.push(Cmd { id: cmd, params: Some(p) });
                }),
                styles: style_rows.clone(),
            }
        });
        // Synchronous request, no detached async `spawn` and no root-Entity guard: the old
        // `cx.spawn(...).detach()` delayed the picker by a frame and could be dropped entirely.
        open_detached_with(popups.clone(), trigger.clone(), Some(top), cx, |anchor, cx| {
            crate::popovers::WindowState::color(cx, anchor, current, picker, contrast.clone(), on_pick, cmds.clone())
        });
    });
    (cf, hit)
}

/// paint 行的 swatch（`paints.rs::paint_row`）：选色推 `fill.set`/`stroke.set`，带 `index`。
fn paint_swatch(
    f: &RightFields,
    row: &PaintRow,
    cf: ColorField,
    styles: Vec<(String, &'static str, Value)>,
) -> (ColorField, Rc<Cell<Option<Bounds<Pixels>>>>) {
    let current = row.paint.solid_color().map(crate::widgets::swatch::color32).unwrap_or(Color32::BLACK);
    let cmd = if row.fills { "fill.set" } else { "stroke.set" };
    // Fill / Stroke 同 index 的两行不能共用取色器 trigger，否则点开一个会关掉另一个
    // （CodeRabbit PR#28）。trigger 里带上 paint 侧。
    let side = if row.fills { "fill" } else { "stroke" };
    // 必须把整份 paint 交给取色器，否则 `PaintPicker` 为 None，类型条（纯色/渐变/图案/图片）
    // 和渐变色阶整块都不会渲染（EGUI 版同样依赖 paint 本体判断类型）。
    let base = serde_json::to_value(&row.paint).ok();
    color_pick(f, cf, current, cmd, json!({ "index": row.index }), "color", base, format!("color-{side}-{}", row.index), styles)
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Fill / Stroke —— `paints.rs::paints` + `paint_row`
// ───────────────────────────────────────────────────────────────────────────────────────────────

/// paint 列表节。`ColorField` 是「颜色输入框」，它自己的 24 pt 尾插槽同时是绑定/解绑按钮；
/// 行尾两个 24 pt 槽固定给显隐与删除，不再另挂绑定 chip。
pub(crate) fn paints(
    f: &RightFields,
    d: &PanelData,
    fills: bool,
    has_vector: bool,
    row_w: f32,
    col: f32,
    n: &mut usize,
    _window: &mut Window,
    cx: &mut App,
    // Motion mode: when `Some`, this is the layer whose first solid paint keys its colour at the
    // playhead (egui `paints.rs` passes `keyframe: Some(layer)`). The Design panel passes `None`.
    // `keyed` is the precomputed keyframe-at-playhead state for that paint's colour property.
    keyframe_layer: Option<NodeId>,
    keyed: bool,
    motion_q: &Queue,
) -> Section {
    let title = if fills { tr("Fill") } else { tr("Stroke") };
    let add_cmd = if fills { "fill.add" } else { "stroke.add" };
    let list = if fills { d.fills.clone() } else { d.strokes.clone() };
    // Header icons（egui `paints.rs::paints` 的 `[Styles, (FillRule), Plus]`）：
    // Styles → 样式菜单；FillRule → 填充规则；Plus → 添加。
    // egui: `let vector = if fills { first_vector(doc, sel) } else { None };
    //        if vector.is_some() { buttons.push(FillRule) }` —— 只有选中矢量网络才画。
    // 此前这里 `if fills` 无条件画，普通矩形/文本也顶着一个无效的填充规则按钮。
    let mut icons = vec![style_menu(f, d, "paint", Some(if fills { "fill" } else { "stroke" }), Icon::Dot)];
    if visibility::fill_rule(fills, has_vector) {
        icons.push(
            menu_icon(
                f,
                Icon::ViewGrid,
                160.0,
                vec![
                    ("Non-zero".into(), "vector.fillRule", json!({ "rule": "nonZero" })),
                    ("Even-odd".into(), "vector.fillRule", json!({ "rule": "evenOdd" })),
                ],
                false,
            )
            .into_any_element(),
        );
    }
    icons.push(icon_cmd(Icon::Plus, add_cmd, json!({}), f, false).into_any_element());
    // EGUI `paints.rs`: the section header's context menu pastes a paint of the same kind.
    let paste_cmd: &'static str = if fills { "fill.paste" } else { "stroke.paste" };
    let can_paste_header = if fills { d.can_paste_fill } else { d.can_paste_stroke };
    let header_entries: Vec<(String, &'static str, Value)> = if can_paste_header {
        vec![(
            if fills { tr("Paste fill") } else { tr("Paste stroke") }.to_string(),
            paste_cmd,
            json!({}),
        )]
    } else {
        Vec::new()
    };
    // 空 / Mixed（多选混合，`list == None`）：**只有置灰头部**——不写提示文字、不给 body、
    // 也就没有 `PanelSection` 那 12 pt 底边距。`+` 图标照旧可点，功能没少。
    let Some(rows) = list.filter(|v| !v.is_empty()) else {
        return Section::empty(title, context_header_row(f, icons, 160.0, header_entries));
    };
    let mut sec = PanelSection::new(title).header_trail(context_header_row(f, icons, 160.0, header_entries));
    // egui `paints.rs`：header 之后、paint 行之前先画链接样式的 chip（`style_picker::chip`）。
    if let Some(chip) = style_chip(f, d.style_chips[if fills { 0 } else { 1 }].as_ref(), if fills { "fill" } else { "stroke" }) {
        sec = sec.child(chip);
    }
    // egui 倒序画（最上层 paint 在最前）。只有**第一道实心 paint** 的 colour 可关键帧化
    // （egui `paints.rs` 的 `first_solid`）。Motion 模式下那一行行尾多出一个菱形按钮。
    let first_solid = rows.iter().position(|r| matches!(r.paint.kind, PaintKind::Solid { .. }));
    // 该 paint 槽可应用的样式（取色器头部「+」弹出的样式面板用）。
    let paint_styles: Vec<(String, &'static str, Value)> = d
        .styles
        .iter()
        .filter(|s| s.ty == "paint")
        .map(|s| (s.name.clone(), "style.apply", json!({ "style": s.id, "target": if fills { "fill" } else { "stroke" } })))
        .collect();
    for (idx, row) in rows.iter().enumerate().rev() {
        let cmd = if fills { "fill.set" } else { "stroke.set" };
        let index = row.index;
        let hex_slot = slot(n);
        let op_slot = slot(n);
        let hex_ed = f.row_field(
            cx,
            hex_slot,
            &row.text,
            RowEditCtx {
                cmd,
                params: json!({ "index": index }),
                key: "color",
                scale: 1.0,
                string_value: true,
                paint: None,
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        let op_ed = f.row_field(
            cx,
            op_slot,
            &format!("{}", row.opacity.round() as i64),
            RowEditCtx {
                cmd,
                params: json!({ "index": index }),
                key: "opacity",
                scale: 100.0,
                string_value: false,
                paint: None,
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        let text = if row.text_is_hex {
            ColorText::Hex(SharedString::from(row.text.clone()))
        } else {
            ColorText::Label(SharedString::from(row.text.clone()))
        };
        // 颜色输入框跨两列（w_full），行尾只有两个 24 pt 槽，同 egui `paint_row` 的
        // `available_width - 56` + `icon_pair`。绑定/解绑一律由 ColorField 自己的尾插槽承担。
        let cf = ColorField::new(cx, row.paint.clone(), text, row_w, hex_ed, op_ed);
        let (cf, swatch_hit) = paint_swatch(f, row, cf, paint_styles.clone());
        // 变量绑定态 = 设计稿 `Field-color / Disabled`：无背景、整框可点换绑、透明度数值消失，
        // 24 pt 尾插槽换成解绑按钮；未绑定态该插槽仍是 100 / "%"。
        let cf = match row.bound.as_deref() {
            Some(name) => bound_color_field(
                f,
                cf,
                name,
                &row.bind_field,
                &row.variables,
                None,
                ("variable.bind", json!({ "field": row.bind_field.clone(), "variable": Value::Null })),
            ),
            None => cf,
        };
        // EGUI `paints.rs::paint_row`: right-clicking anywhere on a fill/stroke row offers Copy
        // that paint and — only when the clipboard holds the same kind — Paste at this index.
        let copy_cmd: &'static str = if fills { "fill.copy" } else { "stroke.copy" };
        let paste_cmd: &'static str = if fills { "fill.paste" } else { "stroke.paste" };
        let copy_label = if fills { tr("Copy fill") } else { tr("Copy stroke") };
        let paste_label = if fills { tr("Paste fill") } else { tr("Paste stroke") };
        let mut entries = vec![(copy_label.to_string(), copy_cmd, json!({ "index": index }))];
        let can_paste = if fills { d.can_paste_fill } else { d.can_paste_stroke };
        if can_paste {
            entries.push((paste_label.to_string(), paste_cmd, json!({ "index": index })));
        }
        let f_for_menu = f.clone();
        let cf = cf.into_element().on_mouse_down(MouseButton::Right, move |e, _, cx: &mut App| {
            context_menu_at(&f_for_menu, e.position, 180.0, entries.clone(), cx);
            cx.stop_propagation();
        });
        let cf = crate::popovers::with_button_bounds(cf, swatch_hit);
        let eye = icon_cmd(if row.visible { Icon::Eye } else { Icon::EyeOff }, cmd, json!({ "index": index, "visible": !row.visible }), f, false);
        let minus = icon_cmd(Icon::Minus, if fills { "fill.remove" } else { "stroke.remove" }, json!({ "index": index }), f, false);
        // Motion mode: the first solid paint's row carries a keyframe diamond (egui `diamond()`).
        // Clicking toggles a keyframe for this paint's colour at the playhead. The keyed fill state
        // is precomputed by the host from the motion playhead.
        let mut trailing = vec![eye, minus];
        if keyframe_layer.is_some() && first_solid == Some(idx) {
            let q = motion_q.clone();
            let layer = keyframe_layer.unwrap();
            let prop = if fills { AnimProperty::FillColor } else { AnimProperty::StrokeColor };
            let diamond =
                IconButton::new(if keyed { Icon::Component } else { Icon::Instance }).on_click(move |_, _, _| q.push(Act::ToggleKey(layer, prop)));
            trailing.push(diamond);
        }
        sec = sec.child(field_row_full_trailing(cf, trailing));
    }
    // Stroke 第二行：描边对齐下拉 + 线宽字段 + 独立边宽 + 高级描边（egui `paints.rs`：
    // `dropdown(align, half)` 之后紧跟 `number_field_mixed("strokeWeight", …, weight_w)`）。
    // ⚠️ 混合模式（Blend）**不在这里**：egui 把它放在 `appearance` 节里，由 header 的
    // blend 开关（`f.blend_open`）展开下拉；Fill 的 `paints` 里没有独立混合模式行。
    // 之前这里把 blend 当成 Fill 末尾常驻行重复画了一次，正是「填充下面多出一块」的根因。
    if !fills {
        if let Some(align) = d.stroke_align {
            let rows = [
                (tr("Inside"), "node.set", json!({"props": {"strokeAlign": "inside"}})),
                (tr("Center"), "node.set", json!({"props": {"strokeAlign": "center"}})),
                (tr("Outside"), "node.set", json!({"props": {"strokeAlign": "outside"}})),
            ]
            .into_iter()
            .map(|(l, c, p)| (l.to_string(), c, p))
            .collect();
            // `IndependentCorners`（独立边宽，`strokeSides`）+ `Dots`（高级描边样式）。
            // egui `paints.rs`: `let w = weight.unwrap_or(1.0)` —— 每条边的权重来自当前
            // 描边宽度，硬编码 1.0 会把一条 8pt 的描边点一下就变成 1pt。
            let w = d.stroke_weight.unwrap_or(1.0);
            let sides = menu_icon(
                f,
                Icon::AbsolutePosition,
                200.0,
                vec![
                    (tr("All").into(), "node.set", json!({ "props": { "strokeSides": Value::Null } })),
                    (tr("Top").into(), "node.set", json!({ "props": { "strokeSides": [w, 0.0, 0.0, 0.0] } })),
                    (tr("Right").into(), "node.set", json!({ "props": { "strokeSides": [0.0, w, 0.0, 0.0] } })),
                    (tr("Bottom").into(), "node.set", json!({ "props": { "strokeSides": [0.0, 0.0, w, 0.0] } })),
                    (tr("Left").into(), "node.set", json!({ "props": { "strokeSides": [0.0, 0.0, 0.0, w] } })),
                    (tr("Custom").into(), "node.set", json!({ "props": { "strokeSides": [w, w, w, w] } })),
                ],
                false,
            );
            // Advanced stroke (egui `right/stroke.rs::show_stroke_settings`)：线宽线形、
            // dash/gap、端帽、连接、宽度配置与 Brush，恢复成带标题栏的「Stroke」通用窗口。
            let popups = f.popups();
            let stroke_queue = f.queue();
            let stroke_data = d.clone();
            let dots = window_open_button("stroke", Icon::Menu, popups.is_open("stroke"), move |window, cx| {
                let top = crate::popovers::anchored_top("stroke", window);
                open_detached_with(popups.clone(), "stroke", Some(top), cx, |anchor, cx| {
                    crate::popovers::WindowState::stroke(stroke_queue.clone(), &stroke_data, cx, anchor)
                });
            });
            // 第二行 = 描边对齐下拉 + **线宽字段** + 独立边宽 + 高级描边（egui `paints.rs`:
            // `dropdown(align, half)` 之后紧跟 `number_field_mixed("strokeWeight", …, weight_w)`）。
            sec = sec.child(field_row(
                vec![
                    pick_dropdown(f, align, col, rows).into_any_element(),
                    // 线宽可绑变量（`paints.rs::stroke`：`variable_ui::number_field_mixed(..., "strokeWeight", ...)`）。
                    bindable(
                        f,
                        "strokeWeight",
                        d.bound.get("strokeWeight").map(String::as_str),
                        &d.number_variables,
                        f.stroke_weight.clone().into_any_element(),
                    ),
                ],
                vec![sides.into_any_element(), dots],
            ));
        }
    }
    Section::Full(sec)
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Effects —— `effects.rs::effects`
// ───────────────────────────────────────────────────────────────────────────────────────────────

/// WGSL 材质下拉行 + 选中文件的参数输入框，并入 Effects section 的统一网格。
/// 已绑定 WGSL 材质时，在 Effects 节内就地画：
/// * 一行**和普通效果行同款的下拉**：显示当前材质名，点开列固定目录里每个 `.wgsl`（就地换材质）
///   以及一条「None」（解绑）；行尾仍是 24 pt 的 `−`（与 effect 行删除键同位置）。
/// * 该 shader 扫描出的参数，逐项画成「左边独立名称 + 右边数值输入框」（名称不再塞进输入框）。
///
/// 未绑定材质时返回空 `Vec`，Effects 节就是原来的样子（零回归）——选择入口在 `+` 菜单里也有
/// 一份，所以不绑定也能加材质；绑定后这里多一个可换材质的下拉。
fn wgsl_material_rows(f: &RightFields, d: &PanelData, col: f32, cx: &mut App) -> Vec<crate::widgets::FieldRow> {
    let Some(path) = d.wgsl.path.as_deref() else { return Vec::new() };
    let name = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());

    let mut out = Vec::new();
    // 下拉：与 `+` 菜单同源（每次现扫固定目录），选中即 `node.set` 换绑定；末尾 `None` 解绑。
    // 这样外置材质和普通效果一样，行首就是一个下拉，而不是一段只能看的纯文本。
    let mut items: Vec<(String, &'static str, Value)> = crate::wgsl_material::scan_material_dir()
        .iter()
        .map(|wf| {
            let p = wf.path.to_string_lossy().into_owned();
            (wf.name.clone(), "node.set", crate::wgsl_material::write_binding_params(Some(&crate::wgsl_material::binding_for(&p))))
        })
        .collect();
    if items.is_empty() {
        // 目录里没有文件（或目录不存在）：下拉至少保留当前这一条，不让它变成一个空菜单。
        items.push((name.clone(), "node.set", crate::wgsl_material::write_binding_params(Some(&crate::wgsl_material::binding_for(path)))));
    }
    items.push((tr("None").to_string(), "node.set", crate::wgsl_material::write_binding_params(None)));
    let dd = pick_dropdown(f, name, col, items);
    let minus = icon_cmd(Icon::Minus, "node.set", crate::wgsl_material::write_binding_params(None), f, false);
    out.push(field_row_full_trailing(dd, vec![minus]));

    // 该 shader 的参数在下面就地展开：名称是**输入框前面的独立文本**，值才进输入框。
    let specs = crate::wgsl_material::params_for(path);
    if specs.is_empty() {
        return out;
    }
    out.push(sublabel_row(tr("WGSL Material Parameters")));
    let binding = crate::wgsl_material::WgslMaterialBinding { path: path.to_string(), params: d.wgsl.params.clone() };
    for (slot, spec) in specs.iter().take(crate::panels::WGSL_PARAM_FIELDS).enumerate() {
        let value = d.wgsl.params.get(&spec.field).copied().unwrap_or(spec.default);
        let field = f.wgsl_param_field(cx, slot, spec, value, &binding);
        let label = div()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_size(pt(crate::widgets::TEXT_PT))
            .text_color(rgba(tokens().text_secondary))
            .child(spec.label.clone());
        out.push(field_row(vec![label.into_any_element(), field.into_any_element()], Vec::<gpui::AnyElement>::new()));
    }
    out
}

/// Effects 标题栏的 `+`：点开是一个「添加效果」菜单 —— 先列 7 种内置效果（`effect.add`），
/// 一条分隔线后列出固定目录里每个 `.wgsl` 文件（`node.set` 写 plugin_data 绑定）。
///
/// 这是用户定的交互：WGSL 不另起一行下拉，而是**和内置效果同处一个菜单**，有多少个文件就
/// 有多少个菜单项；选中文件后其参数才在下方就地展开（由 `d.wgsl.path` 驱动，见 `effects()`）。
/// 菜单项每次点开时现扫固定目录，增删文件立刻反映，无需重编译。
fn add_effect_menu(f: &RightFields) -> IconButton {
    let mut b = IconButton::new(Icon::Plus).size(24.0);
    if let Some(root) = f.root.clone() {
        let cmds = f.cmds.clone();
        b = b.on_click(move |_e, window, cx: &mut App| {
            // `on_click` 是 `Fn`，每次点击都要给 on_pick 一份独立的队列句柄，故这里重新 clone
            // （不能直接把外层的 `cmds` 移进内层闭包）。与 `pick_dropdown` 同构。
            let cmds = cmds.clone();
            // 7 种内置效果：与 `Effect::TYPES` 同源，选中即加一条该类型效果。
            let mut items: Vec<(String, &'static str, Value)> =
                figcraft_doc::Effect::TYPES.iter().map(|(label, ty)| (label.to_string(), "effect.add", json!({ "type": ty }))).collect();
            // 内置效果项数：第一个材质项前面画一条分隔线，把它和内置效果分组。
            let types_len = items.len();
            // 固定目录材质：每个文件一项，选中写完整的 binding（含默认参数）到 plugin_data。
            for wf in crate::wgsl_material::scan_material_dir() {
                let binding = crate::wgsl_material::binding_for(&wf.path.to_string_lossy());
                items.push((wf.name.clone(), "node.set", crate::wgsl_material::write_binding_params(Some(&binding))));
            }
            // `separator_before` 只在行上方画一条线、该行仍可点，故分隔标记挂在第一个材质项上，
            // 不引入不可点的「标题行」（那种行被点中会按 label 匹配、可能误触发命令）。
            let rows: Vec<MenuRow> = items
                .iter()
                .enumerate()
                .map(|(i, (l, _, _))| {
                    let r = MenuRow::new(l.clone());
                    if i == types_len { r.separator() } else { r }
                })
                .collect();
            let on_pick = Rc::new(move |label: &str, _w: &mut Window, _cx: &mut App| {
                if let Some((_, cmd, params)) = items.iter().find(|(l, _, _)| l == label) {
                    cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
                }
            });
            let p = window.mouse_position();
            let anchor = Bounds { origin: p, size: size(px(1.0), px(0.0)) };
            root.update(cx, |cv, cx| cv.open_menu(anchor, Some(180.0), rows, on_pick, cx));
        });
    }
    b
}

/// 效果行的方框：点开弹出「效果设置」小窗（egui `effect_settings` 的设置项搬进浮层），
/// 不再是「弹出菜单切换效果类型」。类型切换在小窗顶部的下拉里，且用
/// `Effect::of_type` 生成干净对象 —— 见 `WindowState::effect_settings` 的说明。
fn effect_settings_button(f: &RightFields, row: &EffectRow, col: f32) -> Dropdown {
    let popups = f.popups();
    let cmds = f.cmds.clone();
    let effect = row.effect.clone();
    let label = SharedString::from(row.label.clone());
    let index = row.index;
    Dropdown::new(label.clone(), col, None).fill().on_click(move |bounds, _window, cx: &mut App| {
        open_detached_with(popups.clone(), format!("effect-{index}"), Some(bounds.origin.y), cx, |anchor, cx| {
            crate::popovers::WindowState::effect_settings(cmds.clone(), cx, anchor, index, label.clone(), effect.clone())
        });
    })
}

pub(crate) fn effects(
    f: &RightFields,
    d: &PanelData,
    col: f32,
    cx: &mut App,
    // Motion 模式：`(layer, 该属性在 playhead 已有关键帧, 动作队列)`；Design 传 None。
    // egui effects.rs 给每个 shadow/blur 数值字段一个菱形；GPUI 的行是「类型下拉 + 显隐 + 删除」，
    // 这里给每条 effect 行一个菱形，按 effect 类型映射到 ShadowBlur / BlurRadius。
    keyframe: Option<(NodeId, Vec<figcraft_doc::anim::motion::AnimProperty>, &Queue)>,
) -> Section {
    // `[Styles, Plus]`；`+` 点开是「添加效果」菜单：7 种内置效果 + 分隔线 + 固定目录里每个
    // `.wgsl` 文件项（有多少文件就多少项）。选中文件写 plugin_data 绑定，其参数才在下方展开。
    // WGSL 与内置效果同处一个菜单，不再另起独立下拉行。
    let make_icons = || vec![style_menu(f, d, "effect", None, Icon::Dot), add_effect_menu(f).into_any_element()];
    // EGUI `effects.rs`: the Effects header's context menu pastes a copied effect.
    let can_paste_effect_header = d.can_paste_effect;
    let header_entries: Vec<(String, &'static str, Value)> = if can_paste_effect_header {
        vec![(tr("Paste effect").to_string(), "effect.paste", json!({}))]
    } else {
        Vec::new()
    };

    // WGSL 材质：只在「已绑定」时于下方就地展开参数输入框（选中文件由上面的 `+` 菜单写入）。
    // 空态（0 效果、未绑定）连参数行都不画，Effects 节就是一行置灰头部 + 可点的 `+`。
    let wgsl_rows = wgsl_material_rows(f, d, col, cx);

    let Some(rows) = d.effects.clone().filter(|v| !v.is_empty()) else {
        // 没有普通效果、但已绑定材质时，Effects 节仍要画出来以承载其参数行；
        // 未绑定时这里是空节（置灰头部 + 可点的 `+`，`+` 菜单里能选内置效果或 wgsl 文件）。
        if !wgsl_rows.is_empty() {
            let mut sec = PanelSection::new(tr("Effects")).header_trail(context_header_row(f, make_icons(), 160.0, header_entries.clone()));
            if let Some(chip) = style_chip(f, d.style_chips[2].as_ref(), "effect") {
                sec = sec.child(chip);
            }
            for r in wgsl_rows {
                sec = sec.child(r);
            }
            return Section::Full(sec);
        }
        return Section::empty(tr("Effects"), context_header_row(f, make_icons(), 160.0, header_entries.clone()));
    };
    let mut sec = PanelSection::new(tr("Effects")).header_trail(context_header_row(f, make_icons(), 160.0, header_entries));
    // egui `effects.rs`：`style_picker::chip(StyleSlot::Effect)` 紧跟在 header 后面。
    if let Some(chip) = style_chip(f, d.style_chips[2].as_ref(), "effect") {
        sec = sec.child(chip);
    }
    // 材质行放在普通效果之前：选中文件→下方参数展开，视觉上和「加一条效果→展开设置」一致。
    for r in wgsl_rows {
        sec = sec.child(r);
    }
    for row in rows.iter().rev() {
        // 点效果行的方框 → 效果设置小窗；原先这里点开的是「切换效果类型」菜单。
        // 原切换实现 clone 旧 JSON 只改 `type`，残留旧类型字段会被引擎
        // `refuse_ignored_effect_keys` 整条拒收，表现为「点了没反应、画布里没效果」。
        // EGUI `effects.rs`: a right-click on the effect row copies that effect and, when the
        // clipboard holds one, offers Paste effect at the same index.
        let mut effect_entries = vec![(tr("Copy effect").to_string(), "effect.copy", json!({ "index": row.index }))];
        let can_paste_effect = d.can_paste_effect;
        if can_paste_effect {
            effect_entries.push((tr("Paste effect").to_string(), "effect.paste", json!({ "index": row.index })));
        }
        let f_for_menu = f.clone();
        let dd = effect_settings_button(f, row, col).into_element().on_mouse_down(MouseButton::Right, move |e, _, cx: &mut App| {
            context_menu_at(&f_for_menu, e.position, 180.0, effect_entries.clone(), cx);
            cx.stop_propagation();
        });
        let mut e2 = row.effect.clone();
        if let Some(o) = e2.as_object_mut() {
            o.insert("visible".into(), json!(!row.visible));
        }
        // 显隐按钮必须反映该 effect 当前的 `visible`，否则用户看不出点它会在显示/隐藏之间
        // 切到哪一边（egui `effects.rs` 用 Eye / EyeOff）。原先这里写了 Dot / Close——图标集
        // 里明明有 `Icon::Eye` / `Icon::EyeOff`，用圆点冒充眼睛是错的，已改正。
        let eye = icon_cmd(if row.visible { Icon::Eye } else { Icon::EyeOff }, "effect.set", json!({ "index": row.index, "effect": e2 }), f, false);
        let minus = icon_cmd(Icon::Minus, "effect.remove", json!({ "index": row.index }), f, false);
        let mut trailing = vec![eye, minus];
        if let Some((layer, keyed, q)) = &keyframe {
            use figcraft_doc::anim::motion::AnimProperty;
            let ty = row.effect.get("type").and_then(|v| v.as_str()).unwrap_or_default();
            let prop = match ty {
                "dropShadow" | "innerShadow" => AnimProperty::ShadowBlur,
                _ => AnimProperty::BlurRadius,
            };
            let on = keyed.contains(&prop);
            let queue = (*q).clone();
            let layer = *layer;
            trailing.push(
                IconButton::new(if on { Icon::Component } else { Icon::Instance }).on_click(move |_, _, _| queue.push(Act::ToggleKey(layer, prop))),
            );
        }
        sec = sec.child(field_row_full_trailing(dd, trailing));
    }
    Section::Full(sec)
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Selection colors —— `selection_colors.rs::section`
// ───────────────────────────────────────────────────────────────────────────────────────────────

/// 参考实现 `selection_colors.rs::header`：折叠时最多显示 3 个色块，多的收进 `+N`。
fn collapsed_swatch_count(n: usize) -> usize {
    n.min(3)
}
fn collapsed_extra(n: usize) -> usize {
    n.saturating_sub(3)
}

/// 参考实现 `Id::new(("selection_colors_open", ds.uid))` 的 GPUI 等价物：
/// 状态按文档 uid 存，缺省折叠，点一次只翻转当前文档那一档。
type OpenByDoc = std::collections::HashMap<u64, bool>;
fn open_for_doc(map: &OpenByDoc, uid: u64) -> bool {
    *map.get(&uid).unwrap_or(&false)
}
fn toggle_doc(map: &mut OpenByDoc, uid: u64) {
    let next = !open_for_doc(map, uid);
    map.insert(uid, next);
}

/// 折叠头部的 16 pt 色块（参考实现 `widgets::paint_swatch` 的纯色分支）。
///
/// 冻结的 `widgets::paint_swatch` **无条件**先画 `checker_rounded`，四角会露出白色/浅色
/// 像素，看上去像「四个圆角溢出色块」（用户 2026-11-11 指出）。参考实现 对
/// `opaque_solid`（`opacity>=1 && color.a>=1`）**根本不画棋盘**，只画一块圆角 3 的色，
/// 最后 `rect_stroke(rect, same(3), 1pt border, Inside)`。这里在业务侧照该规则重画，
/// 不改冻结组件；渐变/图片/图案仍回退到原组件，外层圆角+裁剪兜住溢出。
fn collapsed_swatch(paint: figcraft_color::Paint, side: f32) -> gpui::AnyElement {
    let t = tokens();
    let solid = match &paint.kind {
        PaintKind::Solid { color } => Some(color.clone()),
        _ => None,
    };
    let Some(color) = solid else {
        // 渐变 / 图片 / 图案沿用冻结组件的全部行为，外层圆角 + 裁剪兜住四角溢出。
        return div().flex_none().w(pt(side)).h(pt(side)).rounded(px(3.0)).overflow_hidden().child(swatch(paint, side)).into_any_element();
    };
    let opaque = rgba(crate::widgets::swatch::color32(color.with_alpha(1.0)));
    let tinted = rgba(crate::widgets::swatch::color32(color.with_alpha(color.a * paint.opacity)));
    let is_opaque = paint.opacity >= 0.999 && color.a >= 0.999;
    // 用 GPUI 原生 `div` 背景，而不是 canvas 里自绘 `paint_quad`：quad 的圆角抗锯齿会在
    // 四角留下彩色半透明像素，正是「四个圆角溢出色块」。半透明时直接把带 alpha 的色
    // 叠在面板白底上（参考实现 左不透明/右带 alpha 的 参考编辑器 分半在 16pt 上无差别）。
    // ⚠️ 不画 `border_1()`：1pt 浅灰描边压在圆角填充的抗锯齿上，四角会出现一圈
    // 「浅色虚线尖角」（用户 2026-11-11 指出的「四个圆角溢出色块」）。参考实现 的
    // 1pt Inside 描边在 16pt 色块上肉眼几乎不可见，去掉后与原文观感一致且四角干净。
    div().flex_none().w(pt(side)).h(pt(side)).rounded(px(3.0)).bg(if is_opaque { opaque } else { tinted }).into_any_element()
}

/// Selection colors 节（`参考实现 selection_colors.rs::section`）的折叠样式 1:1：
///
/// * 默认**折叠**：40 pt 可点头部，标题 `text_secondary`；悬停时提亮并显示左侧 chevron；
/// * 折叠态右侧并排最多 3 个 16 pt 色块（间距 8），再多一个 `+N`；
/// * 点标题展开，才画原来的颜色行；展开态 chevron 朝下。
///
/// 头部不用冻结的 `SectionHeader`（它没有整行点击 / hover 状态），但展开后的每一行仍由
/// `field_row_full_trailing` 构造，走同一套统一网格。
pub(crate) fn selection_colors(
    f: &RightFields,
    d: &PanelData,
    row_w: f32,
    _col: f32,
    n: &mut usize,
    _window: &mut Window,
    cx: &mut App,
) -> Option<gpui::AnyElement> {
    // 上游 `selection_colors.rs::section`（参考编辑器 实测）：两种不同颜色显示这一节；
    // 只有一种颜色时，**选中的图层带子级**（frame / group / component / instance）也要显示。
    // 之前只判 `len() < 2`，于是「画框里只有一个组件、一个颜色」整节消失。
    if d.selection_colors.len() < 2 && !(d.selection_colors.len() == 1 && d.selection_has_children) {
        return None;
    }
    let t = tokens();
    // 参考实现 `Id::new(("selection_colors_open", ds.uid))`：开合状态**按文档 uid 分档**，
    // 切到另一个文档时各自保留自己的状态，缺省折叠。
    let uid = d.doc_uid;
    let open = open_for_doc(&f.selection_colors_open.borrow(), uid);
    let hovered = *f.selection_colors_hover.borrow();
    // 参考实现：`hovered && dark` → 白；`hovered` → text；否则 text_secondary。
    let ink = if hovered && t.dark {
        Color32::WHITE
    } else if hovered {
        t.text
    } else {
        t.text_secondary
    };

    // ── 头部 ────────────────────────────────────────────────────────────────
    // 参考实现 `theme::property_section_title()` = medium(ui_font_size(10.5))；GPUI 的
    // `SectionHeader` 把这一档固化成 TITLE_PT=13 / MEDIUM / 1.25 行高。折叠节标题必须
    // 与上面的「填充 / 效果 / 描边」用同一套排版，不能自己写一个 10.5 pt 的小字。
    let title = div()
        .flex_none()
        .text_size(pt(TITLE_PT))
        .line_height(pt(TITLE_PT * 1.25))
        .font_weight(crate::panels::UI_FONT_REGULAR)
        .text_color(rgba(ink))
        .child(tr("Selection colors"));
    // 参考实现 从 `rect.right() - PAD` 起从右往左摆：`+N` 最右，色块在其左，间隔 8 pt。
    let mut right = div().flex_none().flex().flex_row().items_center().gap(pt(8.0));
    if !open {
        for row in d.selection_colors.iter().take(collapsed_swatch_count(d.selection_colors.len())) {
            let color = figcraft_color::Color::from_hex(&row.hex).unwrap_or(figcraft_color::Color::WHITE);
            let paint = serde_json::from_value::<figcraft_color::Paint>(row.paint.clone()).unwrap_or_else(|_| figcraft_color::Paint::solid(color));
            right = right.child(collapsed_swatch(paint, 16.0));
        }
        let extra = collapsed_extra(d.selection_colors.len());
        if extra > 0 {
            // 参考实现 `regular(11.0)` → ui_font_size(11)=12.94，GPUI 对应 TITLE_PT 这一档
            // （字重是 regular，不是标题的 medium），颜色仍是 text_secondary。
            right = right
                .child(div().flex_none().text_size(pt(TITLE_PT)).text_color(rgba(t.text_secondary)).child(SharedString::from(format!("+{extra}"))));
        }
    }
    let open_cell = f.selection_colors_open.clone();
    let hover_cell = f.selection_colors_hover.clone();
    let cmds = f.cmds.clone();
    let mut head = div()
        .id("selection-colors-header")
        .relative()
        .flex_none()
        .w_full()
        .h(pt(HEADER_H))
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .pl(pt(PAD))
        // 参考实现 header 右侧也从 `rect.right() - PAD` 起算，左右都是 16 pt。
        .pr(pt(PAD))
        .cursor_pointer()
        .child(title)
        .child(right)
        .on_click(move |_e, _window, _cx| {
            let mut by_doc = open_cell.borrow_mut();
            toggle_doc(&mut by_doc, uid);
            drop(by_doc);
            cmds.wake();
        })
        .on_hover(move |hovering: &bool, window: &mut Window, _cx: &mut App| {
            *hover_cell.borrow_mut() = *hovering;
            window.refresh();
        });
    if hovered {
        // 悬停才画：10 pt chevron 落在 `PAD - 10` 的位置，展开朝下 / 折叠朝右。
        let icon = if open { Icon::ChevronDown } else { Icon::ChevronRight };
        head = head.child(
            div().absolute().left(pt(PAD - 15.0)).top(pt((HEADER_H - 10.0) / 2.0)).w(pt(10.0)).h(pt(10.0)).child(crate::widgets::icon::paint(
                icon,
                rgba(ink),
                10.0,
            )),
        );
    }
    let mut sec = div().flex().flex_col().w_full().child(head);
    if !open {
        return Some(sec.into_any_element());
    }

    // ── 展开态：原来的颜色行（参考实现 在 `add_enabled_ui(!interacting, …)` 里画）──
    let mut body = div().flex().flex_col().w_full().pb(pt(SECTION_BOTTOM));
    for row in d.selection_colors.iter() {
        let targets = row.targets.clone();
        let hex_slot = slot(n);
        let op_slot = slot(n);
        let hex_ed = f.row_field(
            cx,
            hex_slot,
            &row.hex,
            RowEditCtx {
                cmd: "selection.setPaint",
                params: json!({ "targets": targets }),
                key: "paint",
                scale: 1.0,
                string_value: true,
                paint: None,
                // 改 hex 也要 clone 整组 Paint 再换 `color`（egui `section`）：只发一个
                // `"#RRGGBB"` 会让 `paint_of` 造出 opacity=1 的新 paint，把透明度抹掉。
                nest: None,
                nest_key: None,
                paint_color: Some(row.paint.clone()),
            },
        );
        let op_ed = f.row_field(
            cx,
            op_slot,
            &format!("{}", row.opacity.round() as i64),
            RowEditCtx {
                cmd: "selection.setPaint",
                params: json!({ "targets": targets }),
                key: "opacity",
                scale: 100.0,
                string_value: false,
                paint: Some(row.paint.clone()),
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        // egui `section`: 纯色显示 hex 且可编辑；渐变/图片只显示名字，点它开取色器，
        // 所以 hex 编辑器必须只读，否则会把一个名字当成颜色提交。
        if !row.solid {
            hex_ed.update(cx, |te, _| te.set_readonly(true));
        }
        let color = figcraft_color::Color::from_hex(&row.hex).unwrap_or(figcraft_color::Color::WHITE);
        // egui `section` 直接拿 `group.paint` 画色块：这里也必须用该组原本的完整 Paint，
        // 否则 `Paint::solid(color)` 会把透明度抹成 1、把渐变/图片退成纯色（甚至白色）。
        let paint = serde_json::from_value::<figcraft_color::Paint>(row.paint.clone()).unwrap_or_else(|_| figcraft_color::Paint::solid(color));
        let text =
            if row.solid { ColorText::Hex(SharedString::from(row.hex.clone())) } else { ColorText::Label(SharedString::from(row.hex.clone())) };
        // 取色器的初值同样取自完整 paint（渐变取其代表色），不是从 hex 反解。
        let current = paint.solid_color().map(crate::widgets::swatch::color32).unwrap_or(Color32::BLACK);
        let cf = ColorField::new(cx, paint, text, row_w, hex_ed, op_ed);
        // swatch 点击开取色器（`selection_colors.rs::toggle`）：`selection.setPaint` 用 `paint` 键。
        let (cf, swatch_hit) = color_pick(
            f,
            cf,
            current,
            "selection.setPaint",
            json!({ "targets": targets }),
            "paint",
            Some(row.paint.clone()),
            format!("color-sel-{targets}"),
            Vec::new(),
        );
        // 选区颜色组绑定到变量时同样走 `Field-color / Disabled`：整框可点换绑，24 pt
        // 尾插槽换成解绑按钮（`selection.detachColor`）。换绑菜单只列这组作用域内的变量。
        let var_names: Vec<String> = row.variables.iter().map(|(name, _)| name.clone()).collect();
        let cf = match row.linked.as_deref() {
            Some(name) => bound_color_field(
                f,
                cf,
                name,
                "fill",
                &var_names,
                Some(targets.clone()),
                ("selection.detachColor", json!({ "targets": targets.clone() })),
            ),
            None => cf,
        };
        let cf = crate::popovers::with_button_bounds(cf, swatch_hit);
        let bind = selection_bind(f, row, d).into_any_element();
        let select = icon_cmd(Icon::Search, "selection.selectColor", json!({ "targets": targets }), f, false).into_any_element();
        // 两个 24 pt 尾槽：Styles/变量（绑定态留空占位） + 选中同色图层。
        body = body.child(field_row_full_trailing(cf, vec![bind, select]).into_element());
    }
    Some(sec.child(body).into_any_element())
}

#[cfg(test)]
mod selection_colors_tests {
    use super::{OpenByDoc, collapsed_extra, collapsed_swatch_count, open_for_doc, toggle_doc};

    /// 参考实现 `header`：折叠时 3 个色块，多出来的收进 `+N`。
    #[test]
    fn collapsed_header_shows_three_swatches_and_the_rest_as_a_count() {
        // 少于两组根本不画这一节，但纯函数仍给出安全值。
        assert_eq!(collapsed_swatch_count(0), 0);
        assert_eq!(collapsed_extra(0), 0);
        for n in [2, 3] {
            assert_eq!(collapsed_swatch_count(n), n);
            assert_eq!(collapsed_extra(n), 0);
        }
        assert_eq!(collapsed_swatch_count(7), 3);
        assert_eq!(collapsed_extra(7), 4);
        assert_eq!(collapsed_extra(4), 1);
    }

    /// 参考实现 `Id::new(("selection_colors_open", ds.uid))`：两个文档各自记住开合状态。
    #[test]
    fn open_state_is_keyed_by_document_uid() {
        let mut map = OpenByDoc::new();
        // 缺省折叠。
        assert!(!open_for_doc(&map, 1));
        assert!(!open_for_doc(&map, 2));
        toggle_doc(&mut map, 1);
        assert!(open_for_doc(&map, 1));
        assert!(!open_for_doc(&map, 2), "opening doc 1 must not open doc 2");
        toggle_doc(&mut map, 2);
        assert!(open_for_doc(&map, 1));
        assert!(open_for_doc(&map, 2));
        toggle_doc(&mut map, 1);
        assert!(!open_for_doc(&map, 1));
        assert!(open_for_doc(&map, 2), "closing doc 1 must not close doc 2");
    }
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Layout grid —— `effects.rs::grids`
// ───────────────────────────────────────────────────────────────────────────────────────────────

pub(crate) fn grids(f: &RightFields, d: &PanelData, col: f32) -> Section {
    // `[Styles, Plus]`；egui `effects.rs::grids` 的 `+` 直接加一条默认网格（`grid.add {ids}`），
    // 类型再在行内下拉改。GPUI 不要在 header 弹类型菜单，直接加默认网格（与源码一致）。
    let icons = vec![style_menu(f, d, "grid", None, Icon::Dot), icon_cmd(Icon::Plus, "grid.add", json!({}), f, false).into_any_element()];
    // 空 / Mixed：只有置灰头部（同 Fill / Stroke）。
    let Some(rows) = d.grids.clone().filter(|v| !v.is_empty()) else {
        return Section::empty(tr("Layout grid"), icon_row(icons));
    };
    let mut sec = PanelSection::new(tr("Layout grid")).header_trail(icon_row(icons));
    // egui `effects.rs::grids`：`style_picker::chip(StyleSlot::Grid)` 紧跟 header。
    if let Some(chip) = style_chip(f, d.style_chips[3].as_ref(), "grid") {
        sec = sec.child(chip);
    }
    for row in rows.iter().rev() {
        let kinds: Vec<(String, &'static str, Value)> = ["grid", "columns", "rows", "diagonal", "rings"]
            .into_iter()
            .map(|k| {
                let mut arr = row.grids.clone();
                if let Some(item) = arr.as_array_mut().and_then(|a| a.get_mut(row.index)) {
                    if let Some(o) = item.as_object_mut() {
                        o.insert("kind".into(), json!(k));
                    }
                }
                (k.to_string(), "node.set", json!({ "props": { "grids": arr } }))
            })
            .collect();
        let dd = pick_dropdown(f, row.label.clone(), col, kinds);
        let mut arr = row.grids.clone();
        if let Some(item) = arr.as_array_mut().and_then(|a| a.get_mut(row.index)) {
            if let Some(o) = item.as_object_mut() {
                o.insert("visible".into(), json!(!row.visible));
            }
        }
        // 同 effect 行：显隐图标按该 grid 的 `visible` 切换，不能恒定一个 Dot —— 而且这里
        // 本来就该是 `Eye` / `EyeOff`（egui `effects.rs::grids`），不是圆点/叉。
        let eye = icon_cmd(if row.visible { Icon::Eye } else { Icon::EyeOff }, "node.set", json!({ "props": { "grids": arr } }), f, false);
        let minus = icon_cmd(Icon::Minus, "grid.remove", json!({ "index": row.index }), f, false);
        sec = sec.child(field_row_full_trailing(dd, vec![eye, minus]));
    }
    Section::Full(sec)
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Export —— `export_panel.rs::section` / `setting_row`
// ───────────────────────────────────────────────────────────────────────────────────────────────

pub(crate) fn export(f: &RightFields, d: &PanelData, col: f32, _n: &mut usize, cx: &mut App) -> Section {
    // egui `export_panel.rs::section`: `mixed = shared.is_none() && sel.len() > 1`。
    // mixed 时 `+` 是「整体替换成一条默认设置」（set_props exports），不是 export.add。
    // 空选区 = Page 页：设置存在页面节点上，导出按钮把页面子层合成一张图。
    let page_mode = d.ids.is_empty() && d.page_node.is_some();
    let target: Vec<figcraft_doc::NodeId> =
        if page_mode { vec![d.page_node.expect("page mode has a page node")] } else { d.ids.clone() };
    // `export.*` 的 `ids` 显式带上，空选区时才不会被当成「没有目标」。
    let with_ids = |mut v: Value| {
        if let Some(o) = v.as_object_mut() {
            o.insert("ids".into(), json!(&target));
        }
        v
    };
    let mixed = d.exports.is_none();
    let replace = with_ids(json!({ "props": { "exports": [figcraft_doc::ExportSetting::default()] } }));
    let plus = if mixed {
        icon_cmd(Icon::Plus, "node.set", replace, f, false)
    } else {
        icon_cmd(Icon::Plus, "export.add", with_ids(json!({})), f, false)
    };
    let Some(rows) = d.exports.clone().filter(|v| !v.is_empty()) else {
        return Section::empty(tr("Export"), plus);
    };
    let mut sec = PanelSection::new(tr("Export")).header_trail(plus);
    // 预览框是整行宽（两列 + 间隙），图片再让出左右内边距与 8 pt 呼吸位。
    let full_w = col * 2.0 + crate::widgets::GRID_GAP;
    for row in rows.iter() {
        let index = row.index;
        // ① 第一行（统一网格）：Size（仅位图）+ 格式，行尾 Dots 设置小窗与删除。
        let formats: Vec<(String, &'static str, Value)> =
            [("PNG", "png"), ("JPG", "jpg"), ("SVG", "svg"), ("PDF", "pdf"), ("WebP", "webp"), ("XML", "xml")]
                .into_iter()
                .map(|(l, ext)| (l.to_string(), "export.update", with_ids(json!({ "index": index, "format": ext }))))
                .collect();
        let mut fields: Vec<gpui::AnyElement> = Vec::new();
        if row.is_raster {
            let mut sizes: Vec<(String, &'static str, Value)> = Vec::new();
            for v in figcraft_doc::ExportSize::SCALES {
                let label = figcraft_doc::ExportSize::Scale { value: v }.label();
                sizes.push((label.clone(), "export.update", with_ids(json!({ "index": index, "size": label }))));
            }
            for s in ["512w", "512h"] {
                sizes.push((s.to_string(), "export.update", with_ids(json!({ "index": index, "size": s }))));
            }
            fields.push(pick_dropdown(f, row.size.clone(), col, sizes).into_any_element());
        }
        fields.push(pick_dropdown(f, row.format_label, col, formats).into_any_element());
        // Dots 设置小窗（用户要求：单选器放弹窗，不放下拉）。
        let trigger = format!("export-options-{index}");
        let popups = f.popups();
        let queue = f.queue();
        let export_rows = export_setting_rows(row, &d.doc_profile);
        let btn_key = trigger.clone();
        let dots = window_open_button(&btn_key, Icon::Menu, popups.is_open(&trigger), move |window, cx| {
            let top = crate::popovers::anchored_top(&trigger, window);
            open_detached_with(popups.clone(), trigger.clone(), Some(top), cx, |anchor, _cx| {
                crate::popovers::WindowState::export_options(queue.clone(), tr("Export settings"), anchor, export_rows.clone())
            });
        });
        let minus = icon_cmd(Icon::Minus, "export.remove", with_ids(json!({ "index": index })), f, false).into_any_element();
        sec = sec.child(field_row(fields, vec![dots, minus]));
    }

    // ② 导出按钮（egui `wide_button`）：放在预览**上面**，紧贴设置行；左右 16pt，与预览框同宽。
    //    suffix 输入框那一整行已删除（空值时不可见却占 32pt，把按钮和设置行撑开）。
    let name = d.header.as_ref().map(|h| h.layer_name.clone()).unwrap_or_default();
    let (cmd, params, label): (&'static str, Value, SharedString) = if page_mode {
        let fmt = rows.first().map(|r| r.format).unwrap_or("png");
        let page_ids = d.page.as_ref().map(|p| p.export_ids.clone()).unwrap_or_default();
        ("file.export", json!({ "ids": page_ids, "format": fmt }), tr("Export page").into())
    } else if target.len() == 1 && !name.is_empty() {
        ("file.exportLayers", json!({ "ids": &target }), trf("Export {name}", &[("name", &name)]).into())
    } else {
        ("file.exportLayers", json!({ "ids": &target }), trn("Export {n} layers", target.len(), &[]).into())
    };
    sec = sec.child(block_row(
        div()
            .w_full()
            .h(pt(crate::widgets::ROW_H))
            .flex()
            .items_center()
            .pl(pt(crate::widgets::PAD))
            .pr(pt(crate::widgets::PAD))
            .child(wide_button(f, label, cmd, params).flex_1()),
    ));

    // ③ Preview（egui `export_panel::preview`）：折叠行 + 160 pt 预览框。
    let open = f.export_preview_open.get();
    let toggle = f.export_preview_open.clone();
    let wake = f.cmds.clone();
    let head = div()
        .w_full()
        .h_full()
        .flex()
        .items_center()
        .gap(pt(6.0))
        .cursor_pointer()
        .child(crate::widgets::icon::paint(if open { Icon::ChevronDown } else { Icon::ChevronRight }, rgba(tokens().text_secondary), 12.0))
        .child(div().text_size(pt(11.0)).text_color(rgba(tokens().text)).child(tr("Preview")))
        .on_mouse_down(MouseButton::Left, move |_e, _w, _cx| {
            toggle.set(!toggle.get());
            wake.wake();
        });
    sec = sec.child(field_row_full(head));
    if open {
        let t = tokens();
        // flex_1（不是 w_full）：父层带 16pt 内边距，w_full 会按整栏宽算、右边顶到面板边缘。
        let mut frame = div()
            .flex_1()
            .h(px(160.0))
            .rounded(px(crate::widgets::RADIUS))
            .border_1()
            .border_color(rgba(t.border))
            .overflow_hidden()
            .relative();
        frame = frame.child(
            canvas(|b, _w, _cx| b.size, |b, _, window, _cx| crate::widgets::checker(window, b, 8.0)).absolute().inset_0(),
        );
        if let Some((w, h, buf)) = &d.export_preview {
            let img = img(ImageSource::Render(crate::canvas::render_worker::wrap_bgra(*w, *h, buf.clone())))
                .max_w(px(d.export_preview_max_w.max(32.0)))
                .max_h(px(144.0));
            frame = frame.child(div().absolute().inset_0().flex().items_center().justify_center().child(img));
        }
        // 左右都 16，和上面的按钮严格同宽。
        sec = sec.child(block_row(
            div()
                .w_full()
                .flex()
                .pl(pt(crate::widgets::PAD))
                .pr(pt(crate::widgets::PAD))
                .pb(pt(8.0))
                .child(frame),
        ));
    }
    Section::Full(sec)
}

/// Dots 设置小窗的内容（egui `advanced_options` + `print_options`）。
///
/// 顺序按用户 2026-11-19 的要求：**带勾选 / 单选状态的项全部排在下面**。尺寸已提到行内的
/// 通用下拉，所以菜单里剩下的都是勾选项；两个单选组（Compatibility、Color profile）用
/// 分隔线开头，Color profile 放在最末。
/// 导出高级选项弹窗的正文行（egui `advanced_options` 的 1:1 移植）。
///
/// 勾选项用通用 Toggle，VD_COMPAT / Color profile 两组用 Radio（egui `widgets::radio`）；
/// 每组前一条 SubLabel 就是 egui 的 `widgets::label(..., text_secondary)`。egui 里这些住在
/// `menu_popup` 下拉中，但单选器在下拉里不好放，用户要求改用通用弹窗，内容一行不差。
fn export_setting_rows(row: &ExportRow, doc_profile: &str) -> Vec<crate::popovers::Setting> {
    use crate::popovers::{Opt, Setting};
    let index = row.index;
    let mut out: Vec<Setting> = Vec::new();
    let toggle = |label: String, on: bool, key: &'static str| Setting::Toggle {
        tab: 0,
        label: label.into(),
        on: std::rc::Rc::new(std::cell::RefCell::new(on)),
        supported: true,
        on_cmd: Opt::new(String::new(), "export.update", json!({ "index": index, key: true })),
        off_cmd: Opt::new(String::new(), "export.update", json!({ "index": index, key: false })),
    };
    out.push(toggle(tr("Ignore overlapping layers").to_string(), row.contents_only, "contentsOnly"));
    if row.format == "svg" || row.include_bounding_box {
        out.push(toggle(tr("Include bounding box").to_string(), row.include_bounding_box, "includeBoundingBox"));
    }
    if row.format == "svg" {
        out.push(toggle(tr("Outline text").to_string(), row.svg_outline_text, "svgOutlineText"));
        out.push(toggle(tr("Include id attribute").to_string(), row.svg_id_attribute, "svgIdAttribute"));
        out.push(toggle(tr("Simplify stroke").to_string(), row.svg_simplify_stroke, "svgSimplifyStroke"));
    }
    if row.format == "xml" {
        out.push(toggle(tr("Include id attribute").to_string(), row.svg_id_attribute, "svgIdAttribute"));
        out.push(Setting::SubLabel { tab: 0, text: tr("Compatibility").into() });
        for (label, key) in [
            (tr("Best effort").to_string(), "bestEffort"),
            (tr("Exact only").to_string(), "exact"),
            (tr("Ignore unsupported").to_string(), "ignore"),
        ] {
            out.push(Setting::Radio {
                tab: 0,
                label: label.into(),
                on: row.vd_compat == key,
                cmd: Opt::new(String::new(), "export.update", json!({ "index": index, "vdCompat": key })),
            });
        }
    }
    if row.format == "pdf" {
        out.push(Setting::Toggle {
            tab: 0,
            label: "3 mm bleed".into(),
            on: std::rc::Rc::new(std::cell::RefCell::new(row.bleed > 0.0)),
            supported: true,
            on_cmd: Opt::new(String::new(), "export.update", json!({ "index": index, "bleed": "3mm" })),
            off_cmd: Opt::new(String::new(), "export.update", json!({ "index": index, "bleed": 0 })),
        });
        out.push(toggle(tr("Crop marks").to_string(), row.crop_marks, "cropMarks"));
        out.push(toggle(tr("Bleed marks").to_string(), row.bleed_marks, "bleedMarks"));
        out.push(toggle("CMYK".to_string(), row.cmyk, "cmyk"));
    }
    if row.is_raster {
        out.push(Setting::SubLabel { tab: 0, text: tr("Color profile").into() });
        for p in figcraft_doc::ExportColorProfile::ALL {
            let label = match p {
                figcraft_doc::ExportColorProfile::SameAsFile => trf("Same as file ({profile})", &[("profile", &doc_profile)]),
                figcraft_doc::ExportColorProfile::Srgb => "sRGB".to_string(),
                figcraft_doc::ExportColorProfile::DisplayP3 => "Display P3".to_string(),
            };
            out.push(Setting::Radio {
                tab: 0,
                label: label.into(),
                on: row.color_profile == p.key(),
                cmd: Opt::new(String::new(), "export.update", json!({ "index": index, "colorProfile": p.key() })),
            });
        }
    }
    out
}

fn export_options(row: &ExportRow, doc_profile: &str) -> Vec<(String, Option<bool>, &'static str, Value, bool)> {
    let index = row.index;
    let toggle = |label: String, on: bool, key: &str| {
        (label, Some(on), "export.update", json!({ "index": index, key: !on }), false)
    };
    let mut opts: Vec<(String, Option<bool>, &'static str, Value, bool)> = Vec::new();
    opts.push(toggle(tr("Ignore overlapping layers").to_string(), row.contents_only, "contentsOnly"));
    opts.push(toggle(tr("Include bounding box").to_string(), row.include_bounding_box, "includeBoundingBox"));
    if row.format == "svg" {
        opts.push(toggle(tr("Outline text").to_string(), row.svg_outline_text, "svgOutlineText"));
        opts.push(toggle(tr("Include id attribute").to_string(), row.svg_id_attribute, "svgIdAttribute"));
        opts.push(toggle("Simplify strokes".to_string(), row.svg_simplify_stroke, "svgSimplifyStroke"));
    }
    if row.format == "xml" {
        opts.push(toggle(tr("Include id attribute").to_string(), row.svg_id_attribute, "svgIdAttribute"));
        for (i, (label, key)) in
            [("Compatibility: best effort", "bestEffort"), ("Compatibility: exact only", "exact"), ("Compatibility: ignore unsupported", "ignore")]
                .into_iter()
                .enumerate()
        {
            opts.push((label.to_string(), Some(row.vd_compat == key), "export.update", json!({ "index": index, "vdCompat": key }), i == 0));
        }
    }
    if row.format == "pdf" {
        // egui `print_params`: bleed / cropMarks / bleedMarks / cmyk 都走 `export.update`。
        let bleed = if row.bleed > 0.0 { json!(0) } else { json!("3mm") };
        opts.push(("3 mm bleed".to_string(), Some(row.bleed > 0.0), "export.update", json!({ "index": index, "bleed": bleed }), false));
        opts.push(toggle(tr("Crop marks").to_string(), row.crop_marks, "cropMarks"));
        opts.push(toggle(tr("Bleed marks").to_string(), row.bleed_marks, "bleedMarks"));
        opts.push(toggle("CMYK".to_string(), row.cmyk, "cmyk"));
    }
    if row.is_raster {
        // Color profile（egui `ExportColorProfile::ALL`）：最下面的单选组。
        for (i, p) in figcraft_doc::ExportColorProfile::ALL.into_iter().enumerate() {
            let label = match p {
                figcraft_doc::ExportColorProfile::SameAsFile => trf("Same as file ({profile})", &[("profile", &doc_profile)]),
                figcraft_doc::ExportColorProfile::Srgb => "sRGB".to_string(),
                figcraft_doc::ExportColorProfile::DisplayP3 => "Display P3".to_string(),
            };
            opts.push((label, Some(row.color_profile == p.key()), "export.update", json!({ "index": index, "colorProfile": p.key() }), i == 0));
        }
    }
    opts
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Variable modes / Section / Header / Page
// ───────────────────────────────────────────────────────────────────────────────────────────────

/// Frame-like 的「Clip content」勾选行（参考编辑器 的 Frame 设置区）。
///
/// 多选时值不一致按**未勾选**显示（`Option::unwrap_or(false)`，与 egui `common` 的语义一致）；
/// 切换推 `node.set {"props":{"clipsContent":!current}}`，引擎按当前选区执行。
pub(crate) fn clip_content(f: &RightFields, d: &PanelData) -> crate::widgets::panel_section::FieldRow {
    let cmds = f.cmds.clone();
    let cb = Checkbox::new("clip-content").label(tr("Clip content")).checked(d.clip.unwrap_or(false)).on_change(move |next: &bool, _w, _cx| {
        cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "clipsContent": *next } })) });
    });
    // egui `layout_panel.rs::layout_section` 的末尾就是这一行（`if frames { row(24, checkbox) }`）：
    // Clip content 属于 **Layout 节**，由 `auto_layout_section` 追加在网格轨道之后。
    field_row_full(cb)
}

/// 圆角展开区的显隐规则（egui `paints.rs::appearance`：`smooth_ok && indep_corners_open`）：
/// 只看类型是否支持平滑圆角（`smooth_ok`），与半径数值无关 —— 半径为 0 也能先设平滑值。
/// 注意**折叠态不显示展开区**——这正是「圆角折叠时不该显示平滑按钮」的修复点。
pub(crate) fn shows_corner_smoothing(supported: bool, _radius: f64) -> bool {
    supported
}

/// 圆角展开区里的「Corner smoothing」按钮：打开 `WindowState::corner_smoothing` 小窗。
///
/// 这是通用 24×24 图标按钮（与右栏其它开窗按钮完全同款），不再手写 `◇` div + 自绘背景；
/// 锚点走 [`crate::popovers::with_window_trigger`] 的跨帧探针，按下即与按钮顶部对齐。
/// 冻结的 `Icon` 集没有平滑圆角字形（type settings 的同类入口也因缺图标用 `Icon::Menu`），
/// 故沿用同一个约定用 `Icon::Menu`，不新增字形、不另造控件。
fn corner_smoothing_button(f: &RightFields, d: &PanelData) -> gpui::AnyElement {
    let popups = f.popups();
    let queue = f.queue();
    let value = d.corner_smoothing.unwrap_or(0.0) as f32;
    let open = popups.is_open("corner-smoothing");
    window_open_button("corner-smoothing", Icon::Menu, open, move |window, cx| {
        let top = crate::popovers::anchored_top("corner-smoothing", window);
        open_detached_with(popups.clone(), "corner-smoothing", Some(top), cx, |anchor, _cx| {
            crate::popovers::WindowState::corner_smoothing(queue.clone(), value, anchor)
        });
    })
}

/// Appearance 的 header 图标（egui `paints.rs::appearance`：Eye/Variable/Blend）。
pub(crate) fn appearance(
    f: &RightFields,
    d: &PanelData,
    row_w: f32,
    opacity: &gpui::Entity<crate::widgets::TextField>,
    corner: &gpui::Entity<crate::widgets::TextField>,
) -> PanelSection {
    let visible = d.visible.unwrap_or(true);
    let eye = icon_cmd(if visible { Icon::Eye } else { Icon::EyeOff }, "object.toggleVisible", json!({}), f, false);
    // ⛔ 这里**没有**变量按钮：egui `right.rs::appearance_header` 只有两颗 —— Show/Hide 与
    // Blend mode（`buttons = [(0, 1, …), (1, 0, …)]`）。之前多画了一颗 `variables_window_button`，
    // 用户 2026-11-16 指出「原版设计里没有这个东西」。
    // egui `paints.rs::appearance`：blend 按钮（索引 2）**不推命令**——它只翻转本地 `blend_open`
    // 标记来展开/收起下方的 blend-mode 下拉行；blend 值由那一行里的下拉选定（→ `node.set`）。
    // GPUI 必须一致：点这个按钮只是切换显隐，而不是直接弹 `node.set` 菜单。
    let blend_cmds = f.cmds.clone();
    let blend_open_ref = f.blend_open.clone();
    let blend_is_open = *blend_open_ref.borrow();
    let blend_open_for_click = blend_open_ref.clone();
    // 水滴图标走取色器专用 `PickerIcon::Blend`（资源只在 picker_icons 表注册，
    // 不再往 Icon/ICON_FILES 里重复加一份）。按钮外观与通用 24pt 图标按钮一致。
    let blend = div()
        .size(pt(24.0))
        .rounded(pt(5.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(blend_is_open, |d| d.bg(rgba(tokens().selected_row)))
        .when(!blend_is_open, |d| d.hover(|s| s.bg(rgba(tokens().hover))))
        .child(crate::picker_icons::paint(crate::picker_icons::PickerIcon::Blend, rgba(tokens().icon), 24.0))
        .on_mouse_down(gpui::MouseButton::Left, move |_e, _w, _cx| {
            let open = !*blend_open_for_click.borrow();
            *blend_open_for_click.borrow_mut() = open;
            blend_cmds.wake();
        });
    // egui `right.rs::appearance`：Opacity 与 Corner radius 同一行；圆角字段**后面**紧跟一颗
    // 24 pt「Individual corners」开关（`indep_ok` 才出现），展开后是两行四个独立圆角字段，
    // 第二行行尾才是「Corner smoothing」按钮（点开通用小窗）。
    // egui `paints.rs::appearance`：圆角行尾那颗 24 pt 开关的条件是 `smooth_ok`（不是 indep_ok），
    // 点它翻转 `indep_corners` 展开态；展开区才画四个独立圆角字段与 Corner smoothing。
    // 之前这里用 `corners_independent` 当条件，又给 Polygon/Star 在折叠态补画一颗平滑按钮，
    // 于是「折叠时平滑按钮还在」——改成一颗通用按钮控制展开，按钮本身不打开任何弹窗。
    let smooth_ok = shows_corner_smoothing(d.corner_smoothing_supported, d.corner_radius);
    let indep_open = *f.corners_individual.borrow();
    let indep_toggle = if smooth_ok {
        let flag = f.corners_individual.clone();
        let cmds = f.cmds.clone();
        Some(IconButton::new(Icon::AbsolutePosition).size(24.0).active(indep_open).on_click(move |_e, _w, _cx| {
            // ⛔ 不能写成 `*flag.borrow_mut() = !*flag.borrow()`：同一表达式里先可变借用再借用，
            // RefCell 会直接 panic（点一下卡死）。与 padding 的 `pad_individual` 同样拆两句。
            let next = !*flag.borrow();
            *flag.borrow_mut() = next;
            cmds.wake();
        }))
    } else {
        None
    };
    let mut sec = PanelSection::new(tr("Appearance")).header_trail(icon_row(vec![eye.into_any_element(), blend.into_any_element()]));
    let corner_row_trail: Vec<gpui::AnyElement> = indep_toggle.map(|b| b.into_any_element()).into_iter().collect();
    sec = sec.child(sublabel_row2(tr("Opacity"), tr("Corner radius"))).child(field_row(
        vec![
            // Opacity / corner radius 都是原版白名单里可绑定的数值字段
            // （`paints.rs::appearance` 用 `variable_ui::number_field_mixed`）。
            bindable(f, "opacity", d.bound.get("opacity").map(String::as_str), &d.number_variables, opacity.clone().into_any_element()),
            bindable(f, "cornerRadius", d.bound.get("cornerRadius").map(String::as_str), &d.number_variables, corner.clone().into_any_element()),
        ],
        corner_row_trail,
    ));
    // 展开区（egui 同序）：四个独立圆角字段（仅 indep_ok 的类型画，egui 的
    // `chunks(2).filter(|_| indep_ok)`）+ 平滑圆角按钮。折叠时整块不画——平滑按钮随之隐藏。
    if indep_open && smooth_ok {
        if d.corners_independent {
            sec = sec
                .child(field_row(vec![f.corner_tl.clone(), f.corner_tr.clone()], Vec::<IconButton>::new()))
                .child(field_row(vec![f.corner_bl.clone(), f.corner_br.clone()], vec![corner_smoothing_button(f, d).into_any_element()]));
        } else {
            // Polygon / Star 支持平滑但没有四个独立圆角字段：展开区里只留一行平滑按钮。
            sec = sec.child(field_row(Vec::<gpui::AnyElement>::new(), vec![corner_smoothing_button(f, d).into_any_element()]));
        }
    }
    // 用户：外观的混合模式是一个「选了才加进来的条目」——header 图标点开先给下拉选，
    // 选中后出现一行 dropdown + 右侧 24pt 网格里的减号，减号清除回 Normal 并收起这一行。
    let blend_label = f.blend_label.borrow().clone();
    if *f.blend_open.borrow() || blend_label != SharedString::from(tr("Normal")) {
        // 写回值用英文 serde 名（`blend` 只认 `Normal` / `Multiply` …）。
        let rows: Vec<(String, &'static str, Value)> = crate::panels::blend_rows()
            .into_iter()
            .map(|r| (tr(&r.label).to_string(), "node.set", json!({ "props": { "blend": r.label } })))
            .collect();
        let shown = tr(&blend_label).to_string();
        let minus_cmds = f.cmds.clone();
        let minus_open = f.blend_open.clone();
        let minus = IconButton::new(Icon::Minus).size(24.0).on_click(move |_e, _w, _cx| {
            minus_cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "blend": "Normal" } })) });
            *minus_open.borrow_mut() = false;
        });
        sec = sec.child(field_row_full_trailing(pick_dropdown(f, shown, row_w, rows), vec![minus]));
    }
    sec
}

#[cfg(test)]
mod corner_smoothing_tests {
    use super::shows_corner_smoothing;

    /// 用户反馈圆角为 0 时平滑圆角设置找不到：入口只看类型是否支持，圆角可后设。
    #[test]
    fn entry_only_needs_a_smoothable_kind() {
        assert!(!shows_corner_smoothing(false, 8.0));
        assert!(shows_corner_smoothing(true, 0.0));
        assert!(shows_corner_smoothing(true, 8.0));
    }
}

/// Typography 的 header 图标（egui `typography.rs`：Variable/Styles）。
/// Typography（egui `typography.rs` → `typography/fields.rs::font_rows` + `paragraph_rows`）。
///
/// 逐行照搬 egui：字体族 / 字重 / 字号 / 行高 / 字距 / 段落间距 + 自动缩放 + 水平·垂直对齐。
/// 写回都是 `node.set {"props": {…}}`（等价 egui 的 `apply_style` → `commit_text`）。
///
/// ⚠️ 与 egui 的一处差异：egui 的自动缩放与两组对齐是 `segmented`，图标是 `Hug`/`LayoutV`/
/// `Fixed`/`Dots`/`TextAlign*`，但冻结的 [`Icon`] 集里**没有这些图标**，而 `segmented` 只收
/// `(Icon, tooltip)`。按「缺组件不新建、仅此处用就内联」的约定，这里改用纯文本 `Dropdown`。
pub(crate) fn typography(f: &RightFields, d: &PanelData) -> PanelSection {
    let t = d.text.clone().unwrap_or_default();
    let var = variables_window_button(
        f,
        "typography-var",
        Icon::Variable,
        d.text_variables.iter().map(|v| (v.clone(), "variable.bind", json!({ "field": "characters", "variable": v }))).collect(),
    );
    let mut sec = PanelSection::new(tr("Typography")).header_trail(icon_row(vec![var, style_menu(f, d, "text", None, Icon::Dot)]));

    // ── 字体族（整行）── 打开带预览的选择窗（每项用它自己的字体渲染名称）。
    sec = sec.child(field_row_full(font_picker_button(f, t.font_family.clone().unwrap_or_else(|| tr("Mixed").into()), 240.0, t.font_family.clone())));

    // ── 字重 / 样式 + 字号 ──
    // 样式列表取决于字体族（egui `family_styles(fam)`）；没选族时 egui 提示 "Pick one font
    // family first"，这里用下拉的当前标签表达同一件事（菜单为空即点不开选项）。
    let styles: Vec<(String, &'static str, Value)> = match &t.font_family {
        Some(fam) => figcraft_text::family_styles(fam)
            .into_iter()
            .map(|s| {
                let q = json!({ "props": { "fontStyle": s.clone() } });
                (s, "node.set", q)
            })
            .collect(),
        None => Vec::new(),
    };
    let style_label = match t.font_style.clone() {
        Some(s) => s,
        None if styles.is_empty() => "Pick a font family".to_string(),
        None => tr("Mixed").to_string(),
    };
    sec = sec.child(field_row(
        vec![
            pick_dropdown(f, style_label, 160.0, styles).into_any_element(),
            // 字号可绑变量（`typography/fields.rs`：`variable_ui::number_field(..., "fontSize", ...)`）。
            bindable(f, "fontSize", d.bound.get("fontSize").map(String::as_str), &d.number_variables, f.font_size.clone().into_any_element()),
        ],
        Vec::<IconButton>::new(),
    ));

    // ── 行高 + 字距（池化字段，值由 `set_text_values` 每帧灌入）──
    sec = sec.child(field_row(vec![f.line_height.clone().into_any_element(), f.letter_spacing.clone().into_any_element()], Vec::<IconButton>::new()));

    // Paragraph spacing stays in the type-settings popover; this section keeps LH / LS only.

    // ── 水平 / 垂直对齐：两个通用 `Segmented`（egui `paragraph_rows` 的两个分段控件）──
    //
    // 图标集里没有 `TypeDialogJustify`，所以这里只放 3 格；`justified`（两端对齐）仍然是合法值，
    // 这时两格都不抬起 —— 正好用上 Segmented 的「无选中」状态（设计文件 `State=Rest`），
    // 而且 Justify 在 Type settings 的 Alignment 行里可选。
    let h_cmds = f.cmds.clone();
    let h = Segmented::icons(&[(Icon::AlignLeft, tr("Align left")), (Icon::AlignHCenter, tr("Align center")), (Icon::AlignRight, tr("Align right"))])
        .active(match t.align_h {
            Some("left") => Some(0),
            Some("center") => Some(1),
            Some("right") => Some(2),
            _ => None,
        })
        .on_pick(move |i: usize, _w: &mut Window, _cx: &mut App| {
            const V: [&str; 3] = ["left", "center", "right"];
            h_cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "textAlign": V[i.min(2)] } })) });
        });
    let v_cmds = f.cmds.clone();
    let v = Segmented::icons(&[(Icon::AlignTop, tr("Align top")), (Icon::AlignVCenter, tr("Align middle")), (Icon::AlignBottom, tr("Align bottom"))])
        .active(match t.align_v {
            Some("top") => Some(0),
            Some("middle") => Some(1),
            Some("bottom") => Some(2),
            _ => None,
        })
        .on_pick(move |i: usize, _w: &mut Window, _cx: &mut App| {
            const V: [&str; 3] = ["top", "middle", "bottom"];
            v_cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "textAlignVertical": V[i.min(2)] } })) });
        });
    sec.child(field_row(
        vec![h.into_any_element(), v.into_any_element()],
        // egui `paragraph_rows` 末尾那个 `Icon::TypeSettings` 按钮：点开详细排版设置
        // （`type_settings_panel`，Basics / Details / Variable）。原实现把这一行做成了「两个
        // 对齐下拉」就结束，缺了这个入口（用户 2026-11-11 指出）。
        vec![type_settings_button(f, d)],
    ))
}

/// 「Type settings」入口（egui `typography.rs::paragraph_rows` 末尾的 `Icon::TypeSettings`）。
///
/// egui 把它放在对齐那一行的行尾、24 pt；点开的是一个**独立的浮层**（带页签与关闭按钮），
/// 不是下拉菜单 —— 所以这里走通用 `WindowState`（`popovers::type_settings`）。
///
/// 图标用 `Icon::Menu`（三条横线 =「更多选项」）：egui 的 `Icon::TypeSettings` 字形在冻结的
/// 图标集里没有对应项，而 `Icon::Settings` 目前只是占位圆点（不是齿轮），画出来认不出。
fn type_settings_button(f: &RightFields, d: &PanelData) -> gpui::AnyElement {
    let popups = f.popups();
    let queue = f.queue();
    let text = d.text.clone().unwrap_or_default();
    let open = popups.is_open("type-settings");
    window_open_button("type-settings", Icon::Menu, open, move |window, cx| {
        let top = crate::popovers::anchored_top("type-settings", window);
        open_detached_with(popups.clone(), "type-settings", Some(top), cx, |anchor, cx| {
            crate::popovers::WindowState::type_settings(queue.clone(), &text, cx, anchor)
        });
    })
}

/// 矢量编辑模式下选中点的面板（egui `vector_panel.rs::points`）。
///
/// 只在 `data.vector` 存在时出现，**替换** Position / Auto layout / Arc / Star。`count == 0` 时
/// 只有一行提示（egui `widgets::note`）。写回全部走 `vector.edit.*`，引擎按当前点选区执行。
pub(crate) fn vector_points(f: &RightFields, d: &PanelData, cx: &mut App, n: &mut usize) -> PanelSection {
    use figcraft_doc::{HandleMirroring, StrokeCap, StrokeJoin};
    let Some(v) = d.vector.as_ref() else { return PanelSection::new(tr("Points")) };
    // Variable width 工具：右栏只画 Width point 节（egui `points` 开头同款分支）。
    if let Some(w) = &v.width_point {
        let round = |x: f64| x;
        let mut sec = PanelSection::new(tr("Width point"));
        sec = sec.child(sublabel_row2(tr("Position"), tr("Width")));
        let tf = f.row_field(
            cx,
            slot(n),
            &format!("{}", w.t),
            RowEditCtx {
                cmd: "vector.edit.widthPoint",
                params: json!({ "index": w.index }),
                key: "t",
                scale: 100.0,
                string_value: false,
                paint: None,
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        let wf = f.row_field(
            cx,
            slot(n),
            &format!("{}", round(w.width)),
            RowEditCtx {
                cmd: "vector.edit.widthPoint",
                params: json!({ "index": w.index }),
                key: "width",
                scale: 1.0,
                string_value: false,
                paint: None,
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        sec = sec.child(field_row(vec![tf, wf], Vec::<IconButton>::new()));
        sec = sec.child(sublabel_row2(tr("Left side"), tr("Right side")));
        let lf = f.row_field(
            cx,
            slot(n),
            &format!("{}", round(w.left)),
            RowEditCtx {
                cmd: "vector.edit.widthPoint",
                params: json!({ "index": w.index, "side": "left" }),
                key: "width",
                scale: 1.0,
                string_value: false,
                paint: None,
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        let rf = f.row_field(
            cx,
            slot(n),
            &format!("{}", round(w.right)),
            RowEditCtx {
                cmd: "vector.edit.widthPoint",
                params: json!({ "index": w.index, "side": "right" }),
                key: "width",
                scale: 1.0,
                string_value: false,
                paint: None,
                nest: None,
                nest_key: None,
                paint_color: None,
            },
        );
        return sec.child(field_row(vec![lf, rf], Vec::<IconButton>::new()));
    }
    let mut sec = PanelSection::new(if v.count > 1 { tr("Points") } else { tr("Point") });
    if v.count == 0 {
        return sec.child(field_row_full(
            div()
                .text_size(pt(11.0))
                .text_color(rgba(tokens().text_secondary))
                .child(SharedString::from(tr("Select points to edit their position, radius and mirroring"))),
        ));
    }
    sec = sec.child(sublabel_row(tr("Position"))).child(field_row(vec![f.vpt_x.clone(), f.vpt_y.clone()], Vec::<IconButton>::new()));
    if v.w.is_some() || v.h.is_some() {
        sec = sec.child(sublabel_row(tr("Dimensions"))).child(field_row(vec![f.vpt_w.clone(), f.vpt_h.clone()], Vec::<IconButton>::new()));
    }
    sec = sec.child(sublabel_row(tr("Corner radius"))).child(field_row(vec![f.vpt_radius.clone()], Vec::<IconButton>::new()));

    // Handle mirroring（egui `menu_popup` + `HandleMirroring::id`）。
    let mirror_label = match v.mirroring {
        Some(HandleMirroring::None) => tr("No mirroring"),
        Some(HandleMirroring::Angle) => tr("Mirror angle"),
        Some(HandleMirroring::AngleAndLength) => tr("Mirror angle and length"),
        None => tr("Mixed"),
    };
    let mirror_rows: Vec<(String, &'static str, Value)> = [HandleMirroring::None, HandleMirroring::Angle, HandleMirroring::AngleAndLength]
        .into_iter()
        .map(|m| {
            let label = match m {
                HandleMirroring::None => tr("No mirroring"),
                HandleMirroring::Angle => tr("Mirror angle"),
                HandleMirroring::AngleAndLength => tr("Mirror angle and length"),
            };
            (label.to_string(), "vector.edit.mirroring", json!({ "mode": m.id() }))
        })
        .collect();
    sec = sec.child(sublabel_row(tr("Mirroring"))).child(field_row_full(pick_dropdown(f, mirror_label, 200.0, mirror_rows)));

    // 这些点上的端点 / 连接样式（`Some(None)` = 跟图层一致，`None` = Mixed）。
    let mut cap_rows: Vec<(String, &'static str, Value)> =
        vec![(tr_noop("Same as layer").into(), "vector.edit.strokeCap", json!({ "cap": Value::Null }))];
    for c in StrokeCap::ALL {
        cap_rows.push((c.label().to_string(), "vector.edit.strokeCap", json!({ "cap": c })));
    }
    let mut join_rows: Vec<(String, &'static str, Value)> =
        vec![(tr("Same as layer").into(), "vector.edit.strokeJoin", json!({ "join": Value::Null }))];
    let join_name = |j: StrokeJoin| match j {
        StrokeJoin::Miter => tr("Miter"),
        StrokeJoin::Bevel => tr("Bevel"),
        StrokeJoin::Round => tr("Round"),
    };
    for j in StrokeJoin::ALL {
        join_rows.push((join_name(j).to_string(), "vector.edit.strokeJoin", json!({ "join": j })));
    }
    let cap_label = match v.cap {
        None => tr("Mixed"),
        Some(None) => tr("Same as layer"),
        Some(Some(c)) => c.label(),
    };
    let join_label = match v.join {
        None => tr("Mixed"),
        Some(None) => tr("Same as layer"),
        Some(Some(j)) => join_name(j),
    };
    sec.child(sublabel_row2(tr("End point"), tr("Join"))).child(field_row(
        vec![pick_dropdown(f, cap_label, 140.0, cap_rows).into_any_element(), pick_dropdown(f, join_label, 140.0, join_rows).into_any_element()],
        Vec::<IconButton>::new(),
    ))
}

/// Frame 工具（F）的预设面板（egui `frame_presets.rs`）。
///
/// 按 `FRAME_PRESETS` 的分类折叠列表：点分类折展、点预设推 `frame.create` + 切回 Move +
/// 缩放到新选区。展开状态存在 `RightFields`（egui 存在 `ui.data` 临时值里），默认只开 Phone。
pub(crate) fn frame_presets(f: &RightFields, _d: &PanelData) -> PanelSection {
    use figcraft_engine::cmd::create::FRAME_PRESETS;
    let mut sec = PanelSection::new(tr("Frame"));
    let mut cats: Vec<&'static str> = Vec::new();
    for (c, ..) in FRAME_PRESETS {
        if !cats.contains(c) {
            cats.push(c);
        }
    }
    let fmt = |v: f64| if v.fract() == 0.0 { format!("{}", v as i64) } else { format!("{v}") };
    for cat in cats {
        let open = {
            let map = f.preset_open.borrow();
            map.iter().find(|(c, _)| *c == cat).map(|(_, o)| *o).unwrap_or(cat == tr("Phone"))
        };
        let toggle_cmds = f.cmds.clone();
        let map = f.preset_open.clone();
        let head = div()
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .h_full()
            .gap(pt(6.0))
            .cursor_pointer()
            .on_mouse_down(MouseButton::Left, move |_e, _w, _cx| {
                {
                    let mut m = map.borrow_mut();
                    match m.iter_mut().find(|(c, _)| *c == cat) {
                        Some(e) => e.1 = !e.1,
                        None => m.push((cat, true)),
                    }
                }
                toggle_cmds.wake();
            })
            .child(IconButton::new(if open { Icon::ChevronDown } else { Icon::ChevronRight }).size(16.0))
            .child(div().text_size(pt(11.0)).child(SharedString::from(cat)));
        sec = sec.child(field_row_full(head));
        if !open {
            continue;
        }
        for (_, name, pw, ph) in FRAME_PRESETS.iter().filter(|(c, ..)| *c == cat) {
            let cmds = f.cmds.clone();
            let label = SharedString::from(*name);
            let size = SharedString::from(format!("{}×{}", fmt(*pw), fmt(*ph)));
            let preset = SharedString::from(*name);
            let line = div()
                .flex()
                .flex_row()
                .items_center()
                .w_full()
                .h_full()
                .gap(pt(6.0))
                .pl(pt(16.0))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, move |_e, _w, _cx| {
                    cmds.push(Cmd { id: "frame.create", params: Some(json!({ "preset": preset.clone() })) });
                    cmds.push(Cmd { id: "tool.set", params: Some(json!({ "tool": "move" })) });
                    cmds.push(Cmd { id: "view.zoomSelection", params: Some(json!({})) });
                })
                .child(div().flex_1().text_size(pt(11.0)).child(label))
                .child(div().text_size(pt(11.0)).text_color(rgba(tokens().text_secondary)).child(size));
            sec = sec.child(field_row_full(line));
        }
    }
    sec
}

/// Scale 工具面板（egui `scale_panel.rs`）：Dimensions（W/H）+ 倍数 + 3×3 锚点。
///
/// W/H/倍数的字段是 `RightFields` 里的池化 `TextField`，提交时经 `transform.scale` 生效
/// （描边、效果、圆角、文字都随图层一起缩放）；锚点点击推 `tool.scaleAnchor`。
pub(crate) fn scale(f: &RightFields, d: &PanelData, cx: &mut App) -> PanelSection {
    let mut sec = PanelSection::new(tr("Scale"));
    let Some(s) = d.scale else { return sec };
    // 每帧把当前包围盒 / 支点写进提交上下文，供 W / H 字段的 `on_commit` 换算倍数。
    *f.scale_ctx.borrow_mut() = s;
    let r2 = |v: f64| (v * 100.0).round() / 100.0;
    f.scale_w.update(cx, |tf, _| tf.set_value(format!("{}", r2(s.w))));
    f.scale_h.update(cx, |tf, _| tf.set_value(format!("{}", r2(s.h))));
    sec = sec
        .child(sublabel_row(tr("Dimensions")))
        .child(field_row(vec![f.scale_w.clone(), f.scale_h.clone()], Vec::<IconButton>::new()))
        .child(sublabel_row2(tr("Scale"), tr("Anchor")))
        .child(field_row(vec![f.scale_k.clone().into_any_element(), anchor_picker(f, s.anchor).into_any_element()], Vec::<IconButton>::new()));
    sec
}

/// 3×3 锚点点阵（egui `scale_panel.rs::anchor_picker`）。
///
/// 只在 Scale 节用一处，按「仅此处用就内联」的约定不抽成通用控件；48 pt 方框 + 9 个 16 pt
/// 命中格，选中的点画成 7 pt 实心 accent 方块，其余 4 pt 灰点。点击推 `tool.scaleAnchor`。
fn anchor_picker(f: &RightFields, cur: (u8, u8)) -> gpui::Div {
    let bg = tokens().field;
    let border = tokens().border;
    let accent = tokens().accent;
    let dim = tokens().text_secondary;
    let mut grid = div()
        .w(pt(48.0))
        .h(pt(48.0))
        .rounded(pt(5.0))
        .bg(rgba(bg))
        .border_1()
        .border_color(rgba(border))
        .flex()
        .flex_col()
        .items_center()
        .justify_center();
    for row in 0..3u8 {
        let mut line = div().flex().flex_row().items_center();
        for col in 0..3u8 {
            let on = (col, row) == cur;
            let cmds = f.cmds.clone();
            let size = if on { 7.0 } else { 4.0 };
            line = line.child(
                div()
                    .w(pt(16.0))
                    .h(pt(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_e, _w, _cx| {
                        cmds.push(Cmd { id: "tool.scaleAnchor", params: Some(json!({ "anchor": figcraft_engine::tools::anchor_name((col, row)) })) });
                    })
                    .child(div().w(pt(size)).h(pt(size)).rounded(pt(if on { 2.0 } else { 1.0 })).bg(rgba(if on { accent } else { dim }))),
            );
        }
        grid = grid.child(line);
    }
    grid
}

/// 网格 auto layout 的行列轨道（egui `layout_panel.rs::tracks`）。
///
/// 每轴一组：`Column sizes` / `Row sizes` 标题 + 每条轨道一行（序号 + 尺寸 + sizing 下拉）。
/// Fill 轨道的数值是 fr 份数，Fixed / Hug 显示实际 px 尺寸 —— 与 egui 完全一致。
/// 数值框复用行编辑器池（`f.row_field`）：轨道条数随设计变化，不能每帧新建 retained 实体。
/// 写回：`layout.setTrack {axis, index, sizing, value}`（改尺寸）或省略 `value`（改 sizing）。
pub(crate) fn grid_track_rows(f: &RightFields, d: &PanelData, cx: &mut App, n: &mut usize, mut sec: PanelSection) -> PanelSection {
    let Some(t) = d.grid_tracks.as_ref() else { return sec };
    if t.columns.is_empty() && t.rows.is_empty() {
        return sec;
    }
    for (axis, tracks, caption) in [("columns", &t.columns, tr("Column sizes")), ("rows", &t.rows, tr("Row sizes"))] {
        if tracks.is_empty() {
            continue;
        }
        sec = sec.child(sublabel_row(caption));
        for (i, track) in tracks.iter().enumerate() {
            // ego `tracks`: Fill 编辑 fr；Fixed / Hug 编辑（并显示）px。
            let shown = if track.sizing == "fill" { track.value } else { track.size };
            let s = slot(n);
            let edit = f.row_field(
                cx,
                s,
                &format!("{shown}"),
                RowEditCtx {
                    cmd: "layout.setTrack",
                    params: json!({ "axis": axis, "index": i, "sizing": track.sizing }),
                    key: "value",
                    scale: 1.0,
                    string_value: false,
                    paint: None,
                    nest: None,
                    nest_key: None,
                    paint_color: None,
                },
            );
            let sizes: Vec<(String, &'static str, Value)> =
                [(tr_noop("Fixed"), "fixed"), (tr_noop("Hug contents"), "hug"), (tr_noop("Fill (fr)"), "fill")]
                    .into_iter()
                    .map(|(l, v)| (l.to_string(), "layout.setTrack", json!({ "axis": axis, "index": i, "sizing": v })))
                    .collect();
            let label = match track.sizing {
                "fixed" => tr("Fixed"),
                "hug" => tr("Hug"),
                _ => tr("Fill"),
            };
            let line = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(pt(6.0))
                .child(div().w(pt(18.0)).child(SharedString::from(format!("{}", i + 1))))
                .child(div().flex_1().min_w(px(0.0)).child(edit))
                .child(pick_dropdown(f, label, 140.0, sizes));
            sec = sec.child(field_row_full(line));
        }
    }
    sec
}

pub(crate) fn variable_modes(f: &RightFields, d: &PanelData, row_w: f32) -> Option<PanelSection> {
    if d.variable_modes.is_empty() {
        return None;
    }
    let t = tokens();
    let mut sec = PanelSection::new(tr("Variable modes"));
    for m in &d.variable_modes {
        // egui `paints.rs::variable_modes`: both the collection and the mode are sent as **ids**,
        // not their display names; `Auto` is `mode: null`.
        let rows: Vec<(String, &'static str, Value)> = m
            .options
            .iter()
            .zip(m.option_ids.iter())
            .map(|(o, id)| (o.clone(), "variable.setMode", json!({ "collection": m.collection_id, "mode": id.clone() })))
            .collect();
        // egui `paints.rs::variable_modes`：集合名是 `regular(11.0)` + `t.text`，模式下拉在右。
        //
        // 之前这里是一行手写的 `div().flex().flex_row()`：名字没给字号（继承到面板正文的
        // 13 pt+，比整栏高一行，看着「肿」），后面的下拉又带 `w_full`——在 `flex_row` 里
        // `w_full` 是父宽的 100%，于是整行被名字顶宽、下拉溢出右边界，左右两侧完全不对等。
        // 现在走统一网格：左列 `1fr` 放名字（11 pt、超长裁切），右列 `1fr` 放模式下拉。
        let name = div().w_full().overflow_hidden().text_size(pt(11.0)).text_color(rgba(t.text)).child(SharedString::from(m.collection.clone()));
        let pick = pick_dropdown(f, m.current.clone(), row_w, rows);
        sec = sec.child(field_row(vec![name.into_any_element(), pick.into_any_element()], Vec::<gpui::AnyElement>::new()));
    }
    Some(sec)
}

pub(crate) fn section(f: &RightFields, d: &PanelData, row_w: f32) -> Option<PanelSection> {
    let s = d.section.as_ref()?;
    // egui `section_panel::section`: the "Section color" block only exists for a Section layer
    // (`is_section`); a rectangle / text / frame has no "Section" section at all. It was drawn
    // for every layer here, which is the stray "Section" row the user saw.
    if !s.is_section {
        return None;
    }
    let mut sec = PanelSection::new(tr("Section"));
    if s.is_section {
        // 8 个 Section color 色板（`section_panel.rs::SECTION_COLORS`）。
        let colors = [
            (tr("White"), "#FFFFFF"),
            (tr("Gray"), "#EDEDED"),
            (tr("Red"), "#FFE0DE"),
            (tr("Orange"), "#FFE8D1"),
            (tr("Yellow"), "#FFF4C2"),
            (tr("Green"), "#DDF5E3"),
            (tr("Blue"), "#DCEBFF"),
            (tr("Violet"), "#EBE3FF"),
        ];
        let mut swatches = div().flex().flex_row().items_center().gap(pt(6.0));
        for (_, hex) in colors {
            let c = Color32::from_hex(hex).unwrap_or(Color32::WHITE);
            let cmds = f.cmds.clone();
            let hex = hex.to_string();
            swatches = swatches.child(
                div().size(pt(18.0)).rounded(pt(9.0)).bg(rgba(c)).border_1().border_color(rgba(tokens().border)).cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    move |_e, _w, _cx| {
                        cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "fills": [hex] } })) });
                    },
                ),
            );
        }
        sec = sec.child(sublabel_row(tr("Section color"))).child(field_row_full(swatches));
        sec = sec.child(field_row(
            vec![pick_dropdown(
                f,
                if s.hide_contents { "Hide contents ✓" } else { tr("Hide contents") },
                row_w,
                vec![
                    ("Show contents".into(), "section.toggleContents", json!({ tr("hidden"): false })),
                    (tr("Hide contents").into(), "section.toggleContents", json!({ tr("hidden"): true })),
                ],
            )],
            Vec::<IconButton>::new(),
        ));
    }
    sec = sec.child(sublabel_row(tr("Status"))).child(field_row(
        vec![pick_dropdown(
            f,
            s.status.clone(),
            row_w,
            vec![
                (tr("No status").into(), "node.set", json!({ "props": { "devStatus": "none" } })),
                (tr("Ready for dev").into(), "node.set", json!({ "props": { "devStatus": "readyForDev" } })),
                (tr("Completed").into(), "node.set", json!({ "props": { "devStatus": "completed" } })),
            ],
        )],
        Vec::<IconButton>::new(),
    ));
    if s.changed {
        sec = sec.child(field_row(
            vec![pick_dropdown(f, "Resolve — back to Ready for dev", row_w, vec![(tr("Resolve").into(), "dev.resolveChange", json!({}))])],
            Vec::<IconButton>::new(),
        ));
    }
    sec = sec.child(field_row(
        vec![pick_dropdown(f, tr("Resize to fit"), row_w, vec![(tr("Resize to fit").into(), "object.resizeToFit", json!({}))])],
        Vec::<IconButton>::new(),
    ));
    Some(sec)
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Selection header —— `参考实现 right.rs::header` 的判断规则 1:1
// ───────────────────────────────────────────────────────────────────────────────────────────────

/// 标题行菜单里的一个行模型：参考实现 `menu_item` / `menu_check` / `Menu::separator`。
/// `cmd` 为空 = 当前已选中的勾选项，点了不推命令（egui `if to_frame != frame`）。
#[derive(Clone)]
struct HeadRow {
    label: String,
    cmd: &'static str,
    params: Value,
    /// `Some(on)` = 勾选行 / 保留勾选列；`None` = 普通行。
    checked: Option<bool>,
    separator: bool,
}

impl HeadRow {
    fn plain(label: impl Into<String>, cmd: &'static str, params: Value) -> Self {
        Self { label: label.into(), cmd, params, checked: None, separator: false }
    }
    fn check(label: impl Into<String>, cmd: &'static str, params: Value, on: bool) -> Self {
        Self { checked: Some(on), ..Self::plain(label, cmd, params) }
    }
    /// egui `menu_item_beside_checks`：普通行，但要占住勾选列。
    fn beside_checks(label: impl Into<String>, cmd: &'static str, params: Value) -> Self {
        Self { checked: Some(false), ..Self::plain(label, cmd, params) }
    }
    fn sep(mut self) -> Self {
        self.separator = true;
        self
    }
}

fn head_menu(rows: &[HeadRow]) -> Vec<MenuRow> {
    rows.iter()
        .map(|r| {
            let mut m = MenuRow::new(r.label.clone());
            if let Some(on) = r.checked {
                m = m.checked(Some(on));
            }
            if r.separator {
                m = m.separator();
            }
            m
        })
        .collect()
}

/// 选中一行就推它的命令（参考实现 `menu_popup(&resp).show(...)`）。
fn head_pick(rows: Vec<HeadRow>, cmds: crate::panels::Cmds) -> Rc<dyn Fn(&str, &mut Window, &mut App)> {
    Rc::new(move |label, _w, _cx| {
        if let Some(r) = rows.iter().find(|r| r.label == label) {
            if !r.cmd.is_empty() {
                cmds.push(Cmd { id: r.cmd, params: Some(r.params.clone()) });
            }
        }
    })
}

/// 类型名的文字下拉（参考实现 `header` 的 `lr`：86×32）。Frame / Group 才有；Frame 的菜单
/// 还接着 `FRAME_PRESETS`，每个分类前一条分隔线（`kind_items` + preset 列表）。
fn head_dropdown(f: &RightFields, label: String, rows: Vec<HeadRow>) -> Dropdown {
    let menu = head_menu(&rows);
    let mut d = Dropdown::new(label, 86.0, None);
    if let Some(root) = f.root.clone() {
        let cmds = f.cmds.clone();
        d = d.on_click(move |anchor, _window, cx: &mut App| {
            let on_pick = head_pick(rows.clone(), cmds.clone());
            root.update(cx, |cv, cx| cv.open_menu(anchor, Some(260.0), menu.clone(), on_pick, cx));
        });
    }
    d
}

/// 一个图标按钮，点开标题行的菜单（参考实现 `menu_popup(&resp)`）。
fn head_menu_button(f: &RightFields, icon: Icon, side: f32, width: f32, rows: Vec<HeadRow>) -> IconButton {
    let menu = head_menu(&rows);
    let cmds = f.cmds.clone();
    let root = f.root.clone();
    IconButton::new(icon).size(side).on_click(move |_e, window, cx: &mut App| {
        let Some(root) = root.clone() else { return };
        let anchor = Bounds { origin: window.mouse_position(), size: size(px(0.0), px(0.0)) };
        let on_pick = head_pick(rows.clone(), cmds.clone());
        root.update(cx, |cv, cx| cv.open_menu(anchor, Some(width), menu.clone(), on_pick, cx));
    })
}

/// 「Create link」：参考实现 `menus::invoke("text.link")` 先弹输入框拿 URL，再执行 `text.link`。
/// GPUI 的通用单值对话框（A8/A17 同款）就是那个输入框。
fn link_button(f: &RightFields) -> IconButton {
    let root = f.root.clone();
    IconButton::new(Icon::LinkOn).size(24.0).on_click(move |_e, _window, cx: &mut App| {
        let Some(root) = root.clone() else { return };
        root.update(cx, |cv, cx| cv.open_link_dialog(cx));
    })
}

/// 选区标题行（参考实现 `right.rs::header`）。
///
/// ⛔ **一行 40 pt、没有区块标题**：左边 86 pt 的类型名（Frame / Group 可下拉互转），
/// 右边按选中类型整套换按钮。所有显隐都走 [`crate::panels::visibility::header_actions`]，
/// 规则逐条对应 参考实现 的 `is_frame / is_group / is_text / edit_id / points_selected`：
///
/// * 单选 Frame → 类型下拉里还有 `FRAME_PRESETS`（分类分隔）+ 右侧「交换方向」；
/// * 选区全是 Text → 四个专属按钮（Select matching / Create link / Create component / More），
///   **不画**普通图层的布尔 / 蒙版按钮；
/// * 单选 Component / ComponentSet，或矢量编辑已选点 → 右侧动作一个都不画；
/// * 其余 → Select matching / Create component / Mask / 布尔按钮 + 布尔菜单 / Edit object（单选形状）。
///
/// 与 `view_bar` 一样是**独立自由行**：参考实现 用 `allocate_exact_size(w, 40)` 后绝对定位左右
/// 两块，冻结的统一网格（`field_row_h` 的 2×1fr + 24pt 列）放不下 6 个动作格 + 86 pt 标签。
pub(crate) fn header(f: &RightFields, d: &PanelData, col: f32) -> Option<gpui::Div> {
    let h = d.header.as_ref()?;
    let _ = col;
    let t = tokens();
    let sel = crate::panels::HeaderSel {
        is_frame: h.is_frame,
        is_group: h.is_group,
        is_text: h.is_text,
        is_component: h.is_component,
        is_shape: h.is_shape,
        points_selected: d.vector.as_ref().is_some_and(|v| v.has_selection),
        count: h.count,
    };
    let a = visibility::header_actions(&sel);

    // ── 左：类型名 / 类型下拉 ─────────────────────────────────────────────────────
    let lead: gpui::AnyElement = if a.type_menu {
        // `kind_items`: 勾选项点了什么都不做，只有转换成另一种才推命令。
        let mut rows = vec![
            HeadRow::check(tr("Frame"), if h.is_frame { "" } else { "object.convertToFrame" }, json!({}), h.is_frame),
            HeadRow::check(tr("Group"), if h.is_group { "" } else { "object.convertToGroup" }, json!({}), h.is_group),
        ];
        if h.is_frame {
            use figcraft_engine::cmd::create::FRAME_PRESETS;
            let mut cat = "";
            for (c, name, w, hh) in FRAME_PRESETS {
                let mut r = HeadRow::plain(*name, "node.set", json!({ "props": { "width": w, "height": hh } }));
                if *c != cat {
                    r = r.sep();
                    cat = c;
                }
                rows.push(r);
            }
        }
        head_dropdown(f, tr(&h.label).to_string(), rows).into_any_element()
    } else {
        // 其它类型没有下拉，连 chevron 都不画：就是一段 13 pt 半粗文字。
        // 补 8 pt 内缩 = 有下拉类型 Dropdown 内部的 LEAD_PAD(8)；加上 header 行的 pl(8)，
        // 所有类型名文字统一距左 16，与下方各 section 的 PAD(16) 对齐（Rectangle/Text 不对齐即此因）。
        div()
            .pl(pt(8.0))
            .text_size(pt(13.0))
            .font_weight(crate::panels::UI_FONT_REGULAR)
            .text_color(rgba(t.text))
            .child(SharedString::from(tr(&h.label).to_string()))
            .into_any_element()
    };

    // ── 右：按选中类型整套换按钮 ───────────────────────────────────────────────
    let mut actions: Vec<gpui::AnyElement> = Vec::new();
    // 「交换方向」在 Frame 上挂到动作列最左（参考实现 把它对齐到 4 个动作格 + chevron 的左边）。
    if a.swap_orientation {
        actions.push(icon_cmd(Icon::Rotate, "frame.swapOrientation", json!({}), f, false).into_any_element());
    }
    if a.text {
        // 参考实现 `is_text` 分支的四个按钮，左→右顺序照抄。
        actions.push(icon_cmd(Icon::Search, "select.matching", json!({}), f, false).into_any_element());
        actions.push(link_button(f).into_any_element());
        actions.push(icon_cmd(Icon::Component, "component.create", json!({}), f, false).into_any_element());
        let more: Vec<HeadRow> = h
            .more_actions
            .iter()
            .map(|(label, cmd, sep)| {
                let r = HeadRow::plain(label.clone(), cmd, json!({}));
                if *sep { r.sep() } else { r }
            })
            .collect();
        actions.push(head_menu_button(f, Icon::Menu, 24.0, 240.0, more).into_any_element());
    } else if a.quick {
        // 参考实现：从右往左布局，所以这里左→右是
        // Select matching / Create component / Mask / Union / 布尔 chevron / Edit object。
        actions.push(icon_cmd(Icon::Search, "select.matching", json!({}), f, false).into_any_element());
        actions.push(icon_cmd(Icon::Component, "component.create", json!({}), f, false).into_any_element());
        actions.push(icon_cmd(Icon::Image, "object.mask", json!({}), f, false).into_any_element());
        let bool_icon = match h.bool_op {
            Some(figcraft_doc::BoolOp::Subtract) => Icon::Subtract,
            Some(figcraft_doc::BoolOp::Intersect) => Icon::Intersect,
            Some(figcraft_doc::BoolOp::Exclude) => Icon::Exclude,
            _ => Icon::Union,
        };
        let bool_button = if visibility::bool_union_enabled(h.count) {
            icon_cmd(bool_icon, "bool.union", json!({}), f, false)
        } else {
            // 参考实现：少于两个图层时点了也不执行，按钮只是当前运算的状态图标。
            IconButton::new(bool_icon)
        };
        let cur = h.bool_op.map(|o| o.label());
        let mut bool_rows: Vec<HeadRow> = Vec::new();
        for (label, cmd) in
            [(tr("Union"), "bool.union"), (tr("Subtract"), "bool.subtract"), (tr("Intersect"), "bool.intersect"), (tr("Exclude"), "bool.exclude")]
        {
            let r = match cur {
                Some(c) => HeadRow::check(label, cmd, json!({}), c == label),
                None => HeadRow::plain(label, cmd, json!({})),
            };
            bool_rows.push(r);
        }
        // Flatten：有当前运算时用 `menu_item_beside_checks` 占住勾选列。
        let flatten = if cur.is_some() {
            HeadRow::beside_checks(tr("Flatten"), "vector.flatten", json!({}))
        } else {
            HeadRow::plain(tr("Flatten"), "vector.flatten", json!({}))
        };
        bool_rows.push(flatten.sep());
        // 布尔方式 = 通用 24 图标 + 16 宽下拉箭头、两者间距 0（用户规则）。
        // 下拉箭头宽 16（不是图标 size16），高 24；整体 40 宽作为一个 action。
        let bool_menu = head_menu_button(f, Icon::ChevronDown, 24.0, 180.0, bool_rows);
        actions.push(
            div()
                .flex()
                .flex_row()
                .items_center()
                .child(bool_button.into_any_element())
                .child(div().w(pt(16.0)).h(pt(24.0)).flex().items_center().justify_center().child(bool_menu.into_any_element()))
                .into_any_element(),
        );
        if a.edit_object {
            actions.push(icon_cmd(Icon::Pen, "vector.edit.begin", json!({}), f, false).into_any_element());
        }
    }

    Some(
        div()
            .flex_none()
            .w_full()
            .h(pt(40.0))
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(pt(4.0))
            .pl(pt(8.0))
            .pr(pt(8.0))
            .child(div().flex_none().w(pt(86.0)).flex().items_center().child(lead))
            .child(div().flex().flex_row().items_center().gap(pt(4.0)).children(actions)),
    )
}

// ───────────────────────────────────────────────────────────────────────────────────────────────
// Local styles 列表：1:1 照抄 ui-egui `widgets/style_picker.rs::local_styles` 的微调参数。
// 上游把这套尺寸逐项调过（16 pt 网格 / 24 pt 行 / 8-16-16 分组间距 / 圆形色板 / 整行拖动），
// 任何一项都不得凭感觉改动。
// ───────────────────────────────────────────────────────────────────────────────────────────────
/// Side inset of a row: the hover band starts and ends here, not at the panel's edges.
const LS_INSET: f32 = 8.0;
/// Where a row's content (and a caption's text) starts from the panel's edge.
const LS_PAD: f32 = 16.0;
/// Height of a group's caption row ("Text styles").
const LS_CAPTION_H: f32 = 32.0;
/// Height of a folder row (参考实现 `local_styles`: `allocate_exact_size(vec2(w, 24.0))`).
const LS_FOLDER_H: f32 = 24.0;
/// Height of a style row (用户 2026-11-15：24 pt，与左栏 Pages 行块一致)。
const LS_ROW_H: f32 = 24.0;
/// 行块之间 8 pt（用户 2026-11-15）。spacer 与 `y` 必须同步推进。
const LS_ROW_GAP: f32 = 8.0;
/// A row's icon box, and the gap between it and the name.
const LS_ICON: f32 = 16.0;
const LS_ICON_GAP: f32 = 8.0;
/// The hit area of a row's leading icon: the icon draws at 16, but the click target is 24×24 —
/// the same size as the trailing settings button (用户 2026-11-15：只有点这两个图标才弹设置页)。
const LS_ICON_HIT: f32 = 24.0;
/// How far a nesting level steps in: an icon box and its gap, so a child's icon sits under the
/// folder's name.
const LS_STEP: f32 = LS_ICON + LS_ICON_GAP;
/// 每层的固定缩进起点（band 内坐标）：行基准 + depth * LS_STEP。
/// 子项图标因此正好落在父项文字左边（用户 2026-11-15：每层按同一个固定值缩进）。
fn ls_indent(depth: usize) -> f32 {
    LS_PAD - LS_INSET + depth as f32 * LS_STEP
}
/// A folder's chevron, inside its 16 pt box.
const LS_CHEVRON: f32 = 12.0;
/// Gap above the first group's caption, and above every later one.
const LS_FIRST_GAP: f32 = 8.0;
const LS_GROUP_GAP: f32 = 16.0;
/// Gap under the whole list.
const LS_BOTTOM_PAD: f32 = 16.0;
/// A row's trailing text (a text style's size and line height) ends this far from the band's edge.
const LS_TRAIL: f32 = 8.0;
/// Pointer travel before a press becomes a drag (egui `drag_started`).
const LS_DRAG_DEADZONE: f32 = 3.0;

/// 一行落点槽（egui `Slot`）：文件夹头或样式行，y 是**相对列表容器**的坐标。
#[derive(Clone)]
pub struct LsSlot {
    pub ty: &'static str,
    pub y0: f32,
    pub y1: f32,
    /// 树深度：folder 行 depth=d，其下样式/子 folder 为 d+1（供「父选中带子高亮」用）。
    pub depth: usize,
    pub kind: LsSlotKind,
}

#[derive(Clone)]
pub enum LsSlotKind {
    Header { path: String },
    Style { index: usize, id: String, name: String },
}

/// 一次拖动（egui `ListDrag` 存在 `ui.data` 的 temp data）。
pub struct LocalStyleDrag {
    pub ty: &'static str,
    /// 组内样式序号（egui 每组 `styles` 的计数）。
    pub from: usize,
    pub id: String,
    pub label: String,
    /// 打开样式编辑器用的快照（egui `edit_local` 从 doc 读，这里渲染期备好，事件期不回读）。
    pub name: String,
    pub description: String,
    pub properties: Vec<String>,
    /// 颜色样式的完整 paints（编辑器预览块 + 属性行取色器写回）。
    pub paints: Vec<figcraft_color::Paint>,
    /// 文本样式的完整 `TextStyle` JSON（字号 / 行高 / 字距编辑写回）。
    pub text: Option<serde_json::Value>,
    /// 效果样式的完整 effects 数组（追加 / 删除 / 显隐写回）。
    pub effects: Option<Vec<crate::panels::right::data::EffectRow>>,
    /// 网格样式的完整 grids 数组（追加 / 删除 / 显隐写回）。
    pub grids: Option<Vec<crate::panels::right::data::GridRow>>,
    pub start: Point<Pixels>,
    pub pointer: Point<Pixels>,
    pub armed: bool,
    /// 插入线的相对 y；None = 无处可落 / 空操作。
    pub line_y: Option<f32>,
    pub query: Option<Value>,
}

/// 键盘导航的一条可见样式行：带上打开编辑器所需的快照，按键时不用回读文档。
#[derive(Clone)]
pub struct LocalStyleKeyRow {
    /// 相对列表容器的 y，用来画焦点框。
    pub y0: f32,
    pub ty: &'static str,
    pub id: String,
    pub name: String,
    pub description: String,
    pub properties: Vec<String>,
    pub paints: Vec<figcraft_color::Paint>,
    pub text: Option<serde_json::Value>,
    pub effects: Option<Vec<crate::panels::right::data::EffectRow>>,
    pub grids: Option<Vec<crate::panels::right::data::GridRow>>,
}

/// 列表的跨帧状态：渲染期写落点表，事件期读。
#[derive(Default)]
pub struct LocalStyleUi {
    pub drag: Option<LocalStyleDrag>,
    pub slots: Vec<LsSlot>,
    /// 全文档样式的 (id, name)，供 `moved_name` 查重。
    pub names: Vec<(String, String)>,
    /// 列表容器在窗口里的左上角（prepaint 写入），把指针换成相对坐标。
    pub origin: Option<(f32, f32)>,
    /// 键盘导航：按绘制顺序的样式行。
    pub rows: Vec<LocalStyleKeyRow>,
    /// 当前键盘选中的 `rows` 下标（`Tab` 聚焦列表后由方向键移动）。
    pub sel: Option<usize>,
    /// 选中的文件夹 key = `"{ty}/{path}"`（与 `local_style_folder` 的折叠 key 一致）。
    /// 与 `sel` 互斥：选中样式清空它，选中文件夹清空 `sel`。
    pub sel_folder: Option<String>,
    /// 复制下来的样式（`Ctrl/Cmd+C`）：粘贴时用它 `style.create` 出一份新样式。
    pub clip: Option<LocalStyleKeyRow>,
}

/// egui `style_picker::moved_name`：落进目标文件夹后取叶子名，重名则编号（"Sunset/600 2"）。
fn ls_moved_name(names: &[(String, String)], moved_id: &str, moved: &str, folder: &str) -> String {
    let leaf = moved.rsplit('/').next().unwrap_or(moved);
    let named = |l: &str| if folder.is_empty() { l.to_string() } else { format!("{folder}/{l}") };
    let taken = |n: &str| names.iter().any(|(id, name)| id != moved_id && name == n);
    let first = named(leaf);
    if !taken(&first) {
        return first;
    }
    (2..).map(|k| named(&format!("{leaf} {k}"))).find(|n| !taken(n)).unwrap_or(first)
}

/// egui `style_picker::drop_move` 1:1：按「有多少槽的上沿在指针之上」定插入缝，算出
/// `style.move` 的 `before` / `name`（把样式改名的同时挪进文件夹），并给出插入线的 y。
/// `at_y` 是相对列表容器的 y；文件夹头注册为 Header，两侧都能接落点。
fn ls_drop_move(all: &[LsSlot], names: &[(String, String)], ty: &'static str, from: usize, at_y: f32) -> Option<(f32, Value)> {
    // 每个类型组是一张独立的列表（egui `ListDrag::in_order(ui, ("local_styles", slot.key()))`）。
    let slots: Vec<&LsSlot> = all.iter().filter(|s| s.ty == ty).collect();
    let (fp, moved_id, moved_name) = slots.iter().enumerate().find_map(|(i, s)| match &s.kind {
        LsSlotKind::Style { index, id, name } if *index == from => Some((i, id.clone(), name.clone())),
        _ => None,
    })?;
    let (line, y) = match slots.iter().filter(|s| s.y0 <= at_y).count().checked_sub(1) {
        None => (0, slots.first()?.y0),
        Some(i) => {
            let r = slots[i];
            let over_caption = matches!(r.kind, LsSlotKind::Header { .. }) && at_y < (r.y0 + r.y1) / 2.0;
            match (over_caption, slots.get(i + 1)) {
                (true, _) => (i, r.y0),
                (false, Some(next)) => (i + 1, next.y0),
                (false, None) => (i + 1, r.y1),
            }
        }
    };
    let before = slots.iter().skip(line).find_map(|s| match &s.kind {
        LsSlotKind::Style { id, .. } if *id != moved_id => Some(id.clone()),
        _ => None,
    });
    let folder_of = |s: &LsSlot| match &s.kind {
        LsSlotKind::Style { name, .. } => ls_folder(name),
        LsSlotKind::Header { path } => path.clone(),
    };
    let into = match slots.get(line).copied() {
        Some(s) => match &s.kind {
            LsSlotKind::Style { name, .. } => ls_folder(name),
            LsSlotKind::Header { .. } => match line.checked_sub(1).map(|i| slots[i]) {
                Some(prev) => match &prev.kind {
                    LsSlotKind::Style { name, .. } => ls_folder(name),
                    LsSlotKind::Header { path } => path.clone(),
                },
                None => String::new(),
            },
        },
        None => folder_of(slots.last().copied()?),
    };
    // 落在自己原来的位置不算移动。
    if (line == fp || line == fp + 1) && ls_folder(&moved_name) == into {
        return None;
    }
    let name = ls_moved_name(names, &moved_id, &moved_name, &into);
    let mut q = json!({ "styles": [moved_id] });
    if let Some(b) = before {
        q["before"] = json!(b);
    }
    if name != moved_name {
        q["name"] = json!(name);
    }
    Some((y, q))
}

/// Page 节「Local styles」的整个列表块（egui `style_picker::local_styles` 1:1）。
///
/// 不用 `field_row_full`：那是统一网格的 32 pt 行 + 右端 24 pt trailing 列，装不下上游
/// 微调过的 24 pt 行距、8 pt 侧内缩和 8/16/16 分组间距。整个列表作为一个 free block
/// 自己排版，才能逐项对齐 `LS_*`，也才能把拖动事件绑在一张覆盖全表的容器上。
fn local_styles_block(f: &RightFields, d: &PanelData, window: &mut Window, cx: &mut App) -> Div {
    let t = tokens();
    let ui = f.local_styles_ui.clone();
    let wake = f.cmds.clone();
    let cmds = f.cmds.clone();
    let folded = f.local_style_folded.clone();
    let mut slots: Vec<LsSlot> = Vec::new();
    let names: Vec<(String, String)> = d.styles.iter().map(|s| (s.id.clone(), s.name.clone())).collect();
    let mut key_rows: Vec<LocalStyleKeyRow> = Vec::new();
    let mut rows: Vec<Div> = Vec::new();
    let mut y = 0.0;
    let mut groups = 0usize;
    static GROUPS: &[(&'static str, &'static str)] =
        &[("text", "Text styles"), ("paint", "Color styles"), ("effect", "Effect styles"), ("grid", "Grid styles")];

    for &(ty, cap_key) in GROUPS {
        let list: Vec<&LocalStyleRow> = d.styles.iter().filter(|s| s.ty == ty).collect();
        if list.is_empty() {
            continue;
        }
        // 组间距：第一组 8 pt，其后每组 16 pt（egui `LS_FIRST_GAP` / `LS_GROUP_GAP`）。
        let gap = if groups == 0 { LS_FIRST_GAP } else { LS_GROUP_GAP };
        y += gap;
        rows.push(div().w_full().h(pt(gap)));
        groups += 1;

        // 组标题 32 pt + 该组的 `+`（直接建这种类型，egui `row_plus` 同位置）。
        let create_params = ls_create_params(d, ty, "");
        let cmds_c = cmds.clone();
        let plus = IconButton::new(Icon::Plus).size(24.0).on_click(move |_anchor, _w, _cx| {
            cmds_c.push(Cmd { id: "style.create", params: Some(create_params.clone()) });
        });
        rows.push(local_style_caption(SharedString::from(tr(cap_key)), plus));
        y += LS_CAPTION_H;

        let mut style_no = 0usize;
        // 用户 2026-11-15：组内每个行块之间 8 pt。`y` 必须与 spacer 同步推进，否则
        // LsSlot 的落点表与渲染错位，拖动排序的插入线会画到上一行。
        let mut rows_in_group = 0usize;
        for line in ls_entries(&list, &|p| folded.borrow().contains(&format!("{}/{}", ty, p))) {
            if rows_in_group > 0 {
                y += LS_ROW_GAP;
                rows.push(div().w_full().h(pt(LS_ROW_GAP)));
            }
            rows_in_group += 1;
            match line {
                LsLine::Folder { path, label, depth } => {
                    slots.push(LsSlot { ty, y0: y, y1: y + LS_FOLDER_H, depth, kind: LsSlotKind::Header { path: path.clone() } });
                    rows.push(local_style_folder(f, d, &label, &path, ty, depth, window, cx));
                    y += LS_FOLDER_H;
                }
                LsLine::Style(idx, depth) => {
                    let s = &list[idx];
                    // 这一行在 `rows`（跨类型、按绘制顺序）里的下标：单击选中 / 双击改名都用它。
                    let key_rows_len_before = key_rows.len();
                    slots.push(LsSlot {
                        ty,
                        y0: y,
                        y1: y + LS_ROW_H,
                        depth,
                        kind: LsSlotKind::Style { index: style_no, id: s.id.clone(), name: s.name.clone() },
                    });
                    key_rows.push(LocalStyleKeyRow {
                        y0: y,
                        ty,
                        id: s.id.clone(),
                        name: s.name.clone(),
                        description: s.description.clone(),
                        properties: s.properties.clone(),
                        paints: s.paints.clone(),
                        text: s.text.clone(),
                        effects: s.effects.clone(),
                        grids: s.grids.clone(),
                    });
                    rows.push(local_style_row(f, d, s, depth, ty, style_no, key_rows_len_before, &ui, window, cx));
                    y += LS_ROW_H;
                    style_no += 1;
                }
            }
        }
    }
    if groups > 0 {
        rows.push(div().w_full().h(pt(LS_BOTTOM_PAD)));
    }
    {
        let mut st = ui.borrow_mut();
        st.slots = slots;
        st.names = names;
        let n = key_rows.len();
        st.rows = key_rows;
        // 文档改了导致行数变少时把选中项夹回范围，避免按键读到越界行。
        st.sel = st.sel.filter(|&k| k < n);
    }

    // `track_focus` 把整张列表放进 Tab 顺序；方向键 / Enter / Delete / Ctrl+D 由根视图
    // 的键盘链转发到 `RightFields::local_style_key`（GPUI 元素级按键只在命中元素内触发）。
    let mut root = div().relative().w_full().flex().flex_col().track_focus(&f.local_style_focus);
    // 选中块必须先加：它是行的背景，后加会把整行内容盖住（用户 2026-11-15）。
    {
        let st = ui.borrow();
        let band = |y0: f32, h: f32, c| div().absolute().left(pt(LS_INSET)).right(pt(LS_INSET)).top(pt(y0)).h(pt(h)).rounded(pt(5.0)).bg(rgba(c));
        // ① 选中单一样式行：深块。
        if let Some(k) = st.sel
            && let Some(r) = st.rows.get(k)
        {
            root = root.child(band(r.y0, LS_ROW_H, t.selected_row));
        }
        // ② 选中文件夹：父行深块 + 其子树浅块，连成一段（完全照左栏 marks 的
        //    「Selected 父 + Child 后代」表现，用户 2026-11-15）。子树 = 紧跟其后、
        //    depth 更深的连续行；遇到同级/更浅的行停止。
        if let Some(ref key) = st.sel_folder {
            if let Some(i) = st.slots.iter().position(|s| matches!(&s.kind, LsSlotKind::Header { path } if format!("{}/{}", s.ty, path) == *key)) {
                let d = st.slots[i].depth;
                let r = &st.slots[i];
                root = root.child(band(r.y0, r.y1 - r.y0, t.selected_row));
                for s in st.slots.iter().skip(i + 1) {
                    if s.depth > d {
                        root = root.child(band(s.y0, s.y1 - s.y0, t.selected_row_child));
                    } else {
                        break;
                    }
                }
            }
        }
    }
    root = root.children(rows);
    // 行下面那段空白：点一下取消选中。它只占列表剩余高度，不覆盖任何行，
    // 所以点行不会误清选中（用户 2026-11-15）。
    {
        let ui_blank = ui.clone();
        let wake_blank = wake.clone();
        // CodeRabbit PR#24: a nonzero minimum so a short list still leaves a clickable blank
        // area; flex_1 alone collapses to zero inside a content-sized PanelSection body.
        root = root.child(div().flex_1().min_h(px(120.0)).on_mouse_down(MouseButton::Left, move |_e, _, _cx| {
            let mut st = ui_blank.borrow_mut();
            let had = st.sel.take().is_some() || st.sel_folder.take().is_some();
            drop(st);
            if had {
                wake_blank.wake();
            }
        }));
    }
    // 容器在窗口里的原点：prepaint 写入，下一帧的事件把指针换算成相对坐标。
    let ui_o = ui.clone();
    root = root.child(
        gpui::canvas(
            move |bounds, _, _| {
                ui_o.borrow_mut().origin = Some((f32::from(bounds.origin.x), f32::from(bounds.origin.y)));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h_full(),
    );

    // 拖动中每帧只解析落点画线，松手才提交一次 `style.move`（一个 undo step）。
    let ui_m = ui.clone();
    let wake_m = wake.clone();
    root = root.on_mouse_move(move |e: &MouseMoveEvent, _w, _cx| {
        let (slots, names, origin) = {
            let s = ui_m.borrow();
            (s.slots.clone(), s.names.clone(), s.origin)
        };
        let mut s = ui_m.borrow_mut();
        let Some(drag) = s.drag.as_mut() else { return };
        drag.pointer = e.position;
        let dx = f32::from(e.position.x) - f32::from(drag.start.x);
        let dy = f32::from(e.position.y) - f32::from(drag.start.y);
        if !drag.armed && dx.hypot(dy) > LS_DRAG_DEADZONE {
            drag.armed = true;
        }
        if drag.armed {
            let rel = f32::from(e.position.y) - origin.map(|o| o.1).unwrap_or(0.0);
            let made = ls_drop_move(&slots, &names, drag.ty, drag.from, rel);
            match made {
                Some((ly, q)) => {
                    drag.line_y = Some(ly);
                    drag.query = Some(q);
                }
                None => {
                    drag.line_y = None;
                    drag.query = None;
                }
            }
        }
        wake_m.wake();
    });
    let ui_u = ui.clone();
    let cmds_u = cmds.clone();
    let wake_u = wake.clone();
    root = root.on_mouse_up(MouseButton::Left, move |_e: &MouseUpEvent, _w, _cx: &mut App| {
        let Some(drag) = ui_u.borrow_mut().drag.take() else { return };
        // 松手只处理「拖动排序」：没越过死区的单击在**按下**时就已经选中了这一行
        // （`ui.sel`），双击改名也在按下时判定 —— 这里不再弹设置页
        // （用户 2026-11-15：只有前面 / 行尾那两个 24×24 图标才弹设置）。
        if drag.armed
            && let Some(q) = drag.query
        {
            cmds_u.push(Cmd { id: "style.move", params: Some(q) });
        }
        wake_u.wake();
    });

    // 插入线（2 pt 强调色，左右通到 band 边缘）+ 跟随指针的名字标签（egui `drag_ghost`）。
    {
        let st = ui.borrow();
        if let Some(drag) = st.drag.as_ref().filter(|d| d.armed) {
            if let Some(ly) = drag.line_y {
                root = root.child(div().absolute().left(pt(LS_INSET)).right(pt(LS_INSET)).top(pt(ly - 1.0)).h(pt(2.0)).bg(rgba(t.accent)));
            }
            let (ox, oy) = st.origin.unwrap_or((0.0, 0.0));
            let gx = f32::from(drag.pointer.x) - ox + 14.0;
            let gy = f32::from(drag.pointer.y) - oy + 10.0;
            root = root.child(
                div()
                    .absolute()
                    .left(pt(gx))
                    .top(pt(gy))
                    .px(pt(8.0))
                    .py(pt(4.0))
                    .bg(rgba(t.panel))
                    .border_1()
                    .border_color(rgba(t.accent))
                    .rounded(pt(5.0))
                    .child(div().child(drag.label.clone()).text_size(pt(11.0)).text_color(rgba(t.text))),
            );
        }
    }
    root
}

/// 样式名的「叶子」部分（去掉 "Brand/" 前缀），egui 的 `leaf(&s.name)`。
fn leaf_name(name: &str) -> SharedString {
    SharedString::from(name.rsplit('/').next().unwrap_or(name).to_string())
}

/// 文件夹路径：名字中最后一个 "/" 之前的部分（egui `style_picker::folder`）。
fn ls_folder(name: &str) -> String {
    name.rsplit_once('/').map_or_else(|| String::new(), |(p, _)| p.to_string())
}

/// `entries()` 产出的扁平树的一行（egui `style_picker::local_styles` 的 Folder / Style 行）。
enum LsLine {
    Folder {
        path: String,
        label: String,
        depth: usize,
    },
    /// `idx` 是样式在 `list` 中的下标；`depth` 是缩进层级。
    Style(usize, usize),
}

/// 文件夹树节点（egui `style_picker::entries` 的 `Branch`）。
struct LsBranch {
    path: String,
    label: String,
    styles: Vec<usize>,
    kids: Vec<usize>,
}

/// 按名字里的 "/" 把样式组织成文件夹树，扁平化成「先文件夹头、后其下样式、再递归子文件夹」的列表。
/// 与 egui `entries` / `branch_of` / `flatten` 一一对应；`folded` 返回 true 的文件夹整支收起
/// （本遍不做折叠态，始终展开）。
fn ls_entries(list: &[&LocalStyleRow], folded: &dyn Fn(&str) -> bool) -> Vec<LsLine> {
    // 按文件夹分组（无文件夹的放到 groups[0] 的 "" 组）。
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, s) in list.iter().enumerate() {
        let f = ls_folder(&s.name);
        match groups.iter_mut().find(|(g, _)| *g == f) {
            Some((_, idx)) => idx.push(i),
            None => groups.push((f, vec![i])),
        }
    }
    // 有文件夹的排后面（egui `sort_by_key(|(f, _)| !f.is_empty())`：空目录最前）。
    groups.sort_by_key(|(f, _)| !f.is_empty());
    let mut nodes: Vec<LsBranch> = vec![LsBranch { path: String::new(), label: String::new(), styles: Vec::new(), kids: Vec::new() }];
    for (path, idx) in groups {
        let ni = ls_branch_of(&mut nodes, &path);
        nodes[ni].styles = idx;
    }
    let mut out = Vec::new();
    for &i in &nodes[0].styles {
        out.push(LsLine::Style(i, 0));
    }
    let top = std::mem::take(&mut nodes[0].kids);
    for kid in top {
        ls_flatten(&nodes, kid, 0, folded, &mut out);
    }
    out
}

/// 取得（或递归建出）`path` 对应的分支（egui `branch_of`）。
fn ls_branch_of(nodes: &mut Vec<LsBranch>, path: &str) -> usize {
    if let Some(i) = nodes.iter().position(|n| n.path == path) {
        return i;
    }
    let (parent, label) = path.rsplit_once('/').map_or(("", path), |(p, l)| (p, l));
    let parent = ls_branch_of(nodes, parent);
    nodes.push(LsBranch { path: path.to_string(), label: label.to_string(), styles: Vec::new(), kids: Vec::new() });
    let i = nodes.len() - 1;
    nodes[parent].kids.push(i);
    i
}

/// 一个分支的头 + 其下全部内容（egui `flatten`）。
fn ls_flatten(nodes: &[LsBranch], node: usize, depth: usize, folded: &dyn Fn(&str) -> bool, out: &mut Vec<LsLine>) {
    let n = &nodes[node];
    out.push(LsLine::Folder { path: n.path.clone(), label: n.label.clone(), depth });
    if folded(&n.path) {
        return;
    }
    for &i in &n.styles {
        out.push(LsLine::Style(i, depth + 1));
    }
    for &kid in &n.kids {
        ls_flatten(nodes, kid, depth + 1, folded, out);
    }
}

/// 类型分组的标题行（egui `local_styles` 的 caption：32 pt、类型名 + 右侧 `+`）。
fn local_style_caption(title: SharedString, plus: IconButton) -> Div {
    let t = tokens();
    // egui `allocate_exact_size(vec2(w, LS_CAPTION_H))` + 文字在 `caption.min.x + LS_PAD`。
    div()
        .w_full()
        .h(pt(LS_CAPTION_H))
        .flex()
        .flex_row()
        .items_center()
        .pl(pt(LS_PAD))
        .pr(pt(LS_INSET))
        .child(div().text_size(pt(11.0)).font_weight(crate::panels::UI_FONT_REGULAR).text_color(rgba(t.text_secondary)).child(title))
        .child(div().flex_1().min_w(px(0.0)))
        .child(plus)
}

/// 新建一个 Local styles 改名输入框，并替换掉上一次的实体。
///
/// **每次进入改名都新建**（与左栏 Pages / Layers 同款）：`InlineInput` 复用会留下上次草稿，
/// 且在按下事件里 `begin_edit` 会让同一次手势的抬起吃掉 `select_all_pending`，表现为
/// 双击后没有全选、要再点才能全选（用户 2026-11-17：要点三次）。焦点与全选由渲染期
/// `poll_active` 完成。
fn ls_start_rename(
    slot: &Rc<RefCell<Option<gpui::Entity<crate::inline_input::InlineInput>>>>,
    renaming: &Rc<RefCell<Option<(String, String)>>>,
    renaming_style: &Rc<RefCell<Option<(String, String, String, Option<String>)>>>,
    cmds: &crate::panels::Cmds,
    value: String,
    cx: &mut App,
) {
    let renaming = renaming.clone();
    let renaming_style = renaming_style.clone();
    let cmds = cmds.clone();
    let input = crate::inline_input::InlineInput::build(cx, LS_ROW_H, 0.0, value, move |text: &str, _window, _cx| {
        let name = text.trim().to_string();
        // ① 样式行双击改名：提交 `style.move {styles:[id], name:"<folder>/<新名>"}`。
        //    名字是全名，文件夹前缀必须保留，否则会把样式挪出文件夹。
        if let Some((_ty, id, folder, before)) = renaming_style.borrow_mut().take() {
            if name.is_empty() {
                return;
            }
            let full = if folder.is_empty() { name } else { format!("{folder}/{name}") };
            // 不带 `before` 会把样式挪到列表末尾（引擎语义），这里带上原来的位置锚点。
            let mut params = serde_json::json!({ "styles": [id], "name": full });
            if let Some(b) = before {
                params["before"] = serde_json::Value::String(b);
            }
            cmds.push(Cmd { id: "style.move", params: Some(params) });
            return;
        }
        // ② 文件夹改名：状态是 (类型, 路径)：类型必须传给引擎，否则同名文件夹下
        //    其它类型的样式会被一起改名。
        let Some((ty, folder)) = renaming.borrow_mut().take() else { return };
        if name.is_empty() || name == folder {
            return;
        }
        cmds.push(Cmd { id: "style.renameFolder", params: Some(serde_json::json!({ "folder": folder, "name": name, "type": ty })) });
    });
    *slot.borrow_mut() = Some(input);
}

/// 文件夹头行（egui `local_styles` 的 Folder 行：chevron + 文件夹名，按层级缩进）。
/// 与 egui 一一对应地补齐三件事：① 左键整行切换折叠/展开（chevron 随之翻转）；
/// ② 行尾 `+` 在该文件夹内新建同类型样式（`style.create {type, name:"<folder>/<default>", value}`，
///    `value` 是该类型的起始值，见 `ls_create_params`）；
/// ③ 右键 "Rename folder" → 内联复用通用 `TextField`，提交即 `style.renameFolder`。
fn local_style_folder(
    f: &RightFields,
    d: &PanelData,
    label: &str,
    path: &str,
    ty: &'static str,
    depth: usize,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let t = tokens();
    // 是否正在重命名这个文件夹（egui `renaming.as_deref() == Some(path)`）。
    // 按 **type + path** 匹配：text 与 paint 可以各有 `Brand/Colors`，只比 path 会让两行同时
    // 进入编辑态、共用同一个输入框（CodeRabbit 指出）。
    let rename_key = (ty.to_string(), path.to_string());
    let mut is_renaming = f.local_style_renaming.borrow().as_ref().is_some_and(|k| *k == rename_key);
    // 编辑结束（Enter 提交、失焦提交、或 Esc 取消）时把状态收掉：`TextEdit::on_commit` 只在
    // **值变了**才回调，Esc 与「没改就离开」都不会回调，状态不清就会一直卡在改名态。
    // 焦点 / 全选 / 结束判定全部交给 `InlineInput::poll_active`（与左栏 Pages / Layers 同源），
    // 不再用「曾经激活过」的外部 seen 标记。
    if is_renaming {
        match f.local_style_rename_input.borrow().clone() {
            Some(input) => {
                if input.update(cx, |f, cx| f.poll_active(window, cx)) == crate::inline_input::EditState::Ended {
                    f.local_style_renaming.borrow_mut().take();
                    is_renaming = false;
                }
            }
            None => {
                f.local_style_renaming.borrow_mut().take();
                is_renaming = false;
            }
        }
    }

    // ② 行尾 `+`：在该文件夹内新建一个同类型样式（egui `ls_add_in`）。
    let cmds = f.cmds.clone();
    let create_params = ls_create_params(d, ty, path);
    let plus = IconButton::new(Icon::Plus).size(16.0).on_click(move |_e, _w, _cx| {
        cmds.push(Cmd { id: "style.create", params: Some(create_params.clone()) });
    });

    // ① 折叠态：egui 用 "<type>/<folder path>" 作 key。
    let folded = f.local_style_folded.clone();
    let key = format!("{}/{}", ty, path);
    let open = !folded.borrow().contains(&key);

    // ③ 右键 "Rename folder"：记下正在重命名的文件夹，并预填内联输入框的当前名。
    let root = f.root.clone();
    let renaming_r = f.local_style_renaming.clone();
    let rename_key_r = rename_key.clone();
    let path_r = path.to_string();
    let rename_slot = f.local_style_rename_input.clone();
    let rename_style_slot = f.local_style_rename_style.clone();
    let cmds_r = f.cmds.clone();
    let wake = f.cmds.clone();

    // 名称区：重命名时换成内联输入框；否则 chevron + 文件夹名，并挂折叠/右键菜单。
    // 图标起点 = band 左边 + ls_indent(depth)；每层缩进同一个固定值 LS_STEP（用户 2026-11-15），
    // 子项图标正好对齐父项文字左边。行尾给 16 pt `+` 留 LS_ICON + LS_ICON_GAP，
    // 名字不会被压到按钮下面（egui `text_r`）。
    let name_inset = ls_indent(depth);
    // 点名字区域选中本文件夹（父深块 + 子浅块，照左栏图层）；和样式行一样把焦点交给
    // 列表，便于选中后直接用键盘。chevron 那一小块单独负责折叠（用户 2026-11-15）。
    let ui = f.local_styles_ui.clone();
    let focus = f.local_style_focus.clone();
    let name_area = if is_renaming {
        div()
            .w_full()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(pt(LS_ICON_GAP))
            .pl(pt(name_inset))
            .pr(pt(LS_ICON + LS_ICON_GAP))
            .children(f.local_style_rename_input.borrow().clone())
    } else {
        let chevron = if open { Icon::ChevronDown } else { Icon::ChevronRight };
        div()
            .w_full()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(pt(LS_ICON_GAP))
            .pl(pt(name_inset))
            .pr(pt(LS_ICON + LS_ICON_GAP))
            .cursor_pointer()
            // ① 左键点「名字区域」= 选中文件夹（父深块 + 子浅块，照左栏图层）；
            //    折叠只由 chevron 那一小块触发（用户 2026-11-15）。
            .on_mouse_down(MouseButton::Left, {
                let ui = ui.clone();
                let focus = focus.clone();
                let key = key.clone();
                let wake = wake.clone();
                move |_e, window, cx: &mut App| {
                    window.focus(&focus);
                    let mut st = ui.borrow_mut();
                    // 已选中就不重复写，避免唤醒引起的视觉抖动（同左栏图层）。
                    if st.sel_folder.as_deref() != Some(key.as_str()) || st.sel.is_some() {
                        st.sel = None;
                        st.sel_folder = Some(key.clone());
                        drop(st);
                        wake.wake();
                    }
                    cx.stop_propagation();
                }
            })
            // ③ 右键菜单 "Rename folder"（egui `context_popup`）。
            .on_mouse_down(MouseButton::Right, {
                let root = root.clone();
                let renaming_r = renaming_r.clone();
                let rename_key_r = rename_key_r.clone();
                let path_r = path_r.clone();
                let rename_slot = rename_slot.clone();
                let rename_style_slot = rename_style_slot.clone();
                let cmds_r = cmds_r.clone();
                let wake = wake.clone();
                move |e: &gpui::MouseDownEvent, _w, cx: &mut App| {
                    let root = root.clone();
                    let renaming_r = renaming_r.clone();
                    let rename_key_r = rename_key_r.clone();
                    let path_r = path_r.clone();
                    let rename_slot = rename_slot.clone();
                    let rename_style_slot = rename_style_slot.clone();
                    let cmds_r = cmds_r.clone();
                    let wake = wake.clone();
                    let rows = vec![MenuRow::new("Rename folder")];
                    let Some(root) = root else { return };
                    let anchor = gpui::Bounds::new(e.position, gpui::size(px(0.0), px(0.0)));
                    root.update(cx, move |cv, cx| {
                        cv.open_menu(
                            anchor,
                            Some(160.0),
                            rows,
                            Rc::new(move |label: &str, window, cx| {
                                if label == "Rename folder" {
                                    renaming_r.borrow_mut().replace(rename_key_r.clone());
                                    // 每次改名新建输入框；焦点与全选由渲染期 `poll_active` 完成
                                    // （在按下事件里 begin_edit 会被同一次手势的抬起吃掉全选）。
                                    ls_start_rename(&rename_slot, &renaming_r, &rename_style_slot, &cmds_r, path_r.clone(), cx);
                                    // 改名态是渲染期读的：不唤醒就不会出现内联输入框。
                                    wake.wake();
                                }
                            }),
                            cx,
                        );
                    });
                }
            })
            // ① 折叠只绑定在 chevron 这一小块上（用户 2026-11-15）。命中它时 stop，
            //    不冒泡到父级的「选中文件夹」；命中名字区域则相反——只选中、不折叠。
            .child(
                div()
                    .flex_none()
                    .size(pt(LS_ICON))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, {
                        let folded = folded.clone();
                        let key = key.clone();
                        let wake = wake.clone();
                        move |_e, _w, cx: &mut App| {
                            {
                                let mut s = folded.borrow_mut();
                                if s.contains(&key) {
                                    s.remove(&key);
                                } else {
                                    s.insert(key.clone());
                                }
                            }
                            cx.stop_propagation();
                            // 折叠态是渲染期读的状态，改完必须唤醒一次，否则 chevron 要等别的事件才更新。
                            wake.wake();
                        }
                    })
                    .child(crate::widgets::icon::paint(chevron, rgba(t.text_secondary), LS_CHEVRON)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .text_size(pt(11.0))
                    .font_weight(crate::panels::UI_FONT_REGULAR)
                    .text_color(rgba(t.text))
                    .child(SharedString::from(label.to_string())),
            )
    };

    // 参考实现 `local_styles`：文件夹行 `allocate_exact_size(vec2(w, 24.0))`，
    // 左右各内缩 8 pt，hover band 圆角 5。
    div().relative().w_full().h(pt(LS_FOLDER_H)).px(pt(LS_INSET)).child(
        div()
            .w_full()
            .h_full()
            .rounded(pt(5.0))
            .hover(|s| s.bg(rgba(t.hover)))
            .child(name_area)
            .child(div().absolute().right(pt(LS_INSET)).top_0().h_full().flex().items_center().child(plus)),
    )
}

/// 文件夹 `+` 新建样式所用的默认名（egui `StyleSlot::default_name`）。
fn ls_default_name(ty: &str) -> &'static str {
    match ty {
        "text" => "Text",
        "paint" => "Color",
        "effect" => "Effect",
        "grid" => "Grid",
        _ => "Style",
    }
}

/// `style.create` 的参数，1:1 照 egui `style_picker::new_style_params`：名字取
/// `<folder>/<默认名> <n>`（`n` 让同名不重复），**没有选中图层时带上该类型的起始值**，
/// 这样 Local styles 页的 `+`（该页本来就没有选区）不会静默失败。
/// 引擎 `style.create` 只认 `from` 或 `value` 二者之一，缺了会报
/// "pass `value` or select a layer to take it from"。
fn ls_create_params(d: &PanelData, ty: &str, folder: &str) -> Value {
    let base = if folder.is_empty() { ls_default_name(ty).to_string() } else { format!("{folder}/{}", ls_default_name(ty)) };
    let name = (1..).map(|n| format!("{base} {n}")).find(|n| d.styles.iter().all(|s| &s.name != n)).unwrap_or(base);
    let value = match ty {
        "text" => json!({ "fontSize": 16.0 }),
        "effect" | "grid" => json!([]),
        // paint（以及兼容的 stroke）从纯黑开始。
        _ => json!("#000000"),
    };
    json!({ "type": ty, "name": name, "value": value })
}

/// 一条样式行快照 → `style.create` 的 `value`（各类型的**整值**，与 `style.update` 同构）。
///
/// `Ctrl/Cmd+C` 复制、`Ctrl/Cmd+V` 粘贴要带着值走：新样式必须有内容，不能只留一个空壳名字。
/// 各类型的键与引擎 `style.create` 的解析一一对应：paint = paints 数组、text = TextStyle 对象、
/// effect = effects 数组、grid = grids 数组。
pub(crate) fn ls_style_value(r: &LocalStyleKeyRow) -> Value {
    match r.ty {
        "paint" => serde_json::to_value(&r.paints).unwrap_or_else(|_| json!([])),
        "text" => r.text.clone().unwrap_or_else(|| json!({})),
        "effect" => Value::Array(r.effects.as_ref().map(|e| e.iter().map(|x| x.effect.clone()).collect()).unwrap_or_default()),
        // `GridRow.grids` 存的就是**整个** grids 数组，取第一行即可，不要重复拼。
        _ => r.grids.as_ref().and_then(|g| g.first()).map(|x| x.grids.clone()).unwrap_or_else(|| json!([])),
    }
}

/// 粘贴出来的样式名：`<leaf> copy`，重名则 ` copy 2 / copy 3 …`（同 `style.duplicate` 的语义），
/// 并保留原来的文件夹前缀。
pub(crate) fn ls_paste_name(names: &[(String, String)], src: &str) -> String {
    let folder = ls_folder(src);
    let leaf = src.rsplit('/').next().unwrap_or(src);
    let named = |l: &str| if folder.is_empty() { l.to_string() } else { format!("{folder}/{l}") };
    let base = format!("{leaf} copy");
    let first = named(&base);
    let taken = |n: &str| names.iter().any(|(_, x)| x == n);
    if !taken(&first) {
        return first;
    }
    (2..).map(|k| named(&format!("{base} {k}"))).find(|n| !taken(n)).unwrap_or(first)
}

/// 类型分组标题栏的 `+`：弹出「建哪种样式」菜单（egui `local_styles` 的头部 `+`）。
fn local_styles_add_button(f: &RightFields, d: &PanelData) -> IconButton {
    // (菜单标题, style.create 的 type, 默认名) —— 与 egui `StyleSlot::{title,create_type,default_name}` 对齐。
    // 必须是 `static`：菜单回调会逃出本函数，借本地数组会活不过函数体。
    static ITEMS: &[(&str, &str, &str)] = &[
        ("Text styles", "text", "Text"),
        ("Color styles", "paint", "Color"),
        ("Effect styles", "effect", "Effect"),
        ("Grid styles", "grid", "Grid"),
    ];
    // 每个类型的 `style.create` 参数先算好（含默认 `value`，见 `ls_create_params`）：
    // 菜单回调是 `Fn`，不能在里面重新借 `d`。
    let create_params: Vec<Value> = ITEMS.iter().map(|(_, ctype, _)| ls_create_params(d, ctype, "")).collect();
    let root = f.root.clone();
    let cmds = f.cmds.clone();
    IconButton::new(Icon::Plus).size(24.0).on_click(move |_e, window, cx: &mut App| {
        let rows: Vec<MenuRow> = ITEMS.iter().map(|r| MenuRow::new(r.0)).collect();
        let Some(root) = root.clone() else { return };
        let anchor = Bounds { origin: window.mouse_position(), size: size(px(0.0), px(0.0)) };
        // `on_click` 是 `Fn`，捕获的 `cmds` 不能直接 move 进内层回调，先克隆一份。
        let cmds = cmds.clone();
        let create_params = create_params.clone();
        root.update(cx, move |cv, cx| {
            cv.open_menu(
                anchor,
                Some(200.0),
                rows,
                Rc::new(move |label: &str, _w, _cx| {
                    if let Some(i) = ITEMS.iter().position(|r| r.0 == label) {
                        cmds.push(Cmd { id: "style.create", params: Some(create_params[i].clone()) });
                    }
                }),
                cx,
            );
        });
    })
}

/// Page 节「Local styles」里的一行（egui `style_picker::local_styles` 的 Style 行，1:1）：
/// 左侧徽标 16×16 —— paint 用圆形色板、text 用 "Ag" + 右侧 "32/Auto" 预览、effect/grid 用类型图标；
/// 整行悬停高亮（圆角 t.hover 块，与行同高、内缩 8px）、左键把样式应用到选区、右键 Duplicate / Delete；
/// 文件夹层级用 `depth * 24px` 缩进（egui `LS_STEP`）。
///
/// `d` 只用来算「改名时的位置锚点」（`style.move` 的 `before`），不再参与渲染。
fn local_style_row(
    f: &RightFields,
    d: &PanelData,
    s: &LocalStyleRow,
    depth: usize,
    ty: &'static str,
    style_no: usize,
    // 这一行在 `LocalStyleUi.rows`（跨类型、按绘制顺序）里的下标：单击选中 / 双击改名都用它。
    flat: usize,
    ui: &Rc<RefCell<LocalStyleUi>>,
    window: &mut Window,
    cx: &mut App,
) -> Div {
    let t = tokens();
    let cmds = f.cmds.clone();
    let sid = s.id.clone();
    let name = leaf_name(&s.name);
    // 改名提交 `style.move {name}`：不带 `before` 时引擎会把样式挪到列表**末尾**，列表顺序会跳。
    // 取「同类型里紧跟其后的那个样式」当锚点，改完名字位置不变。
    let rename_before: Option<String> = {
        let same: Vec<&LocalStyleRow> = d.styles.iter().filter(|x| x.ty == ty).collect();
        same.iter().position(|x| x.id == s.id).and_then(|i| same.get(i + 1)).map(|x| x.id.clone())
    };
    // 双击改名的内联输入框状态（与文件夹改名共用同一个控件实体）。
    let mut is_renaming = f.local_style_rename_style.borrow().as_ref().is_some_and(|k| k.0 == ty && k.1 == s.id);
    // 编辑结束（Enter 提交 / Esc / 失焦）时收掉改名态，判定与文件夹改名同源（`poll_active`）。
    if is_renaming {
        match f.local_style_rename_input.borrow().clone() {
            Some(input) => {
                if input.update(cx, |f, cx| f.poll_active(window, cx)) == crate::inline_input::EditState::Ended {
                    f.local_style_rename_style.borrow_mut().take();
                    is_renaming = false;
                }
            }
            None => {
                f.local_style_rename_style.borrow_mut().take();
                is_renaming = false;
            }
        }
    }
    // 打开这个样式的设置页：前面 24×24 图标、行尾 24×24 设置按钮、右键「Edit style」三处共用。
    let edit_title = SharedString::from(match ty {
        "paint" => tr("Edit color style"),
        "text" => tr("Edit text style"),
        "effect" => tr("Edit effect style"),
        _ => tr("Edit grid style"),
    });
    // 跨帧稳定探针：触发按钮在 prepaint 把自身 top 写进这个 cell；右键菜单那条路径没有按钮，
    // 留空走旧近似（anchored_top 的兜底）。
    let editor_trigger = format!("style-editor-{}", s.id);
    let editor_hit: Rc<Cell<Option<Bounds<Pixels>>>> = crate::popovers::window_trigger_top(&editor_trigger);
    let open_editor: Rc<dyn Fn(&mut Window, &mut App)> = {
        let popups = f.popups();
        let cmds = cmds.clone();
        let title = edit_title.clone();
        let editor_key = editor_trigger.clone();
        let (id, full, desc) = (s.id.clone(), s.name.clone(), s.description.clone());
        let props = s.properties.clone();
        let ps = s.paints.clone();
        let stext = s.text.clone();
        let seff = s.effects.clone();
        let sgrid = s.grids.clone();
        Rc::new(move |window: &mut Window, cx: &mut App| {
            // 统一入口：跨帧探针精确顶部 + 触发标识 + 再点关闭。
            let top = crate::popovers::anchored_top(&editor_key, window);
            open_detached_with(popups.clone(), editor_trigger.clone(), Some(top), cx, |anchor, cx| {
                crate::popovers::WindowState::style_editor(
                    cmds.clone(),
                    cx,
                    anchor,
                    title.clone(),
                    ty,
                    id.clone(),
                    full.clone(),
                    desc.clone(),
                    props.clone(),
                    ps.clone(),
                    stext.clone(),
                    seff.clone(),
                    sgrid.clone(),
                    popups.clone(),
                )
            });
        })
    };
    // 左侧徽标（egui `paint_dot` / "Ag" / `slot.icon()`）。
    let badge: gpui::AnyElement = match &s.paint {
        // 参考编辑器 `swatch--swatch`：通用色块的**圆形**类型，45° 斜线分半
        // （左下不透明、右上随 alpha），边框由控件自己画，不再手搓 div 裁圆。
        Some(p) => swatch(p.clone(), LS_ICON).shape(crate::widgets::swatch::SwatchShape::Circle).into_any_element(),
        None if ty == "text" => div()
            .flex_none()
            .size(pt(LS_ICON))
            .flex()
            .items_center()
            .justify_center()
            .text_size(pt(9.0))
            .text_color(rgba(t.text))
            .child("Aa")
            .into_any_element(),
        None => {
            let icon = match ty {
                "effect" => Icon::Rectangle,
                "grid" => Icon::ViewGrid,
                _ => Icon::Text,
            };
            div()
                .flex_none()
                .size(pt(LS_ICON))
                .flex()
                .items_center()
                .justify_center()
                .child(crate::widgets::icon::paint(icon, rgba(t.text_secondary), 14.0))
                .into_any_element()
        }
    };
    // 前面的 **24×24 图标命中区**：点它才弹出这个样式的设置页（用户 2026-11-15）。
    // 图标本身还是 16 pt，命中区放大到 24×24；左右各 -4 的外边距让图标位置与名字间距
    // 和改动前逐像素一致，不因为命中区变大把名字推右。
    let badge_hit = div()
        .flex_none()
        .ml(px(-4.0))
        .mr(px(-4.0))
        .size(pt(LS_ICON_HIT))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, {
            let open = open_editor.clone();
            move |_e, window, cx: &mut App| {
                // 不要再冒泡到整行的「选中 / 拖动」。
                cx.stop_propagation();
                open(window, cx);
            }
        })
        .child(badge);
    // 整行内容：层级缩进 + 8px 内缩，悬停高亮。
    let mut body = div()
        .w_full()
        .h_full()
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(pt(LS_ICON_GAP))
        .child(crate::popovers::bounds_probe(editor_hit.clone()))
        // 每层缩进同一个固定值 LS_STEP（用户 2026-11-15）：图标起点 = 行基准 + depth * LS_STEP，
        // 正好对齐上一层文件夹的文字左边。badge_hit 的 ml(-4) 与内部居中 +4 抵消，圆形视觉左边 = pl。
        .pl(pt(ls_indent(depth)))
        .pr(pt(4.0))
        .cursor_pointer()
        // 左键：① 选中这一行（单击）；② 记拖动起点，越过死区才改成排序拖动；
        // ③ 400ms 内在同一行再按 = 双击改名。**不再**弹设置页——那是前面 / 行尾两个图标的事。
        .on_mouse_down(MouseButton::Left, {
            let ui = ui.clone();
            let wake = f.cmds.clone();
            let id = sid.clone();
            let label = leaf_name(&s.name).to_string();
            let full_name = s.name.clone();
            let description = s.description.clone();
            let properties = s.properties.clone();
            let paints = s.paints.clone();
            let text = s.text.clone();
            let effects = s.effects.clone();
            let grids = s.grids.clone();
            let renaming = f.local_style_rename_style.clone();
            let folder_renaming = f.local_style_renaming.clone();
            let rename_slot = f.local_style_rename_input.clone();
            let cmds_r = cmds.clone();
            let focus = f.local_style_focus.clone();
            let folder = ls_folder(&s.name);
            move |e: &MouseDownEvent, window, cx: &mut App| {
                // 键盘操作（↑↓ / Enter / Delete / Ctrl+D / Ctrl+C / Ctrl+V）都挂在列表的焦点上：
                // 点行即把焦点交给列表，选完立刻就能用键盘，不用先按一次 Tab。
                window.focus(&focus);
                {
                    let mut st = ui.borrow_mut();
                    st.sel = Some(flat);
                    st.sel_folder = None;
                }
                // 双击（原生 `click_count`，与左栏 Layers / 变量表一致）进入改名：
                // 这一下拦住冒泡，否则根视图「点别处就提交」会把刚打开的输入框收掉；
                // 也不再在按下事件里 `begin_edit` —— 焦点与全选交给渲染期 `poll_active`，
                // 否则同一次手势的抬起会把全选吃掉，表现为要点三次才全选。
                if e.click_count >= 2 {
                    cx.stop_propagation();
                    renaming.borrow_mut().replace((ty.to_string(), id.clone(), folder.clone(), rename_before.clone()));
                    ls_start_rename(&rename_slot, &folder_renaming, &renaming, &cmds_r, label.clone(), cx);
                    wake.wake();
                    return;
                }
                // 单击：记拖动起点，越过死区才改成排序拖动。
                ui.borrow_mut().drag = Some(LocalStyleDrag {
                    ty,
                    from: style_no,
                    id: id.clone(),
                    label: label.clone(),
                    name: full_name.clone(),
                    description: description.clone(),
                    properties: properties.clone(),
                    paints: paints.clone(),
                    text: text.clone(),
                    effects: effects.clone(),
                    grids: grids.clone(),
                    start: e.position,
                    pointer: e.position,
                    armed: false,
                    line_y: None,
                    query: None,
                });
                wake.wake();
            }
        })
        .child(badge_hit);
    // 名字占满中间，文本样式的 "32/Auto" 贴行尾（egui 的 meta 右对齐在 `r.max.x - LS_TRAIL`）。
    // 双击改名期间名字位换成内联输入框（与文件夹改名共用同一个控件实体）。
    body = if is_renaming {
        body.child(div().flex_1().min_w(px(0.0)).h_full().flex().items_center().children(f.local_style_rename_input.borrow().clone()))
    } else {
        // 名字用 `static_text`（canvas + 与编辑态同一个居中式）绘制，不再是
        // `div().child(name)`：两条渲染管线的行盒取整不同，切换编辑时会差 1 像素。
        let b = body.child(div().flex_1().min_w(px(0.0)).h_full().flex().items_center().overflow_hidden().child(crate::inline_input::static_text(
            gpui::SharedString::from(name.clone()),
            rgba(t.text).into(),
            LS_ROW_H,
        )));
        match &s.text_meta {
            Some(meta) => b.child(
                div().flex_none().pl(pt(5.0)).text_size(pt(10.0)).text_color(rgba(t.text_secondary)).child(SharedString::from(format!("· {meta}"))),
            ),
            None => b,
        }
    };
    // 右键：Edit style / Duplicate / Delete（egui `context_popup` 1:1）。
    // 「Edit style」与前面 / 行尾两个图标同一个入口，都打开样式编辑器弹窗。
    let cmds_r = cmds.clone();
    let root = f.root.clone();
    let edit_label: String = tr("Edit style").to_string();
    let dup_label: String = tr("Duplicate").to_string();
    let del_label: String = tr("Delete").to_string();
    let open_editor_r = open_editor.clone();
    body = body.on_mouse_down(MouseButton::Right, move |e: &gpui::MouseDownEvent, _w, cx: &mut App| {
        // `on_mouse_down` 是 `Fn`（可多次触发），捕获值不能移出去，先逐项 clone。
        let cmds_r = cmds_r.clone();
        let sid = sid.clone();
        let open_editor = open_editor_r.clone();
        let edit = edit_label.clone();
        let dup = dup_label.clone();
        let del = del_label.clone();
        let dup_trail = if cfg!(target_os = "windows") { "Ctrl+D" } else { "⌘D" };
        let rows = vec![MenuRow::new(edit.clone()), MenuRow::new(dup.clone()).trail(dup_trail), MenuRow::new(del.clone()).trail("Del")];
        let Some(root) = root.clone() else { return };
        let anchor = gpui::Bounds::new(e.position, gpui::size(px(0.0), px(0.0)));
        root.update(cx, move |cv, cx| {
            cv.open_menu(
                anchor,
                Some(160.0),
                rows,
                Rc::new(move |label: &str, window, cx| {
                    if edit == label {
                        open_editor(window, cx);
                        return;
                    }
                    let id = if dup == label {
                        "style.duplicate"
                    } else if del == label {
                        "style.delete"
                    } else {
                        return;
                    };
                    cmds_r.push(Cmd { id, params: Some(json!({ "style": sid })) });
                }),
                cx,
            );
        });
    });
    // 整行左右各内缩 LS_INSET、24 pt 高，hover band 圆角 5（egui `inset_row` + `rect_filled`）。
    let selected = ui.borrow().sel == Some(flat);
    let row_group = SharedString::from(format!("ls-style-row-{}-{}", ty, s.id));
    div()
        .group(row_group.clone())
        .w_full()
        .h(pt(LS_ROW_H))
        .px(pt(LS_INSET))
        // 整行一块背景（含行尾按钮），hover / 选中都是这一块变色。
        .child(
            div()
                .w_full()
                .h_full()
                .rounded(pt(5.0))
                .flex()
                .flex_row()
                .items_center()
                // 样式行不要悬停背景色块（用户 2026-11-15）；设置按钮仍靠 group_hover 显隐。
                .child(div().flex_1().min_w(px(0.0)).h_full().child(body))
                // 设置按钮在块内部右端；只在悬停或选中这一行时显示。
                .child({
                    let open = open_editor.clone();
                    let group = row_group.clone();
                    div()
                        .flex_none()
                        .h_full()
                        .flex()
                        .items_center()
                        .when(!selected, move |d| d.opacity(0.0).group_hover(group, |s| s.opacity(1.0)))
                        .child(IconButton::new(Icon::StyleSettings).size(24.0).on_click(move |_e, window, cx| {
                            cx.stop_propagation();
                            open(window, cx);
                        }))
                }),
        )
}

/// 空选区时的右栏：**两个独立小节**，各自一个 40 pt 标题：
/// ① Page（背景色 / Color profile）② Local styles。
///
/// 原来还有一节「Local variables」＋「Open variables (N)」下拉（点开变量编辑器）；
/// 用户 2026-11-13 明确不需要这一节，已整段删除。变量编辑器仍可从 Appearance /
/// Typography 头部的变量按钮打开，功能没丢。
pub(crate) fn page(f: &RightFields, d: &PanelData, row_w: f32, n: &mut usize, window: &mut Window, cx: &mut App) -> Vec<PanelSection> {
    let Some(p) = d.page.as_ref() else { return Vec::new() };
    let mut sec = PanelSection::new(tr("Page"));
    let hex_slot = slot(n);
    let op_slot = slot(n);
    let hex_ed = f.row_field(
        cx,
        hex_slot,
        &p.background,
        RowEditCtx {
            cmd: "page.setBackground",
            params: json!({}),
            key: "color",
            scale: 1.0,
            string_value: true,
            paint: None,
            nest: None,
            nest_key: None,
            paint_color: None,
        },
    );
    let op_ed = f.row_field(
        cx,
        op_slot,
        "100",
        RowEditCtx {
            cmd: "page.setBackground",
            params: json!({}),
            key: "opacity",
            scale: 100.0,
            // 只读标签（「100 %」），不是可编辑数值：`string_value: true` 让 `row_field`
            // 认领时清掉数值行为，尾部的「%」便不会被拖拽 scrub 改成一个不生效的数字。
            string_value: true,
            paint: None,
            nest: None,
            nest_key: None,
            paint_color: None,
        },
    );
    // egui `right.rs::page_section`: the trailing "100 %" is a **label**, not an editable field.
    // Keep the slot's look but make it read-only, so clicking it cannot raise the bogus
    // `page.setBackground {"opacity": …}` (the engine only reads `color`).
    op_ed.update(cx, |te, _| te.set_readonly(true));
    let color = figcraft_color::Color::from_hex(&p.background).unwrap_or(figcraft_color::Color::WHITE);
    let cf = ColorField::new(cx, figcraft_color::Paint::solid(color), ColorText::Hex(SharedString::from(p.background.clone())), row_w, hex_ed, op_ed);
    // swatch → picker (`toggle_picker(PickerTarget::PageBackground)`).
    let (cf, swatch_hit) = color_pick(
        f,
        cf,
        Color32::from_hex(&p.background).unwrap_or(Color32::WHITE),
        "page.setBackground",
        json!({}),
        "color",
        None,
        "color-page".to_string(),
        Vec::new(),
    );
    sec = sec.child(field_row_full(crate::popovers::with_button_bounds(cf, swatch_hit)));
    // egui keeps this as local UI state and opens a dialog; GPUI has no dialog yet, so the
    // engine's `file.colorProfile` command is the equivalent. `node.set {"colorProfile": …}`
    // was rejected: `colorProfile` is not a node property.
    let profiles: Vec<(String, &'static str, Value)> =
        figcraft_color::ColorProfile::ALL.iter().map(|cp| (cp.label().to_string(), "file.colorProfile", json!({ "profile": cp.id() }))).collect();
    sec = sec.child(field_row_full(pick_dropdown(f, p.profile.clone(), row_w, profiles)));
    let mut out = vec![sec];

    // ── ② Local styles（egui `style_picker::local_styles`）──────────────────────────────────
    // 整张列表作为一个 free block：24 pt 行、8/16/16 分组间距、圆形色板、整行拖动排序
    // 都由 `local_styles_block` 按 `LS_*` 自己排版，不套统一网格的 32 pt 行 / 24 pt trailing 列。
    // 分组顺序与 egui `StyleSlot` 一致：Text / Fill(Color) / Effect / Grid。
    let styles_sec = PanelSection::new(tr("Styles"))
        .variant(if p.local_styles == 0 { SectionHeaderVariant::Dim } else { SectionHeaderVariant::Normal })
        .header_trail(icon_row(vec![local_styles_add_button(f, d)]))
        .child(block_row(local_styles_block(f, d, window, cx)));
    out.push(styles_sec);

    // ── ③ Export（egui `export_panel::page_section`）──────────────────────────────────────
    // 本地样式之后就是「把整页导出为一个 PNG」；按钮与 Design 区的底部导出共用 `wide_button`。
    // ids 传页面子层：`file.export` 收到显式 ids 就按这些层合成一张图，缺省（空页面）引擎同样
    // 会退回到页面子层，所以这里不需要特判。
    out.push(
        PanelSection::new(tr("Export"))
            .child(field_row_full(wide_button(f, tr("Export page as PNG").into(), "file.export", json!({ "ids": p.export_ids, "format": "png" })))),
    );
    out
}

#[cfg(test)]
mod local_styles_1to1_tests {
    use super::*;

    /// 上游 `ui-egui/src/widgets/style_picker.rs` 45-67 行逐项微调过的布局参数。
    /// 这一版是「唯一正确的版本」，任何一项都不许再凭感觉改回去。
    #[test]
    fn ls_layout_constants_match_the_egui_reference() {
        assert_eq!(LS_INSET, 8.0);
        assert_eq!(LS_PAD, 16.0);
        assert_eq!(LS_CAPTION_H, 32.0);
        assert_eq!(LS_ROW_H, 24.0);
        assert_eq!(LS_ICON, 16.0);
        assert_eq!(LS_ICON_GAP, 8.0);
        assert_eq!(LS_STEP, 24.0);
        assert_eq!(LS_CHEVRON, 12.0);
        assert_eq!(LS_FIRST_GAP, 8.0);
        assert_eq!(LS_GROUP_GAP, 16.0);
        assert_eq!(LS_BOTTOM_PAD, 16.0);
        assert_eq!(LS_TRAIL, 8.0);
    }

    fn style(index: usize, id: &str, name: &str, y: f32) -> LsSlot {
        LsSlot { ty: "paint", y0: y, y1: y + LS_ROW_H, depth: 0, kind: LsSlotKind::Style { index, id: id.into(), name: name.into() } }
    }

    /// egui `drop_move`：给出 `before`、跨文件夹时改名、落在原位不动、重名时编号。
    #[test]
    fn ls_drop_move_resolves_before_folder_and_noop() {
        let slots = vec![style(0, "a", "Red", 32.0), style(1, "b", "Brand/Blue", 56.0), style(2, "c", "Brand/Green", 80.0)];
        let names: Vec<(String, String)> = slots
            .iter()
            .map(|s| match &s.kind {
                LsSlotKind::Style { id, name, .. } => (id.clone(), name.clone()),
                _ => unreachable!(),
            })
            .collect();
        // 停在 b 的上沿之前：插入线画在 b 的上沿，before=b；落点行的文件夹是 Brand，
        // 于是根目录的 "Red" 要改名成 "Brand/Red"。
        let (y, q) = ls_drop_move(&slots, &names, "paint", 0, 55.0).unwrap();
        assert_eq!(y, 56.0);
        assert_eq!(q["styles"][0], "a");
        assert_eq!(q["before"], "b");
        assert_eq!(q["name"], "Brand/Red");
        // 落在自己原来的缝里（第一个槽之上）是空操作，不画线也不发命令。
        assert!(ls_drop_move(&slots, &names, "paint", 0, 30.0).is_none());
        // 目标文件夹里已有同名样式时编号，不会留下两个 "Brand/Red"。
        let mut taken = names.clone();
        taken.push(("x".into(), "Brand/Red".into()));
        let (_, q) = ls_drop_move(&slots, &taken, "paint", 0, 55.0).unwrap();
        assert_eq!(q["name"], "Brand/Red 2");
    }
}

#[cfg(test)]
mod view_menu_tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Local styles 的 `+` 必须自带 `value`：Local styles 页没有选区，引擎 `style.create` 只认
    /// `from` 或 `value`，缺了会报 "pass `value` or select a layer to take it from"，`+` 看着
    /// 就没反应（CodeRabbit 指出）。这里锁住每种类型的起始值与不重名的命名。
    #[test]
    fn local_style_create_params_carry_a_starting_value_and_a_unique_name() {
        let mut d = PanelData::default();
        d.styles.push(LocalStyleRow {
            id: "s1".into(),
            name: "Color 1".into(),
            ty: "paint",
            paint: None,
            text_meta: None,
            description: String::new(),
            properties: Vec::new(),
            paints: Vec::new(),
            text: None,
            effects: None,
            grids: None,
        });
        // paint：纯黑；名字避开已有的 "Color 1"。
        let p = ls_create_params(&d, "paint", "");
        assert_eq!(p["type"], "paint");
        assert_eq!(p["value"], "#000000");
        assert_eq!(p["name"], "Color 2");
        // 文件夹前缀。
        assert_eq!(ls_create_params(&d, "paint", "Brand/Colors")["name"], "Brand/Colors/Color 1");
        // text / effect / grid 的起始值。
        assert_eq!(ls_create_params(&d, "text", "")["value"], json!({ "fontSize": 16.0 }));
        assert_eq!(ls_create_params(&d, "effect", "")["value"], json!([]));
        assert_eq!(ls_create_params(&d, "grid", "")["value"], json!([]));
    }

    /// `menu_check` 给勾选行加 `✓ ` / 四个空格前缀，`menu_popup` 在 on_pick 时 trim 掉。
    /// 所以行 label 本身不能带前后空格、也不能重复，否则会选中错的那条命令。
    #[test]
    fn view_rows_are_unique_trimmed_and_registered() {
        let rows = view_rows(&ViewData::default());
        let mut seen = BTreeSet::new();
        for r in &rows {
            assert_eq!(r.label.trim(), r.label, "menu label `{}` would break on_pick trim", r.label);
            assert!(seen.insert(r.label.clone()), "duplicate menu label `{}`", r.label);
            assert!(figcraft_engine::cmd::find_command(r.cmd).is_some(), "view menu raises unknown command `{}`", r.cmd);
        }
        assert_eq!(rows.len(), 14, "7 zoom + 2 pixel preview + 5 show/hide toggles");
        assert_eq!(rows.iter().filter(|r| r.checked.is_some()).count(), 7, "only show/hide rows get a check column");
        assert_eq!(rows.iter().filter(|r| r.separator).count(), 2, "egui separates the zoom / preview / toggle groups");
    }

    /// 1x / 2x 不能同时打勾，且 `on` 必须是当前值的反相（引擎 `pixel_preview` 读 `on`）。
    #[test]
    fn pixel_preview_rows_toggle_the_current_scale() {
        let v = ViewData { pixel_preview: true, pixel_scale: 2, ..Default::default() };
        let rows = view_rows(&v);
        let previews: Vec<&ViewMenuRow> = rows.iter().filter(|r| r.cmd == "view.pixelPreview").collect();
        assert_eq!(previews.len(), 2);
        assert_eq!(previews[0].checked, Some(false));
        assert_eq!(previews[0].params["on"], serde_json::json!(true));
        assert_eq!(previews[1].checked, Some(true));
        assert_eq!(previews[1].params["on"], serde_json::json!(false));
        assert_eq!(previews[1].params["scale"], serde_json::json!(2));
    }
}
