//! The five detached panels, all assembled on the one shared window control.
//!
//! `ui-egui` builds these on `popover::Popover`: the colour picker, the layout settings, the
//! stroke settings, the variable picker and the style picker. This module is the GPUI assembly of
//! the same five, using [`crate::widgets::popover::Popover`] — whose header title is a **slot**, so
//! each window can put its own content there (a swatch, a create button, a count chip, …).
//!
//! The window owns only chrome, placement and drag. Bodies are built from the shared widget
//! library (search field, number fields, icon buttons, text), and every action goes through the
//! very same `CmdQueue` the right panel uses, so the engine sees the exact same commands.

use crate::canvas::geom::Color32;
use crate::panels::right::data::{EffectRow, GridRow, PanelData, TextData};
use crate::panels::right::variables::{self, VariablesData, VariablesEditor};
use crate::panels::{Cmd, Cmds};
use crate::picker_icons::{self, PickerIcon};
use crate::widgets::menu::menu_popup_in;
use crate::widgets::popover::{self, HEADER_H, Open, Surface, WIDTH, popover_body, popover_window, title_text};
use crate::widgets::typ_icons::TypIcon;
use crate::widgets::{
    Icon, IconButton, SegItem, Segmented, Slider, Slot, TextEdit, TextField, number_field, number_field_ranged, pt, rgba, search_field, text_field, tokens,
};
use crate::widgets::{MenuRow, dropdown};
use figcraft_i18n::tr;
use gpui::{
    AnyElement, App, Bounds, Corners, Entity, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, PathBuilder, Pixels, Point, SharedString,
    Size, Window, div, point, prelude::*, px, size,
};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Half of a 24 pt icon button. Callers hand us the press point; the placement rule wants the
/// button's top (`ui-egui/src/popover.rs`: "its top on `y` (the button's top)").
const BUTTON_HALF: f32 = 12.0;

thread_local! {
    /// 开窗触发按钮的**跨帧稳定**矩形（与 `widgets::dropdown::ANCHORS` 同构）。
    ///
    /// 右栏每帧重建元素树，按钮上挂的 `bounds_probe` cell 也每帧新建。于是「点下去那一帧」
    /// 读到的往往是**还没被 prepaint 写入的空 cell**，`button_top` 只能退回鼠标位置 − 12：
    /// 这就是「第一次打开窗口顶部没对齐按钮、鼠标移开刷新后才到位」的根因。
    ///
    /// 按稳定 key 注册：按钮每帧 prepaint 写同一个 cell，点击时读到的永远是**上一帧量到的
    /// 真实按钮顶部**。键用触发按钮的 trigger（每个开窗按钮唯一且跨帧不变）。
    static WINDOW_TRIGGER_TOPS: RefCell<HashMap<String, Rc<Cell<Option<Bounds<Pixels>>>>>> =
        RefCell::new(HashMap::new());
}

/// 取（或建）某开窗触发按钮的跨帧矩形句柄。同一 key 永远返回同一个 `Rc<Cell>`。
pub(crate) fn window_trigger_top(key: &str) -> Rc<Cell<Option<Bounds<Pixels>>>> {
    // 键里带窗口身份：两个编辑器窗口的同一个 key（"stroke" / "layout" / paint 行）原本会
    // 共用一格 bounds，最后画的窗口覆盖另一个的坐标，点前一个窗口的按钮会飘到别的窗口
    //（CodeRabbit PR#30/#31）。
    let key = format!("{}:{}", crate::windows::current(), key);
    WINDOW_TRIGGER_TOPS.with(|m| m.borrow_mut().entry(key).or_insert_with(|| Rc::new(Cell::new(None))).clone())
}

/// 开窗触发按钮**此刻**的窗口 y 顶部。优先用跨帧探针量到的真实矩形；首次绘制前探针为空时
/// 才退回按点按位置估算（这是最后一帧兜底，正常情况下永远走不到）。
///
/// 所有右栏开窗按钮都通过这里取顶部 —— 这是唯一入口，所以「按下即对位」是全局保证，
/// 不再需要「同一帧禁掉第二次刷新」这种掩盖症状的 hack。
pub(crate) fn anchored_top(key: &str, window: &Window) -> Pixels {
    window_trigger_top(key).get().map(|b| b.origin.y).unwrap_or_else(|| window.mouse_position().y - px(BUTTON_HALF))
}

/// 变量 / 文本样式列表的最大可见高度（超出滚动）。必须是常量：从 `body_h` 派生会被
/// `measured_h` 的「量到多少就多高」反馈放大，每帧长一截。
const LIST_MAX_H: f32 = 320.0;

/// 样式编辑器属性行里那颗左侧 `Dropdown` 的宽度：卡片 304 − 左缩进 `PAD` 16 − 右缩进
/// `PAD_RIGHT` 8 − 尾部两颗 24 pt 图标按钮 48。与右栏 `field_row_full_trailing` 的字段列同宽，
/// 这样两个面板里同一行的内缩与图标位置完全一致。
const STYLE_ROW_FIELD_W: f32 = 232.0;

/// Which of the five old popovers is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowKind {
    Layout,
    Stroke,
    Color,
    Variables,
    TextStyles,
    /// 变量编辑器的右侧列表（egui `dialogs/variables_panel.rs::table()`）。
    /// 与 `Variables`（变量**绑定选择器**弹窗）是两个不同的窗口。
    VariablesTable,
    /// 详细排版设置（egui `typography.rs::type_settings_panel`）：Basics / Details /
    /// Variable 三页签的浮层，正文全部用通用 `Setting` 行（chips + 数字框）搭出来。
    TypeSettings,
    /// 效果设置小窗：点右栏效果行的方框弹出，装该效果的设置项与类型切换。
    EffectSettings,
    /// 导出设置小窗：点右栏导出行的 Dots 弹出，装 egui `export_panel::advanced_options`
    /// 的勾选项与两组单选（VD_COMPAT / Color profile）。egui 里是下拉，但单选器在下拉里
    /// 不好放，所以按用户要求用通用弹窗承载（内容 1:1 照抄 egui）。
    ExportSettings,
    /// 本地样式编辑器（egui `style_picker::editor`）：与上面几个设置窗共用
    /// `WindowState` 通用弹窗卡 + `Setting` 通用行，只是分派标识不同，不是另一套弹窗实现。
    StyleEditor,
    /// 平滑圆角小窗：右栏 Appearance 圆角行旁的小按钮打开，正文只有一条通用滑条。
    CornerSmoothing,
    /// 字体选择窗：Typography 的字体族控件打开，列表每项用它自己的字体渲染名称做预览。
    FontPicker,
}

/// One clickable row of a list window (a variable, a style, a create action).
#[derive(Clone)]
pub(crate) struct Row {
    pub group: Option<SharedString>,
    pub label: SharedString,
    pub trail: Option<SharedString>,
    pub cmd: &'static str,
    pub params: Value,
    /// Font-picker rows render their label in this family, so the item name doubles as a live
    /// preview of the font's own style. `None` for ordinary list windows.
    pub font_family: Option<String>,
}

impl Row {
    pub fn new(label: impl Into<SharedString>, cmd: &'static str, params: Value) -> Self {
        Self { group: None, label: label.into(), trail: None, cmd, params, font_family: None }
    }
    pub fn font_family(mut self, family: impl Into<String>) -> Self {
        self.font_family = Some(family.into());
        self
    }
    pub fn group(mut self, name: impl Into<SharedString>) -> Self {
        self.group = Some(name.into());
        self
    }
    pub fn trail(mut self, text: impl Into<SharedString>) -> Self {
        self.trail = Some(text.into());
        self
    }
}

/// One option of a setting's segmented choice (egui's `menu_item` behind a `dropdown`).
#[derive(Clone)]
pub(crate) struct Opt {
    pub label: SharedString,
    pub cmd: &'static str,
    pub params: Value,
    /// 有值时用排版对话框字形（`typ-*.svg`）画格，label 退为 tooltip/无障碍文本。
    /// Type settings  Basics 页的对齐 / 装饰线 / 大小写 / 列表 / 截断全是图标格。
    pub glyph: Option<crate::widgets::typ_icons::TypIcon>,
    /// 有值时在**点击那一刻**用实时状态算出 `params`，而不是用构造时拍下的快照。
    /// 效果设置小窗里数值 / 开关 / 枚举共用一份可变 effect：任何一个控件写回后，
    /// 其余控件的命令都必须从**最新** effect 起改，否则会把彼此的修改覆盖掉
    /// （先改 X 再改 Y，Y 的 `effect.set` 带着开窗时的旧 X）。`params` 退为占位。
    pub params_fn: Option<std::rc::Rc<dyn Fn() -> Value>>,
}

impl Opt {
    pub fn new(label: impl Into<SharedString>, cmd: &'static str, params: Value) -> Self {
        Self { label: label.into(), cmd, params, glyph: None, params_fn: None }
    }

    /// 一个 params 在点击时才算的选项：`params_fn` 读实时共享状态。
    pub fn dynamic(label: impl Into<SharedString>, cmd: &'static str, params_fn: std::rc::Rc<dyn Fn() -> Value>) -> Self {
        Self { label: label.into(), cmd, params: Value::Null, glyph: None, params_fn: Some(params_fn) }
    }

    /// 点击时真正要下发的 params：有懒算函数就用它，否则用构造时的快照。
    pub fn now(&self) -> Value {
        match &self.params_fn {
            Some(f) => f(),
            None => self.params.clone(),
        }
    }
    /// 把这一格改成 lizard 的排版字形（U̲ / AG / 项目符号…），label 仍保留作语义标注。
    pub fn with_glyph(mut self, g: crate::widgets::typ_icons::TypIcon) -> Self {
        self.glyph = Some(g);
        self
    }
    fn item(&self) -> crate::widgets::SegItem {
        match self.glyph {
            Some(g) => crate::widgets::SegItem::Svg(g),
            None => crate::widgets::SegItem::text(self.label.clone()),
        }
    }
}

/// One line of a settings window (Layout / Stroke).
#[derive(Clone)]
pub(crate) enum Setting {
    Caption {
        tab: u8,
        text: SharedString,
    },
    /// A labelled row of choices; `active` is the raised cell. `None` = nothing chosen
    /// (a mixed selection, or a value this node does not have) — the design's `State=Rest`.
    Options {
        tab: u8,
        label: SharedString,
        active: Option<usize>,
        options: Vec<Opt>,
    },
    /// 定宽的图标分段行（Type settings Basics 页）：左标签 + 右侧 SVG 字形条，整条 `bar_w` 宽、
    /// 格子均分；`options` 带 `glyph` 时画 lizard 的 `typ-*.svg`。`drill_in` 是行尾的 `>` 按钮
    /// （文本修饰用），点击把 `active tab` 切到目标页签。
    IconOptions {
        tab: u8,
        label: SharedString,
        active: Option<usize>,
        bar_w: f32,
        options: Vec<Opt>,
        /// 行尾 `>` 按钮：与紧跟其后的 `ExpandedRows.flag` 共用同一份标志，点击翻转即展开/收起。
        drill_in: Option<Rc<RefCell<bool>>>,
    },
    /// A labelled numeric field; its commit was wired to the command queue at construction.
    Number {
        tab: u8,
        label: SharedString,
        field: Entity<TextField>,
    },
    /// 一条滑条行（Type settings 的可变字体轴）：左标签 + 右侧通用 `Slider`。
    /// `on_change` 在拖动时连续回调，直接往命令队列推命令（同一条命令会被队列合并）。
    Slider {
        tab: u8,
        label: SharedString,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
        suffix: SharedString,
        #[allow(clippy::type_complexity)]
        on_change: Rc<dyn Fn(f32, &mut gpui::Window, &mut gpui::App)>,
    },
    /// 颜色属性行（Figma `styles_properties_form`）：16×16 色板 + 名称。空 label 时就是
    /// 那个独立预览块。
    Swatch {
        tab: u8,
        label: SharedString,
        paint: figcraft_color::Paint,
        /// `Some(i)` = 可点开的属性行（`i` 是 paints 下标，点击打开取色器）；
        /// `None` = 顶部那个纯预览块，不可点。
        index: Option<usize>,
    },
    /// 颜色样式的 paint 行（lizard `color_style_editor`）：行 304×32，
    /// `ColorControl` 220×24 @ `x+16`（色块 + hex），眼睛 24×24 @ `x+244`，
    /// 减号 24×24 @ `x+272`。
    PaintRow {
        tab: u8,
        style_id: String,
        index: usize,
        paint: figcraft_color::Paint,
        /// `ColorField` 的两个内嵌编辑框（hex / 透明度数值）：实体必须在有 `&mut App` 的
        /// 构造期建立，渲染期只负责摆位置。
        hex_ed: Entity<TextEdit>,
        op_ed: Entity<TextEdit>,
    },
    /// 颜色样式的预览区（lizard `color_style_editor`：`304×120` 区，内缩 16 后
    /// `272×104` 的色块，`style_paint_preview`）。
    PaintPreview {
        paint: figcraft_color::Paint,
    },
    /// 文本样式的预览框（lizard `text_style_editor`：304×120 灰底，渲染 "Rag 123" 采样字）。
    TextPreview {
        text: serde_json::Value,
    },
    /// 效果样式的渲染预览（lizard `effect_style_preview`：304×152 棋盘底 + 渲染后的椭圆样本）。
    EffectPreview {
        effects: serde_json::Value,
    },
    /// 网格样式的示意图（lizard `grid_style_preview`：棋盘底 + 居中网格/行列样本）。
    GridPreview {
        grids: serde_json::Value,
    },
    /// 精确留白（lizard 的 `ui.add_space(x)`）。StyleEditor 的正文容器 gap=0，间距全靠它控制，
    /// 不能再用通用 `popover_body()` 的 8pt gap（会和各行自带 margin 叠加成 16）。
    Spacer {
        tab: u8,
        h: f32,
    },
    /// Properties 分节标题行（lizard 4 个编辑器共用）：304×32，标题在 `x+16`、
    /// `semibold(11.0)` / `t.text`，行尾 24×24 的「+」按钮在 `x+284`（`right()-20`）。
    /// text 编辑器是 304×24 且没有「+」。`menu` 为空 = 不画按钮；「Add fill」等文字只是
    /// 按钮的 tooltip，**不是可见标签**。点击把候选项追加进 `array` 后整体写回 `style.update`。
    SectionTitle {
        tab: u8,
        text: SharedString,
        h: f32,
        style_id: String,
        array: serde_json::Value,
        menu: Vec<(SharedString, serde_json::Value)>,
        /// 菜单锚点（点「+」时回填按钮矩形）。
        anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    },
    /// 文本样式 Properties 下的三行紧凑控件（lizard `text_style_editor` 的 `ui.scope`，
    /// `item_spacing = (8,8)`，每行先 `add_space(16)`）：
    ///   行1：字体族下拉 248×24
    ///   行2：字重下拉 120 + 字号 120
    ///   行3：行高 120 + 字距 120 + Dots
    /// 全部无左侧标签（字体族/字重靠下拉当前值，行高/字距靠字段自带的 glyph）。
    TextControls {
        tab: u8,
        family: Rc<RefCell<SharedString>>,
        families: Vec<SharedString>,
        font_style: Rc<RefCell<SharedString>>,
        font_styles: Vec<SharedString>,
        size_field: Entity<TextField>,
        line_field: Entity<TextField>,
        spacing_field: Entity<TextField>,
        style_id: String,
        text: Rc<RefCell<serde_json::Value>>,
        family_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
        style_anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    },
    /// 效果样式的属性行（lizard `effect_style_editor`）：行 304×32，类型下拉 + 眼睛 + 减号。
    ///
    /// 行本身走右栏的**统一网格**（`field_row_full_trailing`）：左缩进 `PAD` 16、右缩进
    /// `PAD_RIGHT` 8、尾部两颗 24 pt `IconButton` 各占一个 `SLOT`；标签是 `Dropdown::fill()`
    /// 那一颗（与右栏效果行、填充行同源），不再手写 flex 行 + 裸文字。
    EffectRow {
        tab: u8,
        style_id: String,
        index: usize,
        label: SharedString,
        ty: &'static str,
        visible: bool,
        effect: serde_json::Value,
        /// 完整 effects 数组（编辑时整体写回 `style.update {value}`）。
        array: serde_json::Value,
        /// 类型下拉的菜单锚点（点按钮时回填按钮矩形）。
        anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    },
    /// 网格样式的属性行（lizard `grid_style_editor`）：行 304×32，类型下拉 + 眼睛 + 减号。
    /// 与 `EffectRow` 同构（同一套统一网格 + `Dropdown` + `IconButton`）。
    GridRow {
        tab: u8,
        style_id: String,
        index: usize,
        label: SharedString,
        visible: bool,
        grids: serde_json::Value,
        anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
    },
    /// 多行输入（lizard `TextEdit::multiline(...).desired_rows(2)`）：caption 紧贴、
    /// 输入框高度 `rows × 24`。
    Multiline {
        tab: u8,
        label: SharedString,
        field: Entity<TextField>,
        rows: f32,
    },
    /// 垂直表单行（Figma `styles_properties_form`）：10 pt 标签在上、整宽输入框在下。
    /// `Number` 是横排（76 pt 标签 + 控件），样式编辑器的 Name / Description 要的是这种。
    /// 只加布局能力，不指定任何颜色——墨色与字段底色仍由通用 `TextField` 自己决定。
    Field {
        tab: u8,
        label: SharedString,
        field: Entity<TextField>,
    },
    /// Type settings 顶部的「预览」大框（lizard `type_settings_basics` 的 `previewBox`）。
    /// 所有页签共用、只画一次；内容是选中文本的字面，灰底、11 pt、次要色。
    Preview {
        text: SharedString,
    },
    /// Details 页的粗体分组标题（lizard `details_section`：10.5 pt semibold + 下方一条分隔线）。
    GroupTitle {
        tab: u8,
        text: SharedString,
    },
    /// 一条 1 px 分隔线（Basics 页预览框 / 上下两组之间）。
    Divider {
        tab: u8,
    },
    /// 标签 + 当前值 + 通用下拉（Wrap style：自动 / 平衡 / 美观）。
    /// 触发按钮用通用 `widgets::dropdown`（Input 模板同源），弹层用通用 `menu_popup_in`
    /// （`Menu::show`：菜单表面 / hover / 防溢出 / 滚动）。浮层占着根视图唯一的 `popup` 槽，
    /// 开不了第二个 `Popup::Menu`，所以菜单作为浮层 `render()` 多返回的一个**窗口级兄弟元素**
    /// 挂在根视图的 `els` 上（与浮层卡片同级，因此用窗口坐标、不被滚动裁剪）。
    Dropdown {
        tab: u8,
        label: SharedString,
        /// 当前显示值，选中后就地更新。
        current: Rc<RefCell<SharedString>>,
        /// 触发按钮的窗口矩形（`Dropdown::on_click` 回填），作为通用菜单的锚点。
        anchor: Rc<RefCell<Option<Bounds<Pixels>>>>,
        options: Vec<Opt>,
    },
    /// Details 页 `details_toggle` 的 `— / ✓` 开关行（hanging / features）。
    /// `on` 是当前值（retained，点一次翻转并发对应命令），`supported=false` 时画禁用态。
    Toggle {
        tab: u8,
        label: SharedString,
        on: Rc<RefCell<bool>>,
        supported: bool,
        on_cmd: Opt,
        off_cmd: Opt,
    },
    /// 一条单选行（egui `widgets::radio`）：导出高级选项里的 VD_COMPAT / Color profile。
    /// `on` 是当前是否选中；点击未选中项才推 `cmd`，选中项保持不动（与 egui 同义）。
    Radio {
        tab: u8,
        label: SharedString,
        on: bool,
        cmd: Opt,
    },
    /// 一条只读小标题（egui `widgets::label(..., text_secondary)`）：单选组前的分组名。
    SubLabel {
        tab: u8,
        text: SharedString,
    },
    /// 由行尾 `>` 展开的一组子行（`decoration_details`：样式 / 粗细 / 跳过下伸部）。
    /// `flag` 与 `Setting::IconOptions.drill_in` 共用；false 时整组不渲染，点 `>` 翻转后显示。
    ExpandedRows {
        tab: u8,
        flag: Rc<RefCell<bool>>,
        rows: Vec<Setting>,
    },
}

/// Colour models the value row can show, in the same order as `ui-egui::colorpicker::MODELS`.
const COLOR_MODELS: [&str; 7] = ["Hex", "RGB", "HSL", "HSB", "LCH", "OKLCH", "CSS"];

thread_local! {
    /// Colours picked in any colour picker this session, newest first (ui-egui
    /// `session.recent_colors`), so closing and reopening the picker keeps them.
    static RECENT_COLORS: RefCell<Vec<Color32>> = const { RefCell::new(Vec::new()) };
    /// The page's most used solid colours, cached per (document, page).
    static PAGE_COLORS: RefCell<(Option<(usize, figcraft_doc::NodeId)>, Vec<Color32>)> =
        const { RefCell::new((None, Vec::new())) };
}

fn remember_recent(c: Color32) {
    RECENT_COLORS.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|x| x.0 != c.0);
        r.insert(0, c);
        r.truncate(14);
    });
}

fn recent_colors() -> Vec<Color32> {
    RECENT_COLORS.with(|r| r.borrow().clone())
}

/// Recompute the page palette when the document or page changed. Called once per render; the
/// expensive walk only runs on a document/page change.
pub(crate) fn cache_page_colors(doc: &figcraft_doc::Document, page: figcraft_doc::NodeId) {
    let key = (doc as *const figcraft_doc::Document as usize, page);
    PAGE_COLORS.with(|c| {
        let mut c = c.borrow_mut();
        if c.0 == Some(key) {
            return;
        }
        c.1 = figcraft_engine::cmd::styles::document_colors(doc, page).into_iter().take(21).map(|(col, _)| picker_c32(col)).collect();
        c.0 = Some(key);
    });
}

fn page_colors() -> Vec<Color32> {
    PAGE_COLORS.with(|c| c.borrow().1.clone())
}

/// A 6-column grid of colour swatches, the shape both the page and the recent rows use.
fn swatch_grid(colors: Vec<Color32>, pick: Rc<dyn Fn(Color32, &mut Window, &mut App)>) -> gpui::Div {
    let mut grid = div().w(pt(240.0)).grid().grid_cols(6).gap(pt(6.0));
    for col in colors {
        let p = pick.clone();
        grid =
            grid.child(crate::widgets::swatch(figcraft_color::Paint::solid(picker_color(col)), 20.0).on_click(move |window, cx| p(col, window, cx)));
    }
    grid
}

fn picker_color(c: Color32) -> figcraft_color::Color {
    figcraft_color::Color::from_rgba8(c.0[0], c.0[1], c.0[2], c.0[3])
}

pub(crate) fn picker_c32(c: figcraft_color::Color) -> Color32 {
    let [r, g, b, a] = c.to_rgba8();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

/// The three field values in CSS display units (ui-egui `Polar::values`), or `None` for the
/// text models Hex / CSS.
fn model_values(model: usize, c: Color32) -> Option<[f64; 3]> {
    let col = picker_color(c);
    Some(match COLOR_MODELS.get(model).copied()? {
        "RGB" => {
            let [r, g, b, _] = c.0;
            [f64::from(r), f64::from(g), f64::from(b)]
        }
        "HSL" => {
            let x = figcraft_color::Hsl::from_color(col);
            [f64::from(x.h), f64::from(x.s * 100.0), f64::from(x.l * 100.0)]
        }
        "HSB" => {
            let x = figcraft_color::Hsb::from_color(col);
            [f64::from(x.h), f64::from(x.s * 100.0), f64::from(x.b * 100.0)]
        }
        "LCH" => {
            let x = figcraft_color::Lch::from_color(col);
            [f64::from(x.l), f64::from(x.c), f64::from(x.h)]
        }
        "OKLCH" => {
            let x = figcraft_color::Oklch::from_color(col);
            [f64::from(x.l * 100.0), f64::from(x.c), f64::from(x.h)]
        }
        _ => return None,
    })
}

/// Clamp / wrap one edited field to the model's range (ui-egui `PolarField`).
fn clamp_model_value(model: usize, k: usize, x: f64) -> f64 {
    let wrap = |v: f64| v.rem_euclid(360.0);
    match (COLOR_MODELS.get(model).copied(), k) {
        (Some("RGB"), _) => x.clamp(0.0, 255.0),
        (Some("HSL" | "HSB"), 0) => wrap(x),
        (Some("HSL" | "HSB"), _) => x.clamp(0.0, 100.0),
        (Some("LCH"), 0) => x.clamp(0.0, 100.0),
        (Some("LCH"), 1) => x.clamp(0.0, 230.0),
        (Some("LCH"), _) => wrap(x),
        (Some("OKLCH"), 0) => x.clamp(0.0, 100.0),
        (Some("OKLCH"), 1) => x.clamp(0.0, 0.5),
        (Some("OKLCH"), _) => wrap(x),
        _ => x,
    }
}

fn model_decimals(model: usize, k: usize) -> usize {
    if COLOR_MODELS.get(model) == Some(&"OKLCH") && k == 1 { 2 } else { 0 }
}

/// Turn edited field values back into a colour, keeping the current alpha.
fn model_color(model: usize, v: [f64; 3], c: Color32) -> Color32 {
    let a = picker_color(c).a;
    let [x, y, z] = v.map(|v| v as f32);
    let col = match COLOR_MODELS.get(model).copied().unwrap_or("Hex") {
        "RGB" => figcraft_color::Color::new(x / 255.0, y / 255.0, z / 255.0, a),
        "HSL" => figcraft_color::Hsl { h: x, s: y / 100.0, l: z / 100.0 }.to_color(),
        "HSB" => figcraft_color::Hsb { h: x, s: y / 100.0, b: z / 100.0 }.to_color(),
        "LCH" => figcraft_color::Lch { l: x, c: y, h: z }.to_color(),
        "OKLCH" => figcraft_color::Oklch { l: x / 100.0, c: y, h: z }.to_color(),
        _ => return c,
    };
    picker_c32(col.with_alpha(a))
}

/// The paint a Fill/Stroke row's picker edits. Lets the picker switch the paint kind and its
/// blend mode in place (ui-egui `show`'s type segments and blend dropdown).
#[derive(Clone)]
pub(crate) struct PaintPicker {
    pub paint: figcraft_color::Paint,
    /// Write the whole paint back through the row's own command (`selection.setPaint`).
    pub on_write: Rc<dyn Fn(figcraft_color::Paint, &mut Window, &mut App)>,
    /// 该 paint 所在的样式列表（填充/描边）。取色器头部的「+」用它弹出样式面板。
    pub styles: Vec<Row>,
}

/// 样式编辑器的一行属性（egui `property_row`）：paint 带 14×14 色板，其余带类型图标。
#[derive(Clone)]
pub(crate) enum StyleProp {
    /// `index` 是它在样式 paints 数组里的原始下标（egui `EditPaint(id, i)` 用同一个 i）。
    Paint {
        label: String,
        paint: figcraft_color::Paint,
        index: usize,
    },
    Icon {
        label: String,
    },
}

/// 样式编辑器正文（egui `style_picker::editor`）：Name / Description 两个输入框 + 属性行 +
/// 是否有选区（决定要不要画 "Update from selection"）。全部照源码字段来。
#[derive(Clone)]
pub(crate) struct StyleEditState {
    pub id: String,
    pub has_selection: bool,
    /// 完整 paints 数组：属性行点开取色器后要整体写回 `style.update {value}`。
    pub paints: Vec<figcraft_color::Paint>,
    /// 文本样式的完整 `TextStyle` JSON：编辑某一字段时整体写回 `style.update {value}`。
    pub text: Option<serde_json::Value>,
    /// 效果样式的完整 effects 数组（每行带原始 JSON，供写回 `style.update {value}`）。
    pub effects: Option<Vec<EffectRow>>,
    /// 网格样式的完整 grids 数组（每行带原始 JSON，供写回 `style.update {value}`）。
    pub grids: Option<Vec<GridRow>>,
    /// 打开取色器用（`PopupReqs` 只有面板侧才有）。
    pub popups: crate::panels::PopupReqs,
    /// 属性行点开的**子浮层**（取色器 / 排版设置）：挂在样式面板自己身上、画在它左侧，
    /// 不走 root 的 `popup` 单槽 —— 那条路会用一个新弹窗替换掉样式面板本身
    /// （用户 2026-11-15：「怎么能打开一个关一个呢」）。
    pub picker: Rc<RefCell<Option<Box<WindowState>>>>,
}

/// 样式编辑弹窗刷新用的文档快照：命令回写文档后，据此重建正文 `settings`。
struct StyleSnap {
    ty: &'static str,
    name: String,
    description: String,
    paints: Vec<figcraft_color::Paint>,
    text: Option<serde_json::Value>,
    effects: Option<Vec<EffectRow>>,
    grids: Option<Vec<GridRow>>,
}

#[derive(Clone)]
struct ColorState {
    /// Shared: picking anywhere in the picker must refresh the SV square, hue and alpha strips.
    current: Rc<Cell<Color32>>,
    /// `Some` when opened from a Fill/Stroke row: shows the paint-kind and blend controls.
    paint: Option<PaintPicker>,
    /// The live paint behind `paint`, kept in step with `on_write`, so the kind / stop editors
    /// always read the value that was just written instead of the one the picker opened with.
    paint_state: Option<Rc<RefCell<figcraft_color::Paint>>>,
    /// Selected gradient stop (ui-egui's `stop_id`).
    grad_sel: Rc<Cell<usize>>,
    /// Figma 只保留一个「渐变」类型格，用它记住下次切回渐变时用哪种（`paintPickerFilter`）。
    grad_kind: Rc<Cell<usize>>,
    /// 色板集：0 = 在此页面，1 = 最近使用。
    swatch_set: Rc<Cell<u8>>,
    /// 色板集下拉的触发矩形（菜单开着时 `Some`）。
    swatch_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The blend-mode dropdown's trigger rectangle while its menu is open.
    blend_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 渐变类型下拉的触发矩形（菜单开着时 `Some`）。
    grad_kind_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 取色器自己的子浮层：点击渐变断点颜色 / 头部「+」时，在本卡片**左边**再开一张卡，
    /// 不占 root 的单槽，因此取色器本身不会被替换掉。
    sub: Rc<RefCell<Option<Box<WindowState>>>>,
    /// 由断点弹出的取色器：隐藏顶部的纯色/渐变切换条（用户要求）。
    nested: bool,
    /// 每张取色器固定不变的编号，用于滑条等跨帧拖拽状态。
    uid: usize,
    /// 色板（饱和度/明度）是否正在拖动。
    sv_drag: Rc<Cell<bool>>,
    /// 渐变色阶的拖动状态：每帧重建 UI，必须存在 ColorState 里才能跨帧。
    grad_drag: Rc<Cell<Option<usize>>>,
    grad_press: Rc<Cell<Option<Point<Pixels>>>>,
    grad_moved: Rc<Cell<bool>>,
    grad_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// 断点行里的通用输入控件（百分比 / Hex / 透明度）。每帧重建元素树，但控件实体必须
    /// 跨帧复用，否则一打字就被下一帧重置。断点数量变化时整体重建。
    stop_fields: Rc<RefCell<Vec<StopFields>>>,
    /// 上一次写进 `stop_fields` 的显示值，只有变化时才 `set_value`。
    stop_shown: Rc<RefCell<Vec<(String, String, String)>>>,
    /// Which of [`COLOR_MODELS`] the value row shows.
    model: Rc<Cell<usize>>,
    /// Last (model, rgba) the fields were written for, so typing isn't overwritten every frame.
    shown: Rc<Cell<(usize, [u8; 4])>>,
    /// The model dropdown's trigger rectangle while its menu is open (`None`: closed).
    model_anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Hex / CSS text field.
    hex: Entity<TextField>,
    /// The three numeric fields of RGB / HSL / HSB / LCH / OKLCH.
    fields: [Entity<TextField>; 3],
    /// Alpha percentage (all models except CSS).
    alpha: Entity<TextField>,
    on_pick: Rc<dyn Fn(Color32, &mut Window, &mut App)>,
    /// AA 对比度的选区上下文（背景色 + 文字判定），来自 `PanelData::contrast`。
    contrast: crate::panels::right::data::ContrastContext,
    /// 用户选的分类（`None` = 跟随 auto 判定）与等级；Rc<RefCell> 让菜单回调用 &App 也能改。
    aa_category: std::rc::Rc<std::cell::RefCell<Option<figcraft_color::ContrastCategory>>>,
    aa_level: std::rc::Rc<std::cell::RefCell<figcraft_color::ContrastLevel>>,
    /// AA 对比度设置下拉的锚点矩形（`Some` = 本帧显示菜单）。
    aa_anchor: std::rc::Rc<std::cell::RefCell<Option<Bounds<Pixels>>>>,
    /// 跨帧保留的 HSB（EGUI 用 `ui.data` temp 存同一份东西）：拖动 SV 时 hue 必须保持稳定，
    /// 否则每帧从 u8 颜色反算 hue 会有量化误差，色相手柄就会「微微抽动」（用户实测）。
    /// 只有缓存的 rgba 与当前色完全一致才复用，外部改色则重新反算。
    hsb_cache: std::rc::Rc<std::cell::Cell<Option<(f32, f32, f32, [u8; 4])>>>,
}

/// 渐变断点行里的通用输入控件：位置百分比 + 通用颜色行（Hex + 透明度）。
#[derive(Clone)]
struct StopFields {
    pct: Entity<TextField>,
    hex: Entity<TextEdit>,
    opacity: Entity<TextEdit>,
}

/// Everything an open detached window needs. Kept as data on the root view (a retained tree), not
/// rebuilt into a fresh element tree from a closure every frame.
#[derive(Clone)]
pub(crate) struct WindowState {
    kind: WindowKind,
    title: SharedString,
    open: Open,
    width: f32,
    pad: f32,
    surface: Surface,
    height: f32,
    /// 上一帧量到的**真实**卡片高度（`Popover::on_measure` 写入，`0` = 还没量过）。
    ///
    /// `height` 是构造期估算，只够用来算锚点抬升；一旦估算偏大，`clamp` 会把卡片顶死在
    /// 上边界、怎么拖都不动（用户 2026-11-15：取色器「跟他绑死在一起了」）。
    measured_h: Rc<Cell<f32>>,
    /// `Some` once the header has been dragged (egui's `Place::At`).
    place: Option<Point<Pixels>>,
    /// 已经完成过一次定位。`measured_h` 修正会改 `height`，若每帧都重新 `open_pos`/`clamp`，
    /// 卡片会在「量到真实高度」的那一帧跳一下（用户 2026-11-16：Type settings 点开后
    /// 移开鼠标才「刷新」）。定位一次后钉住原点，除非用户拖动。
    placed_once: bool,
    /// Exact window-space **top** of the button that opened this card, when the trigger
    /// reported it. `anchored_open` uses this instead of guessing from the press point.
    /// `None` keeps the old pointer-minus-half-button fallback (keyboard / command opens).
    exact_top: Option<Pixels>,
    origin: Point<Pixels>,
    /// Last viewport the card was laid out in. A drag clamps every step to it, exactly like
    /// egui's `clamp(at - grab, size, screen)`, so a pointer leaving the window cannot park the
    /// card outside it.
    viewport: Size<Pixels>,
    /// 通用「按住拖动」状态：按下 / 3pt 死区 / latch / 相对位移 / 收尾。
    /// 与尺寸分隔条（`resize_drag`）、通用对话框（`dialogs`）共用同一份 `hold_drag` 实现。
    /// 按下本身**不重绘**：GPUI 靠 ElementId 匹配 click 的 down/up，重绘会让头部按钮点不动。
    hold: crate::hold_drag::HoldDrag,
    /// 标题栏 / 头部左侧的拖动回调（整卡拖动）。**只绑在头部左侧**，不是整张卡片：
    /// 绑整卡会让「点表格行、点按钮」都开始拖窗口（用户 2026-11-07 指出）。
    drag_start: Option<Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>>,
    cmds: Cmds,
    rows: Vec<Row>,
    settings: Rc<RefCell<Vec<Setting>>>,
    /// 平滑圆角小窗的当前值（0..1，拖动时由通用滑条写回）；`Some` 时才画那条滑条。
    smoothing: Option<Rc<Cell<f32>>>,
    tabs: Option<(Vec<SharedString>, Rc<RefCell<u8>>)>,
    search: Option<Entity<TextField>>,
    query: Rc<RefCell<String>>,
    color: Option<ColorState>,
    /// 本地样式编辑器（egui `style_picker::editor`）的正文状态：两个输入框 + 属性行。
    style_edit: Option<StyleEditState>,
    /// 样式选择窗口（WindowKind::TextStyles）的 slot 类型：`text`/`paint`/`effect`/`grid`。
    /// 标题栏「+」按它推 `style.create {type}` 与默认名 —— egui `StyleSlot::create_type/default_name`。
    /// 四类样式窗口是同一类弹窗，只有这个值不同。
    style_create_ty: &'static str,
    /// 完整变量编辑器（只有 `VariablesTable` 用）：持有只读快照 + retained 交互状态。
    /// 延迟到 `body_element` 才创建 —— 那里才拿得到 `&mut App` 来建搜索框 / 值输入框。
    editor: Option<VariablesEditor>,
    variables_data: Option<VariablesData>,
    /// 视图级记住的 Full/Floating。`editor` 是延迟到 `body_element` 才建的，所以切换模式的
    /// 调用可能发生在建 editor 之前 —— 必须把它存下来，建好后再灌进去，否则 Full 永远不生效。
    editor_full: Option<bool>,
    var_on_full: Rc<dyn Fn(bool, &mut App)>,
    var_on_close: Rc<dyn Fn(&mut App)>,
    /// 变量面板里点颜色 swatch 时，通过它向根视图请求通用取色器窗口。
    var_popups: Option<crate::panels::PopupReqs>,
    /// 浮层内通用下拉的菜单卡片（窗口坐标，作为 `render()` 的兄弟元素返回）。每帧清空重建。
    overlays: Rc<RefCell<Vec<AnyElement>>>,
    /// 与 `overlays` 一一对应的关闭回调。菜单打开时在窗口根垫一层透明 backdrop，
    /// 点击卡片以外的任何位置都能把所有菜单关掉（通用行为，不再每个菜单各写一套）。
    overlay_dismiss: Rc<RefCell<Vec<Rc<dyn Fn(&mut Window, &mut App)>>>>,
    /// 当前浮层的关闭回调（`render()` 每帧写入），供菜单 `dismiss` 之外复用。
    dismiss: Rc<RefCell<Option<Rc<dyn Fn(&mut Window, &mut App)>>>>,
}

/// 文本样式的 `text` 对象 → `Type settings` 用的 `TextData` 快照。
///
/// 文本样式的编辑器与「选中文本节点」的 Type settings 是**同一个面板**（Figma 里也是同一张
/// 面板，只是写入目标不同）。面板正文只读 `TextData`，所以这里把样式里的那几个键读成快照；
/// 样式没写的键留空（面板就显示「未选」），与 Mixed 的显示一致。
fn style_text_data(v: &Value) -> TextData {
    /// 只接受面板认识的那几个枚举值，其余当空。
    fn one(v: &Value, k: &str, allowed: &[&'static str]) -> Option<&'static str> {
        let s = v.get(k).and_then(Value::as_str)?;
        allowed.iter().copied().find(|a| *a == s)
    }
    /// 行高 / 字距：数字 = px，`"150%"` = 百分比，`"AUTO"` = 空。
    fn num_or_pct(v: &Value, k: &str) -> Option<(f64, bool)> {
        match v.get(k)? {
            Value::Number(n) => n.as_f64().map(|x| (x, false)),
            Value::String(s) => {
                let s = s.trim();
                if let Some(p) = s.strip_suffix('%') {
                    p.trim().parse::<f64>().ok().map(|x| (x, true))
                } else {
                    s.parse::<f64>().ok().map(|x| (x, false))
                }
            }
            _ => None,
        }
    }
    let num = |k: &str| v.get(k).and_then(Value::as_f64);
    let flag = |k: &str| v.get(k).and_then(Value::as_bool);
    TextData {
        font_family: v.get("fontFamily").and_then(Value::as_str).map(str::to_owned),
        font_style: v.get("fontStyle").and_then(Value::as_str).map(str::to_owned),
        font_size: num("fontSize"),
        line_height: num_or_pct(v, "lineHeight"),
        letter_spacing: num_or_pct(v, "letterSpacing"),
        paragraph_spacing: num("paragraphSpacing"),
        auto_resize: one(v, "textAutoResize", &["none", "height", "widthAndHeight", "truncate"]),
        align_h: one(v, "textAlign", &["left", "center", "right", "justified"]),
        align_v: one(v, "textAlignVertical", &["top", "middle", "bottom"]),
        decoration: one(v, "textDecoration", &["none", "underline", "strikethrough"]),
        decoration_style: one(v, "textDecorationStyle", &["solid", "dotted", "wavy"]),
        decoration_thickness: num("textDecorationThickness"),
        decoration_skip_ink: flag("textDecorationSkipInk"),
        case: one(v, "textCase", &["original", "upper", "lower", "title", "smallCaps"]),
        leading_trim: one(v, "leadingTrim", &["none", "capHeight"]),
        list_style: one(v, "listStyle", &["none", "bulleted", "numbered"]),
        paragraph_indent: num("paragraphIndent"),
        hanging_punctuation: flag("hangingPunctuation"),
        hanging_list: flag("hangingList"),
        wrap_style: one(v, "wrapStyle", &["auto", "balance", "pretty"]),
        max_lines: v.get("maxLines").and_then(Value::as_u64).map(|x| x as u32),
        ..TextData::default()
    }
}

/// 文本样式的 Type settings 用**改写队列**：这批行都是写死的 `node.set { props }`，
/// 而样式编辑器必须写回 `style.update`（否则写到了当前选中的节点上，关掉面板就全丢）。
///
/// 改写只发生在样式自己的 Type settings 窗口里 —— 队列是派生出来的，外层按钮照旧。
fn style_write_queue(cmds: &Cmds, style_id: String, text: Rc<RefCell<Value>>) -> Cmds {
    let map: Rc<dyn Fn(Cmd) -> Cmd> = Rc::new(move |mut cmd: Cmd| {
        if cmd.id != "node.set" {
            return cmd;
        }
        let Some(props) = cmd.params.take().and_then(|p| p.get("props").cloned()) else {
            return Cmd { id: "style.update".into(), params: Some(json!({ "style": style_id, "value": text.borrow().clone() })) };
        };
        // 合并进样式的 text 对象后整份写回（`style.update` 的 `value` 是整值，不是补丁）。
        let updated = {
            let mut v = text.borrow_mut();
            if let (Some(dst), Some(src)) = (v.as_object_mut(), props.as_object()) {
                for (k, val) in src {
                    dst.insert(k.clone(), val.clone());
                }
            }
            v.clone()
        };
        Cmd { id: "style.update".into(), params: Some(json!({ "style": style_id, "value": updated })) }
    });
    cmds.mapped(map)
}

/// 一行“下拉式”设置（PR #823）：描边的枚举设置是列表，不是左右分格。
fn stroke_dropdown(tab: u8, label: SharedString, current: impl Into<SharedString>, options: Vec<Opt>) -> Setting {
    Setting::Dropdown { tab, label, current: Rc::new(RefCell::new(current.into())), anchor: Rc::new(RefCell::new(None)), options }
}

/// `node.set` 的 cap id 是 lowerCamelCase（`none`、`lineArrow`…），不是 Debug 名。
fn cap_param_id(c: figcraft_doc::StrokeCap) -> &'static str {
    use figcraft_doc::StrokeCap as C;
    match c {
        C::None => "none",
        C::Round => "round",
        C::Square => "square",
        C::LineArrow => "lineArrow",
        C::TriangleArrow => "triangleArrow",
        C::ReversedTriangle => "reversedTriangle",
        C::CircleFilled => "circleFilled",
        C::DiamondFilled => "diamondFilled",
    }
}

fn cap_options(key: &'static str) -> Vec<Opt> {
    figcraft_doc::StrokeCap::ALL
        .iter()
        .map(|c| {
            let mut props = serde_json::Map::new();
            props.insert(key.to_string(), json!(cap_param_id(*c)));
            Opt::new(tr(c.label()), "node.set", json!({ "props": props }))
        })
        .collect()
}

/// 描边 Dashes 字段的文本 → 虚线数组（egui `stroke.rs::parse_dashes`）：逗号分隔的
/// 非负数；空段忽略，出现非数字 / 负数 / 非有限值即整条无效。
fn parse_dashes(text: &str) -> Option<Vec<f64>> {
    let values = text
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0))
        .collect::<Option<Vec<f64>>>()?;
    (!values.is_empty()).then_some(values)
}

#[cfg(test)]
mod dash_tests {
    use super::parse_dashes;

    #[test]
    fn custom_dashes_parse_like_egui() {
        assert_eq!(parse_dashes("10, 20, 10"), Some(vec![10.0, 20.0, 10.0]));
        assert_eq!(parse_dashes("4"), Some(vec![4.0]));
        // 空段忽略、浮点/负数/非数字即整条无效（egui `parse_dashes`）。
        assert_eq!(parse_dashes(" 4 , , 8 "), Some(vec![4.0, 8.0]));
        assert_eq!(parse_dashes("4, -1"), None);
        assert_eq!(parse_dashes("4, abc"), None);
        assert_eq!(parse_dashes("   "), None);
    }
}

/// 宽度剖面当前显示名：真实 preset、Custom（手改过）、Uniform（无）或 Mixed（多选不一致）。
fn profile_label(data: &PanelData) -> String {
    match &data.stroke_profile {
        None => tr("Mixed").to_string(),
        Some(None) => tr("Uniform").to_string(),
        Some(Some(p)) => p.preset().map_or_else(|| tr("Custom").to_string(), |w| tr(w.label()).to_string()),
    }
}

fn join_label(j: figcraft_doc::StrokeJoin) -> &'static str {
    match j {
        figcraft_doc::StrokeJoin::Miter => "Miter",
        figcraft_doc::StrokeJoin::Bevel => "Bevel",
        figcraft_doc::StrokeJoin::Round => "Round",
    }
}

impl WindowState {
    /// Pin the card's top to the exact top of its trigger button (window coordinates).
    /// Call this at the open site when the button reported its own `Bounds`; the press
    /// point is then used only to decide *which* button was pressed, never to place the card.
    pub(crate) fn anchor_to_button_top(&mut self, top: Pixels) {
        self.exact_top = Some(top);
    }

    /// 供根视图判断当前是否是某个特定窗口（例如变量面板已开时再点按钮就不要二次弹出）。
    pub(crate) fn kind(&self) -> WindowKind {
        self.kind
    }

    /// 高度由设计稿**固定**、不随内容测量的窗口。它们不吃 `measured_h` 回写，否则
    /// 「body 高度 = height + 列表 flex_1 撑满」会每帧把卡片顶高 40pt（越滚越大）。
    fn fixed_height(&self) -> bool {
        matches!(self.kind, WindowKind::FontPicker)
    }

    /// Base state; the five constructors below fill in the body model.
    fn base(kind: WindowKind, title: impl Into<SharedString>, cmds: Cmds, open: Open) -> Self {
        Self {
            kind,
            title: title.into(),
            open,
            width: WIDTH,
            pad: popover::SLOT_PAD,
            surface: Surface::Panel,
            height: 240.0,
            measured_h: Rc::new(Cell::new(0.0)),
            place: None,
            placed_once: false,
            exact_top: None,
            origin: Point { x: px(0.0), y: px(0.0) },
            viewport: size(px(0.0), px(0.0)),
            hold: crate::hold_drag::HoldDrag::default(),
            drag_start: None,
            cmds,
            rows: Vec::new(),
            settings: Rc::new(RefCell::new(Vec::new())),
            smoothing: None,
            tabs: None,
            search: None,
            query: Rc::new(RefCell::new(String::new())),
            color: None,
            style_edit: None,
            style_create_ty: "text",
            editor: None,
            variables_data: None,
            editor_full: None,
            var_on_full: Rc::new(|_, _| {}),
            var_on_close: Rc::new(|_| {}),
            var_popups: None,
            overlays: Rc::new(RefCell::new(Vec::new())),
            overlay_dismiss: Rc::new(RefCell::new(Vec::new())),
            dismiss: Rc::new(RefCell::new(None)),
        }
    }

    // ── ① 自动布局设置（egui `panels/layout_panel.rs::settings`, 标题 "Layout"）───────────────
    pub(crate) fn layout(cmds: Cmds, data: &PanelData, cx: &mut App, anchor: Point<Pixels>) -> Self {
        let mut w = Self::base(WindowKind::Layout, tr("Layout"), cmds, Open::Anchor(anchor));
        w.pad = 12.0;
        w.height = 200.0;
        let wrap = data.layout.wrap;
        let main = data.layout.main_align.unwrap_or("min");
        let gap_active = |main: &str, wrap: bool| -> usize {
            if wrap {
                if data.layout.content_align == Some("spaceBetween") { 1 } else { 0 }
            } else {
                match main {
                    "spaceBetween" => 1,
                    "spaceEvenly" => 2,
                    "spaceAround" => 3,
                    _ => 0,
                }
            }
        };
        let text_baseline = data.layout.cross_align == Some("baseline");
        let settings = vec![
            Setting::Options {
                tab: 0,
                label: tr("Strokes").into(),
                active: Some(0),
                options: vec![
                    Opt::new(tr("Included"), "node.set", json!({ "props": { "strokesIncluded": true } })),
                    Opt::new(tr("Excluded"), "node.set", json!({ "props": { "strokesIncluded": false } })),
                ],
            },
            Setting::Options {
                tab: 0,
                label: tr("Canvas stacking").into(),
                active: Some(0),
                options: vec![
                    Opt::new(tr("Last on top"), "node.set", json!({ "props": { "reverseZ": false } })),
                    Opt::new(tr("First on top"), "node.set", json!({ "props": { "reverseZ": true } })),
                ],
            },
            Setting::Options {
                tab: 0,
                label: "Text baselines".into(),
                active: Some(usize::from(text_baseline)),
                options: vec![
                    Opt::new(tr("Off"), "node.set", json!({ "props": { "crossAlign": "min" } })),
                    Opt::new(tr("Align"), "node.set", json!({ "props": { "crossAlign": "baseline" } })),
                ],
            },
            Setting::Options {
                tab: 0,
                label: "Gap mode".into(),
                active: Some(gap_active(main, wrap)),
                options: if wrap {
                    vec![
                        Opt::new(tr("Fixed"), "node.set", json!({ "props": { "contentAlign": "auto" } })),
                        Opt::new(tr("Auto"), "node.set", json!({ "props": { "contentAlign": "spaceBetween" } })),
                    ]
                } else {
                    vec![
                        Opt::new(tr("Fixed"), "node.set", json!({ "props": { "mainAlign": "min" } })),
                        Opt::new("Between", "node.set", json!({ "props": { "mainAlign": "spaceBetween" } })),
                        Opt::new("Evenly", "node.set", json!({ "props": { "mainAlign": "spaceEvenly" } })),
                        Opt::new("Around", "node.set", json!({ "props": { "mainAlign": "spaceAround" } })),
                    ]
                },
            },
        ];
        w.settings = Rc::new(RefCell::new(settings));
        let _ = cx; // no cached editor in this window yet
        w
    }

    // ── ② 描边高级设置（egui `panels/right/stroke.rs::stroke_settings_body`, 标题 "Stroke"）───
    pub(crate) fn stroke(cmds: Cmds, data: &PanelData, cx: &mut App, anchor: Point<Pixels>) -> Self {
        let mut w = Self::base(WindowKind::Stroke, tr("Stroke"), cmds.clone(), Open::Anchor(anchor));
        w.pad = 12.0;
        w.height = 320.0;
        // The Basic / Brush switch is the stroke's *type*: open on the one it already is, so a
        // brush stroke opens on Brush (user 2026-11-18).
        let initial_tab = if data.brush.is_some() { 1 } else { 0 };
        w.tabs = Some((vec!["Basic".into(), tr("Brush").into()], Rc::new(RefCell::new(initial_tab))));
        let weight = data.stroke_weight.unwrap_or(1.0);
        let dash = Rc::new(RefCell::new([weight * 4.0, weight * 4.0]));
        let mk_number = |cx: &mut App, value: f64, cmd: &'static str, apply: Rc<dyn Fn(f64) -> Value>| -> Entity<TextField> {
            let f = number_field(cx, 100.0, Some(value), "", 1.0, Slot::None);
            let queue = cmds.clone();
            f.update(cx, move |tf, _| {
                tf.set_on_commit(move |text, _, _| {
                    if let Ok(v) = text.trim().parse::<f64>() {
                        queue.push(Cmd { id: cmd, params: Some(apply(v)) });
                    }
                });
            });
            f
        };
        let d = dash.clone();
        let dash_value = d.borrow()[0];
        let dash_field = mk_number(
            cx,
            dash_value,
            "node.set",
            Rc::new(move |v| {
                let mut pair = *d.borrow();
                pair[0] = v.max(0.0);
                *d.borrow_mut() = pair;
                json!({ "props": { "dashes": pair } })
            }),
        );
        let g = dash.clone();
        let gap_value = g.borrow()[1];
        let gap_field = mk_number(
            cx,
            gap_value,
            "node.set",
            Rc::new(move |v| {
                let mut pair = *g.borrow();
                pair[1] = v.max(0.0);
                *g.borrow_mut() = pair;
                json!({ "props": { "dashes": pair } })
            }),
        );
        let miter_field = mk_number(cx, 28.6, "node.set", Rc::new(|v| json!({ "props": { "miterAngle": v } })));
        // 全部 6 个 preset（旧版只列 4 个，丢了 Rounded / Pinch）。
        let profiles: Vec<Opt> =
            figcraft_doc::WidthPreset::ALL.iter().map(|p| Opt::new(tr(p.label()), "stroke.widthProfile", json!({ "preset": p.id() }))).collect();
        // Brushes are read from external `.abr` files in the user's brushes directory; the engine
        // contains no built-in shapes. Group / folder / name keeps packs of hundreds selectable.
        let installed = figcraft_abr::infos();
        let mut brush_options = vec![Opt::new(tr("None"), "stroke.brush", json!({ "preset": Value::Null }))];
        // Show the selection's real brush, not a hardcoded "None" (user 2026-11-18).
        let mut selected_label = tr("None").to_string();
        for b in &installed {
            let label = if b.folder.is_empty() {
                format!("{} / {}", b.group, b.name)
            } else {
                format!("{} / {} / {}", b.group, b.folder.join(" / "), b.name)
            };
            if data.brush.as_ref().is_some_and(|sel| sel.preset == b.id) {
                selected_label = label.clone();
            }
            brush_options.push(Opt::new(label, "stroke.brush", json!({ "preset": b.id })));
        }
        // A preset the library no longer has (its .abr file was removed) still shows its id.
        let brush_label = if installed.is_empty() {
            "No .abr brushes — import a file".to_string()
        } else if let Some(sel) = data.brush.as_ref().filter(|b| !b.preset.is_empty()) {
            if selected_label == tr("None") { sel.preset.clone() } else { selected_label }
        } else {
            selected_label
        };
        // Brush tab: the brush's own fields (ui-egui `stroke::brush_row`): spacing, scatter,
        // size jitter and rotation jitter. Only when the selection has a brush.
        let brush_fields = data.brush.clone().map(|b| {
            let spacing = {
                let b = b.clone();
                mk_number(
                    cx,
                    b.spacing,
                    "node.set",
                    Rc::new(move |v| {
                        let mut nb = b.clone();
                        nb.spacing = v;
                        json!({ "props": { "brush": serde_json::to_value(nb.sanitized()).unwrap_or_default() } })
                    }),
                )
            };
            let scatter = {
                let b = b.clone();
                mk_number(
                    cx,
                    b.scatter,
                    "node.set",
                    Rc::new(move |v| {
                        let mut nb = b.clone();
                        nb.scatter = v;
                        json!({ "props": { "brush": serde_json::to_value(nb.sanitized()).unwrap_or_default() } })
                    }),
                )
            };
            let size_jitter = {
                let b = b.clone();
                mk_number(
                    cx,
                    b.size_jitter * 100.0,
                    "node.set",
                    Rc::new(move |v| {
                        let mut nb = b.clone();
                        nb.size_jitter = v / 100.0;
                        json!({ "props": { "brush": serde_json::to_value(nb.sanitized()).unwrap_or_default() } })
                    }),
                )
            };
            let rotation_jitter = {
                let b = b.clone();
                mk_number(
                    cx,
                    b.rotation_jitter * 100.0,
                    "node.set",
                    Rc::new(move |v| {
                        let mut nb = b.clone();
                        nb.rotation_jitter = v / 100.0;
                        json!({ "props": { "brush": serde_json::to_value(nb.sanitized()).unwrap_or_default() } })
                    }),
                )
            };
            [spacing, scatter, size_jitter, rotation_jitter]
        });
        // 行为驱动，不是静态列表：虚线字段仅在虚线时出现，miter 角仅 miter 连接，端帽仅开放路径。
        // 这些枚举设置是**下拉**而非左右分格（PR #823）：分格行宽只有 134pt，多出选项就被挤掉。
        //
        // Stroke style 是 Solid / Dash / Custom 三选（egui `stroke.rs::dash_settings`）：
        // - 空/None = Solid；一段或两段 = Dash（Dash + Gap 两个数字框）；
        // - 三段以上 = Custom（一条 Dashes 文本框，填逗号分隔的 dash,gap,dash,…）。
        // 窗口是点击时的快照（只有 StyleEditor 会随命令刷新），所以 Custom 的输入框在构造期
        // 就按 `data.dashes` 决定可见性，与 Dash/Gap 二选一 —— 和 egui 的行为一致。
        let pattern = data.dashes.as_deref().filter(|d| !d.is_empty());
        let custom = pattern.is_some_and(|d| d.len() > 2);
        let style_current = match &data.dashes {
            None => tr("Mixed").to_string(),
            Some(d) if d.is_empty() => tr("Solid").to_string(),
            Some(d) if d.len() > 2 => tr("Custom").to_string(),
            Some(_) => tr("Dash").to_string(),
        };
        // 从 Dash 切到 Custom 时先给一条三段样例，窗口重开后正好落到 Custom 分支显示文本框。
        let custom_seed = [weight * 4.0, weight * 2.0, weight * 4.0];
        let join_current = data.join.map_or_else(|| tr("Mixed").to_string(), |j| join_label(j).to_string());
        let mut settings = vec![stroke_dropdown(
            0,
            tr("Stroke style").into(),
            style_current,
            vec![
                Opt::new(tr("Solid"), "node.set", json!({ "props": { "dashes": [] } })),
                Opt::new(tr("Dash"), "node.set", json!({ "props": { "dashes": [weight * 4.0, weight * 4.0] } })),
                Opt::new(tr("Custom"), "node.set", json!({ "props": { "dashes": custom_seed } })),
            ],
        )];
        if custom {
            // Custom：一条 Dashes 文本框，值为当前数组用逗号拼出；提交时按 parse_dashes 解析回数组。
            // 用通用 `text_field` + `Setting::Field`，不自造输入控件。
            let text = pattern.map(|d| d.iter().map(|v| crate::widgets::input::fmt_num(*v)).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            let dashes_field = text_field(cx, 176.0, &text, "", Slot::None, Slot::None);
            let queue = cmds.clone();
            dashes_field.update(cx, move |tf, _| {
                tf.set_on_commit(move |typed, _, _| {
                    if let Some(list) = parse_dashes(typed) {
                        queue.push(Cmd { id: "node.set", params: Some(json!({ "props": { "dashes": list } })) });
                    }
                });
            });
            settings.push(Setting::Field { tab: 0, label: tr("Dashes").into(), field: dashes_field });
        } else if pattern.is_some() {
            settings.push(Setting::Number { tab: 0, label: tr("Dash").into(), field: dash_field });
            settings.push(Setting::Number { tab: 0, label: tr("Gap").into(), field: gap_field });
        }
        settings.push(stroke_dropdown(
            0,
            tr("Join").into(),
            join_current,
            figcraft_doc::StrokeJoin::ALL
                .iter()
                .map(|j| Opt::new(tr(join_label(*j)), "node.set", json!({ "props": { "strokeJoin": format!("{j:?}").to_lowercase() } })))
                .collect(),
        ));
        if data.join == Some(figcraft_doc::StrokeJoin::Miter) {
            settings.push(Setting::Number { tab: 0, label: tr("Miter angle").into(), field: miter_field });
        }
        if data.is_open_path {
            // 两端分开两行，各列全 8 种形状（旧版只有 3 个，且 id 用了引擎不认的 "butt"）。
            settings.push(stroke_dropdown(
                0,
                tr("Start point").into(),
                data.cap_start.map_or_else(|| tr("Mixed").to_string(), |c| tr(c.label()).to_string()),
                cap_options("startCap"),
            ));
            settings.push(stroke_dropdown(
                0,
                tr("End point").into(),
                data.cap_end.map_or_else(|| tr("Mixed").to_string(), |c| tr(c.label()).to_string()),
                cap_options("endCap"),
            ));
        }
        // 宽度剖面两个页签都放，列全 6 个；当前值取自选择（Mixed / Uniform / Custom / preset 名）。
        // 两个页签共用一份 current：Basic 里换了剖面，切到 Brush 要看到同一个值
        // （CodeRabbit PR#34）。
        let profile_current = Rc::new(RefCell::new(SharedString::from(profile_label(data))));
        settings.push(Setting::Dropdown {
            tab: 0,
            label: tr("Width profile").into(),
            current: profile_current.clone(),
            anchor: Rc::new(RefCell::new(None)),
            options: profiles.clone(),
        });
        settings.push(Setting::Caption { tab: 1, text: tr("Brush").into() });
        // Every brush comes from an external `.abr` file; "None" keeps a plain vector stroke.
        settings.push(Setting::Dropdown {
            tab: 1,
            label: tr("Brush").into(),
            current: Rc::new(RefCell::new(SharedString::from(brush_label))),
            anchor: Rc::new(RefCell::new(None)),
            options: brush_options,
        });
        if let Some([spacing, scatter, size_jitter, rotation_jitter]) = brush_fields {
            settings.push(Setting::Number { tab: 1, label: tr("Spacing").into(), field: spacing });
            settings.push(Setting::Number { tab: 1, label: tr("Scatter").into(), field: scatter });
            settings.push(Setting::Number { tab: 1, label: tr("Size jitter").into(), field: size_jitter });
            settings.push(Setting::Number { tab: 1, label: tr("Rotation jitter").into(), field: rotation_jitter });
        }
        settings.push(stroke_dropdown(
            1,
            tr("Brush library").into(),
            tr("Brush library").to_string(),
            vec![Opt::new(tr("Import brushes…"), "brush.import", json!({})), Opt::new(tr("Reload"), "brush.reload", json!({}))],
        ));
        settings.push(Setting::Dropdown {
            tab: 1,
            label: tr("Width profile").into(),
            current: profile_current,
            anchor: Rc::new(RefCell::new(None)),
            options: profiles,
        });
        w.settings = Rc::new(RefCell::new(settings));
        w
    }

    // ── ④ 变量弹窗（egui `panels/variable_ui.rs::picker`, 标题 "Variables"）─────────────────
    pub(crate) fn variables(cmds: Cmds, cx: &mut App, anchor: Point<Pixels>, rows: Vec<Row>) -> Self {
        let mut w = Self::base(WindowKind::Variables, tr("Variables"), cmds, Open::Anchor(anchor));
        w.surface = Surface::Menu;
        w.pad = 8.0;
        w.height = 300.0;
        w.rows = rows;
        w.add_search(cx, "Search variables");
        w
    }

    // ── ⑤ 详细排版设置（egui `typography.rs::type_settings_panel` + `type_settings_basics`）──
    //
    // egui 的这个浮层是「三页签 + 头部关闭按钮」的 Compact 面板：
    // `Basics`（对齐 / 装饰线 / 大小写 / 垂直裁切 / 列表 / 段落间距）、
    // `Details`（装饰线样式 / 粗细 / 跳过下伸部 / 大小写）、
    // `Variable`（可变字体的轴）。
    //
    // 这里**不新建任何控件**：页签用通用「设置窗口」自带的那条页签，行用 `Setting::Options`
    // （chips）与 `Setting::Number`（数字框）搭；正文与页签的排版完全交给 `settings_element`。
    // 与 egui 的两处差异都来自控件库的现状，不是另起一套：
    //   * egui 的装饰线 / 大小写 / 对齐是**图标**分段控件，通用 chips 只有文字（图标集里没有
    //     `TypeDialog*` 这些字形），所以这里用文字 chips；
    //   * `Variable` 页在 egui 里是滑杆；控件库没有滑杆，改用每条轴一个数字框（同样是通用控件）。
    // 第三页只在字体**确实是可变字体**时出现 —— egui 是把它画成禁用页签，通用页签没有禁用态，
    // 于是缺省就不出现，避免留一个点不动的死页签。
    pub(crate) fn type_settings(cmds: Cmds, text: &TextData, cx: &mut App, anchor: Point<Pixels>) -> Self {
        let mut w = Self::base(WindowKind::TypeSettings, tr("Type settings"), cmds, Open::Anchor(anchor));
        // 卡片 pad=8：标题行（页签）从 y=8 起，与右上角 X（top 8 / 24 高，中心 y=20）垂直居中对齐。
        // 正文另在 settings_element 里补 px(4)，把左右视觉内边距补回 12（见那里的 kind 判断）。
        w.pad = 8.0;
        // Basics 短、Details 长：高度用于抬升/夹取，正文交给滚动容器按内容延展。
        w.height = 520.0;
        let t = text.clone();
        let has_variable = !t.axes.is_empty();
        let mut tabs = vec![tr("Basics").into(), tr("Details").into()];
        if has_variable {
            tabs.push(tr("Variable").into());
        }
        w.tabs = Some((tabs, Rc::new(RefCell::new(0u8))));

        // 「文本修饰」行尾 `>` 展开的装饰线明细，与该行的 drill_in 共用同一标志。
        let deco_flag: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
        let mut settings: Vec<Setting> = Vec::new();

        // ── Basics：预览框（三页签共用，Variable 页不画）───────────────────────────────
        settings.push(Setting::Preview { text: tr("Preview").into() });
        settings.push(Setting::Divider { tab: 0 });
        settings.push(Setting::IconOptions {
            tab: 0,
            label: tr("Alignment").into(),
            active: match t.align_h {
                Some("left") => Some(0),
                Some("center") => Some(1),
                Some("right") => Some(2),
                Some("justified") => Some(3),
                _ => None,
            },
            bar_w: 96.0,
            drill_in: None,
            options: vec![
                Opt::new(tr("Align left"), "node.set", json!({ "props": { "textAlign": "left" } })).with_glyph(TypIcon::AlignLeft),
                Opt::new(tr("Align center"), "node.set", json!({ "props": { "textAlign": "center" } })).with_glyph(TypIcon::AlignCenter),
                Opt::new(tr("Align right"), "node.set", json!({ "props": { "textAlign": "right" } })).with_glyph(TypIcon::AlignRight),
                Opt::new(tr("Justify"), "node.set", json!({ "props": { "textAlign": "justified" } })).with_glyph(TypIcon::Justify),
            ],
        });
        settings.push(Setting::IconOptions {
            tab: 0,
            label: tr("Decoration").into(),
            active: match t.decoration {
                Some("none") => Some(0),
                Some("underline") => Some(1),
                Some("strikethrough") => Some(2),
                _ => None,
            },
            bar_w: 72.0,
            drill_in: Some(deco_flag.clone()),
            options: vec![
                Opt::new(tr("None"), "node.set", json!({ "props": { "textDecoration": "none" } })).with_glyph(TypIcon::None),
                Opt::new(tr("Underline"), "node.set", json!({ "props": { "textDecoration": "underline" } })).with_glyph(TypIcon::Underline),
                Opt::new(tr("Strikethrough"), "node.set", json!({ "props": { "textDecoration": "strikethrough" } })).with_glyph(TypIcon::Strike),
            ],
        });
        settings.push(Setting::IconOptions {
            tab: 0,
            label: tr("Letter case").into(),
            active: match t.case {
                Some("original") => Some(0),
                Some("upper") => Some(1),
                Some("lower") => Some(2),
                Some("title") => Some(3),
                Some("smallCaps") => Some(4),
                _ => None,
            },
            bar_w: 120.0,
            drill_in: None,
            options: vec![
                Opt::new(tr("As typed"), "node.set", json!({ "props": { "textCase": "original" } })).with_glyph(TypIcon::None),
                Opt::new(tr("Uppercase"), "node.set", json!({ "props": { "textCase": "upper" } })).with_glyph(TypIcon::Upper),
                Opt::new(tr("Lowercase"), "node.set", json!({ "props": { "textCase": "lower" } })).with_glyph(TypIcon::Lower),
                Opt::new(tr("Title case"), "node.set", json!({ "props": { "textCase": "title" } })).with_glyph(TypIcon::Title),
                Opt::new(tr("Small caps"), "node.set", json!({ "props": { "textCase": "smallCaps" } })).with_glyph(TypIcon::SmallCaps),
            ],
        });
        settings.push(Setting::Divider { tab: 0 });
        settings.push(Setting::IconOptions {
            tab: 0,
            label: tr("Vertical trim").into(),
            active: match t.leading_trim {
                Some("capHeight") => Some(1),
                Some("none") => Some(0),
                _ => None,
            },
            bar_w: 48.0,
            drill_in: None,
            options: vec![
                Opt::new(tr("Standard"), "node.set", json!({ "props": { "leadingTrim": "none" } })).with_glyph(TypIcon::TrimStandard),
                Opt::new(tr("Cap height"), "node.set", json!({ "props": { "leadingTrim": "capHeight" } })).with_glyph(TypIcon::TrimCap),
            ],
        });
        settings.push(Setting::IconOptions {
            tab: 0,
            label: tr("List style").into(),
            active: match t.list_style {
                Some("none") => Some(0),
                Some("bulleted") => Some(1),
                Some("numbered") => Some(2),
                _ => None,
            },
            bar_w: 72.0,
            drill_in: None,
            options: vec![
                Opt::new(tr("No list"), "node.set", json!({ "props": { "listStyle": "none" } })).with_glyph(TypIcon::None),
                Opt::new(tr("Bulleted list"), "node.set", json!({ "props": { "listStyle": "bulleted" } })).with_glyph(TypIcon::ListBullets),
                Opt::new(tr("Numbered list"), "node.set", json!({ "props": { "listStyle": "numbered" } })).with_glyph(TypIcon::ListNumbered),
            ],
        });
        let spacing = number_field(cx, 72.0, t.paragraph_spacing, "", 1.0, Slot::None);
        wire_set(&spacing, cx, &w.cmds, "paragraphSpacing", 0.0, 10_000.0);
        settings.push(Setting::Number { tab: 0, label: tr("Paragraph spacing").into(), field: spacing });
        settings.push(Setting::IconOptions {
            tab: 0,
            label: tr("Truncate text").into(),
            active: match t.auto_resize {
                Some("truncate") => Some(1),
                _ => None,
            },
            bar_w: 48.0,
            drill_in: None,
            options: vec![
                Opt::new(tr("Don't truncate"), "node.set", json!({ "props": { "textAutoResize": "none" } })).with_glyph(TypIcon::None),
                Opt::new(tr("Truncate with an ellipsis"), "node.set", json!({ "props": { "textAutoResize": "truncate" } }))
                    .with_glyph(TypIcon::Truncate),
            ],
        });
        settings.push(Setting::Dropdown {
            tab: 0,
            label: tr("Wrap style").into(),
            current: Rc::new(RefCell::new(
                match t.wrap_style {
                    Some("balance") => tr("Balance"),
                    Some("pretty") => tr("Pretty"),
                    _ => tr("Automatic"),
                }
                .into(),
            )),
            anchor: Rc::new(RefCell::new(None)),
            options: vec![
                Opt::new(tr("Automatic"), "node.set", json!({ "props": { "wrapStyle": "auto" } })),
                Opt::new(tr("Balance"), "node.set", json!({ "props": { "wrapStyle": "balance" } })),
                Opt::new(tr("Pretty"), "node.set", json!({ "props": { "wrapStyle": "pretty" } })),
            ],
        });

        // 「文本修饰 >」展开：装饰线样式 / 粗细 / 跳过下伸部（lizard `decoration_details`）。
        let thickness = number_field(cx, 72.0, t.decoration_thickness, "", 0.5, Slot::None);
        wire_set(&thickness, cx, &w.cmds, "textDecorationThickness", 0.0, 1_000.0);
        settings.push(Setting::ExpandedRows {
            tab: 0,
            flag: deco_flag.clone(),
            rows: vec![
                Setting::Caption { tab: 0, text: tr("Decoration style").into() },
                Setting::Options {
                    tab: 0,
                    label: SharedString::from(""),
                    active: match t.decoration_style {
                        Some("solid") => Some(0),
                        Some("dotted") => Some(1),
                        Some("wavy") => Some(2),
                        _ => None,
                    },
                    options: vec![
                        Opt::new(tr("Solid"), "node.set", json!({ "props": { "textDecorationStyle": "solid" } })),
                        Opt::new(tr("Dotted"), "node.set", json!({ "props": { "textDecorationStyle": "dotted" } })),
                        Opt::new(tr("Wavy"), "node.set", json!({ "props": { "textDecorationStyle": "wavy" } })),
                    ],
                },
                Setting::Number { tab: 0, label: tr("Thickness").into(), field: thickness },
                Setting::Toggle {
                    tab: 0,
                    label: tr("Skip ink").into(),
                    on: Rc::new(RefCell::new(t.decoration_skip_ink.unwrap_or(true))),
                    supported: true,
                    on_cmd: Opt::new(tr("On"), "node.set", json!({ "props": { "textDecorationSkipInk": true } })),
                    off_cmd: Opt::new(tr("Off"), "node.set", json!({ "props": { "textDecorationSkipInk": false } })),
                },
            ],
        });

        // ── Details：lizard `type_settings_details` 的六个分组 ────────────────────────
        settings.push(Setting::GroupTitle { tab: 1, text: tr("Indentation").into() });
        settings.push(Setting::Toggle {
            tab: 1,
            label: tr("Hanging punctuation").into(),
            on: Rc::new(RefCell::new(t.hanging_punctuation.unwrap_or(false))),
            supported: true,
            on_cmd: Opt::new(tr("On"), "node.set", json!({ "props": { "hangingPunctuation": true } })),
            off_cmd: Opt::new(tr("Off"), "node.set", json!({ "props": { "hangingPunctuation": false } })),
        });
        settings.push(Setting::Toggle {
            tab: 1,
            label: tr("Hanging list").into(),
            on: Rc::new(RefCell::new(t.hanging_list.unwrap_or(false))),
            supported: true,
            on_cmd: Opt::new(tr("On"), "node.set", json!({ "props": { "hangingList": true } })),
            off_cmd: Opt::new(tr("Off"), "node.set", json!({ "props": { "hangingList": false } })),
        });
        let indent = number_field(cx, 72.0, t.paragraph_indent, "", 1.0, Slot::None);
        wire_set(&indent, cx, &w.cmds, "paragraphIndent", 0.0, 10_000.0);
        settings.push(Setting::Number { tab: 1, label: tr("Paragraph indent").into(), field: indent });
        settings.push(Setting::Divider { tab: 1 });

        settings.push(Setting::GroupTitle { tab: 1, text: tr("Letter case").into() });
        settings.push(Setting::IconOptions {
            tab: 1,
            label: tr("Letter case").into(),
            active: match t.case {
                Some("original") => Some(0),
                Some("upper") => Some(1),
                Some("lower") => Some(2),
                Some("title") => Some(3),
                Some("smallCaps") => Some(4),
                _ => None,
            },
            bar_w: 120.0,
            drill_in: None,
            options: vec![
                Opt::new(tr("As typed"), "node.set", json!({ "props": { "textCase": "original" } })).with_glyph(TypIcon::None),
                Opt::new(tr("Uppercase"), "node.set", json!({ "props": { "textCase": "upper" } })).with_glyph(TypIcon::Upper),
                Opt::new(tr("Lowercase"), "node.set", json!({ "props": { "textCase": "lower" } })).with_glyph(TypIcon::Lower),
                Opt::new(tr("Title case"), "node.set", json!({ "props": { "textCase": "title" } })).with_glyph(TypIcon::Title),
                Opt::new(tr("Small caps"), "node.set", json!({ "props": { "textCase": "smallCaps" } })).with_glyph(TypIcon::SmallCaps),
            ],
        });
        // OpenType feature 分组完全由扫描结果 `features_supported` 驱动：字体没有的 feature
        // 根本不生成行（不是置灰）。本地只持有一份「分组依据」表，回答「扫描到这个 tag 时
        // 归到哪组 / 显示什么名字 / 默认开关」，不决定显示哪些。
        settings.push(Setting::Divider { tab: 1 });
        settings.extend(Self::type_feature_groups(1, &t.features, &t.features_supported));

        // ── Variable（只有可变字体才有这一页）──────────────────────────────────────────
        for axis in t.axes.iter() {
            let tag = axis.tag.clone();
            let cmds = w.cmds.clone();
            // 轴区间取字体 fvar 表的真实 min/max；退化区间（min >= max）用 0..1000 兜底。
            let (min, max) =
                if axis.min.is_finite() && axis.max.is_finite() && axis.max > axis.min { (axis.min as f32, axis.max as f32) } else { (0.0, 1000.0) };
            settings.push(Setting::Slider {
                tab: 2,
                label: axis.name.clone().into(),
                value: axis.value as f32,
                min,
                max,
                step: 1.0,
                suffix: "".into(),
                on_change: Rc::new(move |v, _, _| {
                    cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "variationSettings": { tag.clone(): f64::from(v) } } })) });
                }),
            });
        }
        if has_variable {
            settings.push(Setting::Options {
                tab: 2,
                label: tr("Variations").into(),
                active: Some(0),
                options: vec![Opt::new(tr("Reset to style"), "node.set", json!({ "props": { "variationSettings": Value::Null } }))],
            });
        }
        w.settings = Rc::new(RefCell::new(settings));
        w
    }

    // OpenType feature 面板是**数据驱动**的：显示哪些行完全由字体的扫描结果
    // `features_supported`（`face.feature_tags()`）决定，字体没有的 feature 根本不出现。
    // 本地只持有一份「分组依据」表：把扫描到的 tag 归到 lizard 的分组、给显示名和默认开关。
    // 这些 tag -> 组 / 显示名 / 默认值是**本地知识**（分组依据），不是显示清单。
    //
    // 命令增量写 `text.setFeatures {features:{tag:bool}}`，不会互相覆盖整张 features 表
    // （与 node.set 的清空重建语义不同）。开关的 `on` 优先取文档显式覆盖，否则用默认值；
    // 既然行是从扫描结果生成的，它就一定被字体支持，`supported` 恒真、永不变灰。
    const FEATURE_CATALOG: &'static [(&'static str, &'static str, &'static str, bool)] = &[
        //  tag     显示名                        归属分组 (tr key)      默认
        ("case", "Case-sensitive forms", "Letter case|dialog", false),
        ("cpsp", "Capital spacing", "Letter case|dialog", false),
        ("tnum", "Tabular numbers", "Numbers", false),
        ("frac", "Fractions", "Numbers", false),
        ("zero", "Slashed zero", "Numbers", false),
        ("liga", "Standard ligatures", "Letter forms", true),
        ("dlig", "Discretionary ligatures", "Letter forms", false),
        ("calt", "Contextual alternates", "Letter forms", true),
        ("ordn", "Ordinals", "Letter forms", false),
        ("smcp", "Small capitals", "Letter forms", false),
        ("ss01", "Stylistic set 1", "Stylistic sets", false),
        ("ss02", "Stylistic set 2", "Stylistic sets", false),
        ("ss03", "Stylistic set 3", "Stylistic sets", false),
        ("ss04", "Stylistic set 4", "Stylistic sets", false),
        ("ss05", "Stylistic set 5", "Stylistic sets", false),
        ("kern", "Kerning pairs", "Horizontal spacing", true),
    ];
    /// 分组的显示顺序（与 lizard `type_settings_details` 一致）；空组不显示。
    const FEATURE_GROUP_ORDER: &'static [&'static str] =
        &["Letter case|dialog", "Numbers", "Letter forms", "Stylistic sets", "Horizontal spacing", "Other features"];

    fn type_feature_groups(
        tab: u8,
        features: &std::collections::BTreeMap<String, bool>,
        features_supported: &std::collections::BTreeSet<String>,
    ) -> Vec<Setting> {
        use std::collections::BTreeMap;
        // ① 把扫描到的每个 tag 归进组（本地表只提供归属 / 显示名 / 默认值）。
        //    显示清单 = features_supported ∩ 表；表外但扫描到的 tag 归「Other features」。
        let mut grouped: BTreeMap<&'static str, Vec<Setting>> = BTreeMap::new();
        let mut others: Vec<Setting> = Vec::new();
        for tag in features_supported.iter() {
            let known = Self::FEATURE_CATALOG.iter().find(|(t, _, _, _)| *t == tag.as_str());
            let (label, group, default_on): (SharedString, &'static str, bool) = match known {
                Some((_, name, grp, def)) => (tr(*name).into(), *grp, *def),
                // 字体有、但本地没有可信中文名的 feature：用 tag 本身，归「其它」。
                None => (SharedString::from(tag.clone()), "Other features", false),
            };
            let on = features.get(tag).copied().unwrap_or(default_on);
            let key = tag.clone();
            let row = Setting::Toggle {
                tab,
                label,
                on: Rc::new(RefCell::new(on)),
                supported: true, // 来自扫描结果，必然支持
                on_cmd: Opt::new(tr("On"), "text.setFeatures", json!({ "features": { (key.clone()): true } })),
                off_cmd: Opt::new(tr("Off"), "text.setFeatures", json!({ "features": { (key): false } })),
            };
            if known.is_some() {
                grouped.entry(group).or_default().push(row);
            } else {
                others.push(row);
            }
        }
        if !others.is_empty() {
            grouped.insert("Other features", others);
        }
        // ② 按固定组序输出；组里没有任何行（字体不支持该组所有 feature）就整组不出现。
        let mut out = Vec::new();
        for group in Self::FEATURE_GROUP_ORDER {
            let Some(rows) = grouped.remove(group) else { continue };
            if rows.is_empty() {
                continue;
            }
            out.push(Setting::GroupTitle { tab, text: tr(*group).into() });
            out.extend(rows);
            out.push(Setting::Divider { tab });
        }
        out
    }
    // ── ⑥ 变量编辑器右侧列表（egui `dialogs/variables_panel.rs::table()`）─────────────────
    // 只搬右边的表格：左侧集合/分组 sidebar、顶部搜索、编辑与拖拽都还没做。
    // 标题用集合名（egui 的浮动面板标题），右侧放变量总数的计数 chip。
    pub(crate) fn variables_table(cmds: Cmds, anchor: Point<Pixels>, data: VariablesData, popups: Option<crate::panels::PopupReqs>) -> Self {
        let mut w = Self::base(WindowKind::VariablesTable, tr("Variables"), cmds, Open::Anchor(anchor));
        w.width = variables::WINDOW_W;
        w.height = variables::WINDOW_H;
        // 表格自己控制左右 inset，卡片 body 不再加通用 16 pt 内边距。
        w.pad = 0.0;
        w.variables_data = Some(data);
        w.var_popups = popups;
        w
    }

    /// 效果设置小窗：点效果行的方框弹出。egui `effects.rs::effect_settings` 是**向下 inline
    /// 展开**设置项，这里按用户 2026-11-13 的要求把设置项装进 detached 小窗。
    ///
    /// 类型切换也搬进小窗顶部，且用 `Effect::of_type` 生成**全新默认对象**。原实现是 clone
    /// 旧 effect JSON 只改 `type`：旧类型的字段（offset / spread / size / density …）会残留，
    /// 被引擎 `refuse_ignored_effect_keys` 判为 unknown properties 后整条 `effect.set` 拒收，
    /// 命令静默失效 —— 这就是「有一些效果点了、选中图层在画布中没有效果」的根因。
    /// `effect.add` 走 `Effect::of_type`，所以从 `+` 菜单添加一直是正常的。
    pub(crate) fn effect_settings(cmds: Cmds, cx: &mut App, anchor: Point<Pixels>, index: usize, label: SharedString, effect: Value) -> Self {
        let mut w = Self::base(WindowKind::EffectSettings, label.clone(), cmds.clone(), Open::Anchor(anchor));
        w.width = 248.0;
        w.height = 320.0;
        w.pad = 12.0;
        let ty = effect.get("type").and_then(Value::as_str).unwrap_or("dropShadow").to_string();
        let mut settings: Vec<Setting> = Vec::new();
        // ① 类型切换：每项都带一份干净的、只属于该类型的默认 effect。
        let options: Vec<Opt> = figcraft_doc::Effect::TYPES
            .iter()
            .map(|(l, t)| {
                let clean = figcraft_doc::Effect::of_type(*t).and_then(|e| serde_json::to_value(e).ok()).unwrap_or(Value::Null);
                Opt::new(tr(*l), "effect.set", json!({ "index": index, "effect": clean }))
            })
            .collect();
        let current: SharedString =
            figcraft_doc::Effect::TYPES.iter().find(|(_, t)| *t == ty).map(|(l, _)| tr(*l).into()).unwrap_or_else(|| label.clone());
        settings.push(Setting::Dropdown {
            tab: 0,
            label: tr("Type").into(),
            current: Rc::new(RefCell::new(current)),
            anchor: Rc::new(RefCell::new(None)),
            options,
        });
        // ② 该效果的具体设置项（egui effect_settings 的数值 / 开关 / 枚举）。
        // 全部控件共用这一份可变 effect：任何一个写回后，其余控件的命令都从最新值起改，
        // 否则「先改 X 再改 Y」时 Y 的 effect.set 会带着开窗时的旧 X 把 X 冲掉。
        let shared = Rc::new(RefCell::new(effect.clone()));
        settings.extend(effect_setting_rows(cx, &cmds, index, &shared, &ty));
        w.settings = Rc::new(RefCell::new(settings));
        w
    }

    /// 导出高级选项弹窗（egui `export_panel::advanced_options`）：正文是一组通用 `Setting`
    /// 行 —— 勾选项用 Toggle，VD_COMPAT / Color profile 两组用 Radio。egui 原本是下拉菜单，
    /// 但单选器在下拉里不好放，用户要求改用通用弹窗承载；行的编号与文案 1:1 照抄 egui。
    pub(crate) fn export_options(cmds: Cmds, title: impl Into<SharedString>, anchor: Point<Pixels>, settings: Vec<Setting>) -> Self {
        let mut w = Self::base(WindowKind::ExportSettings, title, cmds, Open::Anchor(anchor));
        w.width = 240.0;
        w.pad = 12.0;
        w.height = HEADER_H + (settings.len() as f32) * 28.0 + 24.0;
        w.settings = Rc::new(RefCell::new(settings));
        w
    }

    /// 根视图注入 Floating/Full 切换与关闭回调（编辑器尚未创建时先存着）。
    pub(crate) fn set_variables_callbacks(&mut self, on_full: Rc<dyn Fn(bool, &mut App)>, on_close: Rc<dyn Fn(&mut App)>) {
        self.var_on_full = on_full;
        self.var_on_close = on_close;
        if let Some(e) = &mut self.editor {
            e.set_callbacks(self.var_on_full.clone(), self.var_on_close.clone());
        }
    }

    fn total_variables(&self) -> usize {
        if let Some(e) = &self.editor {
            return e.total();
        }
        self.variables_data.as_ref().and_then(|d| d.collections.first()).map(|c| c.rows.len()).unwrap_or(0)
    }

    /// 变量编辑器是否处于全屏呈现（左栏选中态、卡片几何都看它）。
    pub(crate) fn variables_full(&self) -> bool {
        // `render()` 在 `body_element` 之前就要决定几何，首帧 editor 还没建，必须读显式状态。
        self.editor_full.unwrap_or_else(|| self.editor.as_ref().is_some_and(|e| e.is_full()))
    }

    /// 由根视图把「记住的 Full/Floating」写回。editor 可能尚未创建，故先存字段，
    /// `body_element` 建好后再灌进去（`variables_full()` 也以这个字段为准）。
    pub(crate) fn set_variables_full(&mut self, full: bool) {
        self.editor_full = Some(full);
        if let Some(e) = &self.editor {
            e.set_full(full);
        }
    }

    /// 变量窗口是否有拖拽正在进行（整卡移动 / 8 个 resize handle）。
    pub(crate) fn variables_active(&self) -> bool {
        self.hold.busy()
            || self.editor.as_ref().is_some_and(|e| e.resizing())
            // 编辑器内部的拖拽（行排序 / 列宽 / 侧栏 / 分栏 / 模式列 / 横向滚动条）同样要由
            // **根视图**接管：GPUI 元素级的 `on_mouse_move` 只在指针悬停该元素时触发，
            // 指针一移出卡片拖拽就断（用户 2026-11-07 明确指出这个监听方式不对）。
            || self.editor.as_ref().is_some_and(|e| e.internal_drag())
    }

    /// resize handle 优先：按下落在边缘时，不让整卡拖动抢走这次手势。
    pub(crate) fn variables_resizing(&self) -> bool {
        self.editor.as_ref().is_some_and(|e| e.resizing())
    }

    /// 打开「Corner smoothing」小窗：一条 0..100% 的通用滑条，拖动写回 `node.set`。
    ///
    /// 与 Layout / 描边 / 效果设置共用同一张 `WindowState` 通用弹窗卡（header + 关闭 + 拖动），
    /// 只是正文换成滑条，没有另造一套窗口框架。
    pub(crate) fn corner_smoothing(cmds: Cmds, value: f32, anchor: Point<Pixels>) -> Self {
        let mut w = Self::base(WindowKind::CornerSmoothing, tr("Corner smoothing"), cmds, Open::Anchor(anchor));
        w.width = 220.0;
        w.pad = 12.0;
        // 40 头部 + 24 滑条 + 上下各 12 内边距（真实高度由 `on_measure` 回写）。
        w.height = HEADER_H + 24.0 + 24.0;
        w.smoothing = Some(Rc::new(Cell::new(value.clamp(0.0, 1.0))));
        w
    }

    /// 字体选择窗：列表每项显示字体名，并用该字体自身渲染（名字即预览）。
    pub(crate) fn font_picker(cmds: Cmds, cx: &mut App, current: Option<&str>, anchor: Point<Pixels>) -> Self {
        let mut w = Self::base(WindowKind::FontPicker, tr("Font family"), cmds, Open::Anchor(anchor));
        // Figma `typePanel.fontFamily.fontPickerWindow`：宽 `15rem`(=240)，窗口背景是
        // `var(--color-bg)`（**面板色**，不是菜单色），body 内联高度 429px、内边距 0。
        w.surface = Surface::Panel;
        w.width = 240.0;
        // `height` 在 `Popover` 里是**整卡**高度（`open_pos` / `clamp` 用它算触底上翻），
        // 所以要含 40pt 头部：body 429 + header 40。否则上翻会少算 40pt、卡片底仍飞出屏幕。
        w.height = 429.0 + crate::widgets::popover::HEADER_H;
        w.pad = 0.0;
        w.rows = figcraft_text::family_names()
            .into_iter()
            .map(|fam| {
                let params = json!({ "props": { "fontFamily": fam.clone() } });
                let mut row = Row::new(fam.clone(), "node.set", params).font_family(fam.clone());
                if Some(fam.as_str()) == current {
                    row = row.trail("✓");
                }
                row
            })
            .collect();
        w.add_search(cx, "Search fonts");
        w
    }

    /// 正文：标题在通用窗口头部，这里只有一条滑条（右侧百分比数值由 `Slider` 自己画）。
    fn corner_smoothing_body(&self) -> AnyElement {
        let value = self.smoothing.as_ref().map(|c| c.get()).unwrap_or(0.0);
        let cell = self.smoothing.clone();
        let cmds = self.cmds.clone();
        let slider = Slider::new(value * 100.0, 0.0, 100.0).step(1.0).show_value().suffix("%").precision(0).on_change(move |pct, _w, _cx| {
            let v = (pct / 100.0).clamp(0.0, 1.0);
            if let Some(c) = &cell {
                c.set(v);
            }
            cmds.push(Cmd { id: "node.set", params: Some(json!({ "props": { "cornerSmoothing": v } })) });
        });
        popover_body().child(slider.render()).into_any_element()
    }

    /// 由根视图用**窗口坐标**推进变量窗口的所有拖拽：resize 改 `size`，整卡移动改 `place`。
    /// 返回是否真的变了（调用方据此 `cx.notify()`，配合节流避免每帧整树重绘导致拖动发涩）。
    ///
    /// resize 优先且独占：一旦某个边缘/角 handle 按下（`editor.resizing()`），本次手势只改尺寸，
    /// 绝不启动整卡移动——否则按下右下角会被同时解读成「移动 + 缩放」，窗口乱跳。
    pub(crate) fn variables_route_move(&mut self, at: Point<Pixels>) -> bool {
        if self.editor.as_ref().is_some_and(|e| e.resizing()) {
            if let Some(e) = &self.editor {
                e.route_move(at);
            }
            return true;
        }
        // 内置拖拽（行 / 列宽 / 侧栏 / 分栏 / 模式列宽 / 横向滚动条）进行中：整卡不要动，
        // 但**必须由根视图把移动转给编辑器的 `route_move`**——否则指针一离开卡片，
        // 编辑器自己的 `on_mouse_move` 就不再触发，拖拽会当场冻结。
        if self.editor.as_ref().is_some_and(|e| e.internal_drag()) {
            if let Some(e) = &self.editor {
                e.route_move(at);
            }
            return true;
        }
        // 整卡移动：3pt 死区 / latch / 相对位移 / 收尾全在通用 `hold_drag` 里
        // （egui `click_and_drag`）；这里只把算出的绝对位置写回 `place`。
        self.move_drag(at)
    }

    /// 抬起 / Escape：结束整卡拖动与 resize。
    pub(crate) fn variables_end_drag(&mut self) {
        self.end_drag();
        if let Some(e) = &self.editor {
            e.end_resize();
        }
    }

    /// 重新打开变量编辑器时刷新文档快照，但保留交互状态（选中 / 分组折叠 / 窗口位置）。
    pub(crate) fn set_variables_data(&mut self, data: VariablesData) {
        if let Some(e) = &mut self.editor {
            e.set_data(data);
        } else {
            self.variables_data = Some(data);
        }
    }

    /// 根视图的 `on_key_down` 转发给变量编辑器（Escape / Ctrl-Cmd+C/V）。
    pub(crate) fn variables_key(&self, key: &str, accel: bool) -> bool {
        if self.kind != WindowKind::VariablesTable {
            return false;
        }
        self.editor.as_ref().is_some_and(|e| e.key(key, accel))
    }

    // ── ⑥ 样式选择弹窗（egui `widgets/style_picker.rs`：Color/Effect/Grid/Text 共用一类窗口）──
    /// `ty` 是 slot 类型（`text`/`paint`/`effect`/`grid`），只影响标题栏「新建样式」推的
    /// `style.create {type}` 与默认名；四类窗口的正文（搜索 + 列表 + 筛选）完全一致。
    pub(crate) fn styles(cmds: Cmds, cx: &mut App, anchor: Point<Pixels>, title: impl Into<SharedString>, ty: &'static str, rows: Vec<Row>) -> Self {
        let mut w = Self::base(WindowKind::TextStyles, title, cmds, Open::Anchor(anchor));
        w.surface = Surface::Menu;
        w.pad = 8.0;
        w.height = 320.0;
        w.rows = rows;
        w.style_create_ty = ty;
        w.add_search(cx, "Search styles");
        w
    }

    // ── ⑦ 本地样式编辑器（lizard `style_picker` 的 4 个全尺寸编辑器）──────────────────────
    /// 不另写窗口：复用 `WindowState` 通用弹窗卡（标题栏 / 拖动 / clamp）。正文按 lizard
    /// `style_metadata` + Properties + 属性行的精确 add_space 排布，用 `Setting` 行实现。
    /// 写回走引擎 `style.update`；删除不在弹窗里（右栏样式行右键菜单 / Del 键），与
    /// lizard 4 个全尺寸编辑器一致。
    pub(crate) fn style_editor(
        cmds: Cmds,
        cx: &mut App,
        anchor: Point<Pixels>,
        title: SharedString,
        ty: &'static str,
        id: String,
        name: String,
        description: String,
        properties: Vec<String>,
        paints: Vec<figcraft_color::Paint>,
        text: Option<serde_json::Value>,
        effects: Option<Vec<EffectRow>>,
        grids: Option<Vec<GridRow>>,
        popups: crate::panels::PopupReqs,
    ) -> Self {
        // lizard `FloatingPanelWidth::Regular` = 304 且 `inner_margin(Margin::ZERO)`：
        // 面板本身没有内边距，源码里所有 `x + 16` 都是每一行自己加的左缩进。以前设成
        // width 336 / pad 16，外层 16 与行内 pl(16) 叠加成 32，整张卡片左右都虚胖一圈。
        let mut w = Self::base(WindowKind::StyleEditor, title, cmds.clone(), Open::Anchor(anchor));
        w.width = 304.0;
        w.pad = 0.0;
        // 卡片高度由内容撑开，这里的值只参与锚点抬升 / 边界 clamp 的估算，必须贴近真实高度，
        // 写死 640 会把弹窗整体顶到画布上方。按 lizard 各段精确高度累计（含 40 头部）：
        //   preview 120/152 + 16 + Name 24 + 8 + Desc 24 + 20|24 + 标题 24|32 + 10|8
        //   + 属性区（行 ×32，text 是 3×24 + 2×8）+ 8 + Delete 24
        let rows = match ty {
            "paint" => paints.len().max(1) as f32 * 32.0,
            "text" => 3.0 * 24.0 + 2.0 * 8.0,
            "effect" => effects.as_ref().map(|e| e.len().max(1) as f32 * 32.0).unwrap_or(32.0),
            _ => grids.as_ref().map(|g| g.len().max(1) as f32 * 32.0).unwrap_or(32.0),
        };
        let preview_h = if ty == "effect" || ty == "grid" { 152.0 } else { 120.0 };
        let meta_gap = if ty == "text" { 20.0 } else { 24.0 };
        let title_h = if ty == "text" { 24.0 } else { 32.0 };
        let after_title = if ty == "text" { 10.0 } else { 8.0 };
        // 末尾两段：属性行后的 add_space(8)（text 是 16）+ 卡片底部留白 8；再 +56 让
        // `open_pos` 的抬升把卡片整体提到浮动工具栏之上（工具栏浮在画布底部约 56pt）。
        // 底部 Delete 按钮已按 lizard 去掉，所以这里不再有 +24 的按钮行。
        let tail = if ty == "text" { 16.0 } else { 8.0 };
        w.height = 40.0 + preview_h + 16.0 + 24.0 + 8.0 + 24.0 + meta_gap + title_h + after_title + rows + tail + 8.0 + 56.0;
        // 表面色不指定：`Setting` 行 / `TextField` 的墨色与字段底色都按默认 `Surface::Panel`
        // 调好，调用方指定 surface 会把整套配色打乱。

        let settings = Self::style_editor_settings(
            cx,
            ty,
            id.clone(),
            name.clone(),
            description.clone(),
            paints.clone(),
            text.clone(),
            effects.clone(),
            grids.clone(),
            cmds.clone(),
        );
        w.style_edit = Some(StyleEditState { id, has_selection: false, paints, text, effects, grids, popups, picker: Rc::new(RefCell::new(None)) });
        w.settings = Rc::new(RefCell::new(settings));
        w
    }

    /// 构造样式编辑器的正文 `settings`（Name / Description / Properties 各属性行）。
    /// 打开弹窗与命令回写文档后刷新共用同一份逻辑，保证界面与文档一致。
    fn style_editor_settings(
        cx: &mut App,
        ty: &'static str,
        id: String,
        name: String,
        description: String,
        paints: Vec<figcraft_color::Paint>,
        text: Option<serde_json::Value>,
        effects: Option<Vec<EffectRow>>,
        grids: Option<Vec<GridRow>>,
        cmds: Cmds,
    ) -> Vec<Setting> {
        let name_cmds = cmds.clone();
        let name_id = id.clone();
        let name_field = TextField::build(cx, 176.0, move |tf, _| {
            tf.set_value(name);
            tf.set_on_commit(move |text, _w, _cx| {
                let text = text.trim();
                if !text.is_empty() {
                    name_cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": name_id, "name": text })) });
                }
            });
        });
        let desc_cmds = cmds.clone();
        let desc_id = id.clone();
        let desc_field = TextField::build(cx, 176.0, move |tf, _| {
            tf.set_value(description);
            tf.set_on_commit(move |text, _w, _cx| {
                desc_cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": desc_id, "description": text })) });
            });
        });

        let mut settings: Vec<Setting> = Vec::new();
        // ① 预览区：color 304×120（内缩 16 后 272×104 **整块**色）；text 304×120 字体采样；
        //    effect / grid 304×152 棋盘底预览。
        match ty {
            "paint" => {
                if let Some(p) = paints.last() {
                    settings.push(Setting::PaintPreview { paint: p.clone() });
                }
            }
            "text" => {
                if let Some(t) = text.clone() {
                    settings.push(Setting::TextPreview { text: t });
                }
            }
            "effect" => {
                if let Some(e) = effects.clone() {
                    let arr: Vec<Value> = e.iter().map(|x| x.effect.clone()).collect();
                    settings.push(Setting::EffectPreview { effects: Value::Array(arr) });
                }
            }
            _ => {
                if let Some(g) = grids.clone()
                    && let Some(first) = g.first()
                {
                    settings.push(Setting::GridPreview { grids: first.grids.clone() });
                }
            }
        }
        // ② `style_metadata`：预览后 add_space(16)，Name 行 304×24，add_space(8)，Description 行。
        settings.push(Setting::Spacer { tab: 0, h: 16.0 });
        settings.push(Setting::Field { tab: 0, label: tr("Name").into(), field: name_field });
        settings.push(Setting::Spacer { tab: 0, h: 8.0 });
        settings.push(Setting::Field { tab: 0, label: tr("Description").into(), field: desc_field });
        // ③ Properties：color/effect/grid 是 add_space(24) + 304×32 标题行（行尾「+」）；
        //    text 是 add_space(20) + 304×24 标题行（无「+」）。
        settings.push(Setting::Spacer { tab: 0, h: if ty == "text" { 20.0 } else { 24.0 } });
        let mut section = Setting::SectionTitle {
            tab: 0,
            text: tr("Properties").into(),
            h: if ty == "text" { 24.0 } else { 32.0 },
            style_id: id.clone(),
            array: Value::Array(vec![]),
            menu: vec![],
            anchor: Rc::new(RefCell::new(None)),
        };
        match ty {
            "paint" => {
                let paints_json = serde_json::to_value(&paints).unwrap_or(Value::Array(vec![]));
                if let Setting::SectionTitle { array, menu, .. } = &mut section {
                    *array = paints_json.clone();
                    // lizard：Add fill 点击直接追加一个默认灰填充，没有类型菜单。
                    *menu = vec![(
                        SharedString::default(),
                        serde_json::to_value(figcraft_color::Paint::solid(figcraft_color::Color::from_rgba8(0xD9, 0xD9, 0xD9, 255)))
                            .unwrap_or(Value::Null),
                    )];
                }
                settings.push(section);
                settings.push(Setting::Spacer { tab: 0, h: 8.0 });
                for (i, p) in paints.iter().enumerate().rev() {
                    let hex = p.solid_color().map(|c| c.to_hex()).unwrap_or_default();
                    let pct = (p.opacity * 100.0).round().clamp(0.0, 100.0) as i64;
                    // 与右栏 Fill 行共用通用控件 `color_row_editors`：编辑器构建、数值行为、
                    // 「%」拖动、范围夹紧都只在 `widgets/color_field.rs` 一处实现；这里只给
                    // 写回 `style.update` 的回调，不再自己拼一份 `TextEdit`。
                    let (hex_ed, op_ed) = crate::widgets::ColorField::color_row_editors(
                        cx,
                        &hex,
                        pct,
                        {
                            let all = paints.clone();
                            let sid = id.clone();
                            let write = cmds.clone();
                            move |text: &str, _, _| {
                                let Some(c) = figcraft_color::Color::from_hex(text.trim()) else { return };
                                let mut ps = all.clone();
                                if let Some(p) = ps.get_mut(i) {
                                    if let figcraft_color::PaintKind::Solid { color } = &mut p.kind {
                                        *color = c.with_alpha(color.a);
                                    }
                                }
                                write.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": ps })) });
                            }
                        },
                        {
                            let all = paints.clone();
                            let sid = id.clone();
                            let write = cmds.clone();
                            move |v: f64, _, _| {
                                let mut ps = all.clone();
                                if let Some(p) = ps.get_mut(i) {
                                    p.opacity = (v / 100.0).clamp(0.0, 1.0) as f32;
                                }
                                write.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": ps })) });
                            }
                        },
                    );
                    settings.push(Setting::PaintRow { tab: 0, style_id: id.clone(), index: i, paint: p.clone(), hex_ed, op_ed });
                }
            }
            "text" => {
                settings.push(section);
                settings.push(Setting::Spacer { tab: 0, h: 10.0 });
                // lizard `text_style_editor`：Properties 下三行紧凑控件，无左侧标签。
                let tid = id.clone();
                let text_state = Rc::new(RefCell::new(text.clone().unwrap_or(Value::Object(Default::default()))));
                let tf = text.clone().unwrap_or(Value::Null);
                let family = Self::json_str_opt(&tf, &["fontFamily", "font_family"]).unwrap_or_default();
                let style_name = Self::json_str_opt(&tf, &["fontStyle", "font_style"]).unwrap_or_default();
                let families: Vec<SharedString> = figcraft_text::family_names().into_iter().map(SharedString::from).collect();
                let font_styles: Vec<SharedString> = figcraft_text::family_styles(&family).into_iter().map(SharedString::from).collect();
                let size_field = Self::text_number(
                    cx,
                    Self::json_num(&tf, &["fontSize", "font_size"]).unwrap_or(12.0),
                    "font_size",
                    tid.clone(),
                    text_state.clone(),
                    cmds.clone(),
                );
                let line_field = Self::text_number_auto(
                    cx,
                    Self::json_num(&tf, &["lineHeight", "line_height"]),
                    "line_height",
                    tid.clone(),
                    text_state.clone(),
                    cmds.clone(),
                );
                let spacing_field = Self::text_number_auto(
                    cx,
                    Self::json_num(&tf, &["letterSpacing", "letter_spacing"]),
                    "letter_spacing",
                    tid.clone(),
                    text_state.clone(),
                    cmds.clone(),
                );
                settings.push(Setting::TextControls {
                    tab: 0,
                    family: Rc::new(RefCell::new(SharedString::from(family))),
                    families,
                    font_style: Rc::new(RefCell::new(SharedString::from(style_name))),
                    font_styles,
                    size_field,
                    line_field,
                    spacing_field,
                    style_id: tid,
                    text: text_state,
                    family_anchor: Rc::new(RefCell::new(None)),
                    style_anchor: Rc::new(RefCell::new(None)),
                });
            }
            "effect" => {
                if let Some(list) = effects.clone() {
                    let effects_json = Value::Array(list.iter().map(|x| x.effect.clone()).collect());
                    if let Setting::SectionTitle { array, menu, .. } = &mut section {
                        *array = effects_json.clone();
                        *menu = figcraft_doc::Effect::TYPES
                            .iter()
                            .filter_map(|(label, kind)| {
                                figcraft_doc::Effect::of_type(kind).and_then(|e| serde_json::to_value(e).ok()).map(|v| (tr(label).into(), v))
                            })
                            .collect();
                    }
                    settings.push(section);
                    settings.push(Setting::Spacer { tab: 0, h: 8.0 });
                    for (i, e) in list.iter().enumerate().rev() {
                        settings.push(Setting::EffectRow {
                            tab: 0,
                            style_id: id.clone(),
                            index: i,
                            label: SharedString::from(e.label.clone()),
                            ty: e.ty,
                            visible: e.visible,
                            effect: e.effect.clone(),
                            array: effects_json.clone(),
                            anchor: Rc::new(RefCell::new(None)),
                        });
                    }
                }
            }
            _ => {
                if let Some(list) = grids.clone() {
                    let grids_json = list.first().map(|g| g.grids.clone()).unwrap_or(Value::Array(vec![]));
                    if let Setting::SectionTitle { array, menu, .. } = &mut section {
                        *array = grids_json.clone();
                        *menu = [
                            (tr("Grid"), figcraft_doc::LayoutGrid::default()),
                            (tr("Columns"), figcraft_doc::LayoutGrid::columns()),
                            (tr("Rows"), figcraft_doc::LayoutGrid::rows()),
                        ]
                        .into_iter()
                        .filter_map(|(label, g)| serde_json::to_value(g).ok().map(|v| (label.into(), v)))
                        .collect();
                    }
                    settings.push(section);
                    settings.push(Setting::Spacer { tab: 0, h: 8.0 });
                    for (i, g) in list.iter().enumerate().rev() {
                        settings.push(Setting::GridRow {
                            tab: 0,
                            style_id: id.clone(),
                            index: i,
                            label: SharedString::from(g.label.clone()),
                            visible: g.visible,
                            grids: grids_json.clone(),
                            anchor: Rc::new(RefCell::new(None)),
                        });
                    }
                }
            }
        }
        // ④ 底部：4 个全尺寸编辑器在属性之后只有一段 add_space，**没有**底部 Delete 按钮
        // （删除走右栏样式行的右键菜单 / Del 键；text 编辑器的删除在标题栏）。
        // lizard：color/effect/grid 是 `add_space(8)`，text 的 `ui.scope` 之后是 `add_space(16)`。
        settings.push(Setting::Spacer { tab: 0, h: if ty == "text" { 16.0 } else { 8.0 } });
        settings
    }

    /// 样式编辑弹窗刷新用的文档快照：命令回写文档后，据此重建正文 `settings`。
    fn style_snapshot(st: &figcraft_doc::Style) -> StyleSnap {
        let ty = st.kind.type_name();
        let (paints, text, effects, grids) = match &st.kind {
            figcraft_doc::StyleKind::Paint { paints } => (paints.clone(), None, None, None),
            figcraft_doc::StyleKind::Text { style, .. } => (Vec::new(), serde_json::to_value(style).ok(), None, None),
            figcraft_doc::StyleKind::Effect { effects } => {
                let list = effects.clone();
                (
                    Vec::new(),
                    None,
                    Some(
                        list.iter()
                            .enumerate()
                            .map(|(index, e)| EffectRow {
                                index,
                                label: e.label().to_string(),
                                ty: match e.kind {
                                    figcraft_doc::EffectKind::DropShadow(_) => "dropShadow",
                                    figcraft_doc::EffectKind::InnerShadow(_) => "innerShadow",
                                    figcraft_doc::EffectKind::LayerBlur(_) => "layerBlur",
                                    figcraft_doc::EffectKind::BackgroundBlur(_) => "backgroundBlur",
                                    figcraft_doc::EffectKind::Noise(_) => "noise",
                                    figcraft_doc::EffectKind::Texture(_) => "texture",
                                    figcraft_doc::EffectKind::Glass(_) => "glass",
                                },
                                visible: e.visible,
                                effect: serde_json::to_value(e).unwrap_or(serde_json::Value::Null),
                            })
                            .collect(),
                    ),
                    None,
                )
            }
            figcraft_doc::StyleKind::Grid { grids } => {
                let gl = grids.clone();
                (
                    Vec::new(),
                    None,
                    None,
                    Some(
                        gl.iter()
                            .enumerate()
                            .map(|(index, g)| GridRow {
                                index,
                                label: crate::panels::right::data::grid_label(g),
                                color: g.color.to_hex(),
                                visible: g.visible,
                                grids: serde_json::to_value(&gl).unwrap_or(serde_json::Value::Array(vec![])),
                            })
                            .collect(),
                    ),
                )
            }
        };
        StyleSnap { ty, name: st.name.clone(), description: st.description.clone(), paints, text, effects, grids }
    }

    /// 卡片高度估算（与 `style_editor` 打开时一致）：命令增删属性行后重算，避免卡片定位偏移。
    fn style_editor_height(
        ty: &'static str,
        paints: &[figcraft_color::Paint],
        _text: &Option<serde_json::Value>,
        effects: &Option<Vec<EffectRow>>,
        grids: &Option<Vec<GridRow>>,
    ) -> f32 {
        let rows = match ty {
            "paint" => paints.len().max(1) as f32 * 32.0,
            "text" => 3.0 * 24.0 + 2.0 * 8.0,
            "effect" => effects.as_ref().map(|e| e.len().max(1) as f32 * 32.0).unwrap_or(32.0),
            _ => grids.as_ref().map(|g| g.len().max(1) as f32 * 32.0).unwrap_or(32.0),
        };
        let preview_h = if ty == "effect" || ty == "grid" { 152.0 } else { 120.0 };
        let meta_gap = if ty == "text" { 20.0 } else { 24.0 };
        let title_h = if ty == "text" { 24.0 } else { 32.0 };
        let after_title = if ty == "text" { 10.0 } else { 8.0 };
        let tail = if ty == "text" { 16.0 } else { 8.0 };
        40.0 + preview_h + 16.0 + 24.0 + 8.0 + 24.0 + meta_gap + title_h + after_title + rows + tail + 8.0 + 56.0
    }

    /// 命令回写文档后，用新快照原地重建正文 `settings`（保留位置 / 拖动状态），让界面即时反映。
    pub(crate) fn apply_style_snapshot(&mut self, cx: &mut App, snap: StyleSnap) {
        let StyleSnap { ty, name, description, paints, text, effects, grids } = snap;
        let id = self.style_edit.as_ref().map(|e| e.id.clone()).unwrap_or_default();
        let mut settings = Self::style_editor_settings(
            cx,
            ty,
            id.clone(),
            name,
            description,
            paints.clone(),
            text.clone(),
            effects.clone(),
            grids.clone(),
            self.cmds.clone(),
        );
        // 拖动「%」时每一步都会写回文档并触发本刷新。若整份 settings 连同编辑器一起重建，
        // 正在 scrub 的 Entity 会被换掉，拖动就断了（用户 2026-11-15）。按 (tab, index)
        // 复用 PaintRow 的 hex / opacity 编辑器，只让 paint 数据跟上文档。
        {
            let prev = self.settings.borrow();
            for row in settings.iter_mut() {
                let Setting::PaintRow { tab, index, hex_ed, op_ed, .. } = row else { continue };
                for old in prev.iter() {
                    if let Setting::PaintRow { tab: ot, index: oi, hex_ed: oh, op_ed: oo, .. } = old {
                        if *ot == *tab && *oi == *index {
                            *hex_ed = oh.clone();
                            *op_ed = oo.clone();
                            break;
                        }
                    }
                }
            }
        }
        self.height = Self::style_editor_height(ty, &paints, &text, &effects, &grids);
        self.settings = Rc::new(RefCell::new(settings));
        if let Some(edit) = self.style_edit.as_mut() {
            edit.paints = paints;
            edit.text = text;
            edit.effects = effects;
            edit.grids = grids;
        }
    }

    /// 打开中的样式编辑器对应的 style id（用于命令回写后判断是否刷新）。
    pub(crate) fn style_id(&self) -> Option<String> {
        self.style_edit.as_ref().map(|e| e.id.clone())
    }

    /// 用文档最新状态刷新打开中的样式编辑器：成功返回 `true`；样式已被删除返回 `false`
    /// （调用方据此关闭弹窗）。命令已执行、文档已更新，这里只是让界面跟上。
    pub(crate) fn refresh_style(&mut self, cx: &mut App, doc: &figcraft_doc::Document) -> bool {
        let id = match self.style_edit.as_ref() {
            Some(e) => e.id.clone(),
            None => return true,
        };
        match doc.style(&id) {
            Some(st) => {
                self.apply_style_snapshot(cx, Self::style_snapshot(st));
                true
            }
            None => false,
        }
    }
    /// 文本样式编辑器的数字字段：提交时把整段 `TextStyle` 写回 `style.update {value}`。
    /// 缺字段会被引擎按默认重置，所以必须整段保留（`text_state`）。
    fn text_number(
        cx: &mut App,
        value: f64,
        key: &'static str,
        style_id: String,
        text_state: Rc<RefCell<serde_json::Value>>,
        cmds: Cmds,
    ) -> Entity<TextField> {
        let field = crate::widgets::number_field(cx, 120.0, Some(value), "", 1.0, Slot::None);
        let cid = style_id.clone();
        let ck = key;
        let cts = text_state.clone();
        let push = cmds.clone();
        field.update(cx, move |tf, _| {
            tf.set_on_commit(move |text, _w, _cx| {
                let parsed = text.trim().parse::<f64>().ok();
                let mut v = cts.borrow_mut();
                match parsed {
                    Some(n) => v[ck] = json!(n),
                    None => {
                        // 字号清空不写回（避免被重置成默认）。
                        if !(ck == "font_size") {
                            v[ck] = json!("auto");
                        }
                    }
                }
                push.push(Cmd { id: "style.update", params: Some(json!({ "style": cid, "value": v.clone() })) });
            });
        });
        field
    }

    /// 行高 / 字距的空字段：`line_height` 空 → `"auto"`（引擎 `props::line_height` 认），
    /// `letter_spacing` 空 → `0`（字距没有 auto；写 "auto" 会让 `props::letter_spacing` 解析失败，
    /// 进而 `style.update` 报错——lizard `parse_letter_spacing` 空串同样得 0）。
    fn text_number_auto(
        cx: &mut App,
        value: Option<f64>,
        key: &'static str,
        style_id: String,
        text_state: Rc<RefCell<serde_json::Value>>,
        cmds: Cmds,
    ) -> Entity<TextField> {
        // 与 `text_number` 同逻辑，只是初始值可能是 None。number_field 对 None 默认给 "Mixed"
        // 占位，但 lizard 的 `LineHeight::label()` / `LetterSpacing::label()` 分别是 "Auto" / "0%"，
        // 覆盖占位符以对齐；真正的写回值仍由 on_commit 决定。
        let field = crate::widgets::number_field(cx, 120.0, value, "", 1.0, Slot::None);
        if value.is_none() {
            let hint = if key == "line_height" { tr("Auto") } else { tr("0") };
            field.update(cx, |tf, _| tf.set_placeholder(hint));
        }
        let cid = style_id.clone();
        let ck = key;
        let cts = text_state.clone();
        let push = cmds.clone();
        field.update(cx, move |tf, _| {
            tf.set_on_commit(move |text, _w, _cx| {
                let parsed = text.trim().parse::<f64>().ok();
                let mut v = cts.borrow_mut();
                match parsed {
                    Some(n) => v[ck] = json!(n),
                    None => v[ck] = if ck == "line_height" { json!("auto") } else { json!(0.0) },
                }
                push.push(Cmd { id: "style.update", params: Some(json!({ "style": cid, "value": v.clone() })) });
            });
        });
        field
    }

    /// 从 `TextStyle` JSON 读一个数字字段（camelCase / snake_case 都认；"auto" / 对象返回 None）。
    fn json_num(v: &Value, keys: &[&str]) -> Option<f64> {
        for k in keys {
            if let Some(x) = v.get(*k) {
                return match x {
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) => {
                        let s = s.trim();
                        if s.eq_ignore_ascii_case("auto") { None } else { s.trim_end_matches('%').trim().parse().ok() }
                    }
                    Value::Object(o) => o.get("px").and_then(|p| p.as_f64()).or_else(|| o.get("value").and_then(|p| p.as_f64())),
                    _ => None,
                };
            }
        }
        None
    }

    /// 从 `TextStyle` JSON 读一个字符串字段（camelCase / snake_case 都认）。
    fn json_str_opt(v: &Value, keys: &[&str]) -> Option<String> {
        for k in keys {
            if let Some(x) = v.get(*k) {
                return x.as_str().map(|s| s.to_string());
            }
        }
        None
    }

    // ── ③ 颜色选择器弹窗（egui `widgets/colorpicker.rs`, 标题 "Custom"）────────────────────
    /// The picker's write-back callback: the canvas eyedropper needs it after sampling.
    pub(crate) fn color_on_pick(&self) -> Option<Rc<dyn Fn(Color32, &mut Window, &mut App)>> {
        self.color.as_ref().map(|c| c.on_pick.clone())
    }

    /// 删除当前选中的渐变断点（Delete / Backspace）。至少保留 2 个，删后选中相邻一个。
    /// 返回是否消费了这次按键（非渐变 / 无可删则 false，让按键继续冒泡）。
    pub(crate) fn delete_selected_stop(&mut self, window: &mut Window, cx: &mut App) -> bool {
        let Some(c) = &self.color else { return false };
        let Some(state) = &c.paint_state else { return false };
        let Some(p) = &c.paint else { return false };
        let mut np = state.borrow().clone();
        let figcraft_color::PaintKind::Gradient(g) = &mut np.kind else { return false };
        if g.stops.len() <= 2 {
            return false;
        }
        let idx = c.grad_sel.get().min(g.stops.len() - 1);
        g.stops.remove(idx);
        c.grad_sel.set(idx.min(g.stops.len() - 1));
        let write = p.on_write.clone();
        write(np, window, cx);
        true
    }

    /// 登记当帧的内部菜单与其关闭回调。`render()` 据此在窗口根垫一层透明 backdrop：
    /// 点击菜单以外的任何位置都会关闭菜单（通用行为）。
    fn push_overlay(&self, el: AnyElement, dismiss: Rc<dyn Fn(&mut Window, &mut App)>) {
        self.overlays.borrow_mut().push(el);
        self.overlay_dismiss.borrow_mut().push(dismiss);
    }

    pub(crate) fn color(
        cx: &mut App,
        origin: Point<Pixels>,
        current: Color32,
        paint: Option<PaintPicker>,
        contrast: crate::panels::right::data::ContrastContext,
        on_pick: Rc<dyn Fn(Color32, &mut Window, &mut App)>,
        cmds: Cmds,
    ) -> Self {
        let mut w = Self::base(WindowKind::Color, tr("Custom"), cmds, Open::Anchor(origin));
        // Figma 的 paint 面板是 240 宽、上下 8 内边距、左右 0（`raw_components--modalPanel`），
        // 每一行自己给 8 / 16 的内缩。卡片内边距因此归零，宽度就是面板本身的 240。
        w.width = 240.0;
        w.pad = 0.0;

        let grad_sel = Rc::new(Cell::new(0usize));
        let grad_kind = Rc::new(Cell::new(0usize));
        let blend_anchor = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let swatch_anchor = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let swatch_set = Rc::new(Cell::new(0u8));
        // Mirror every write into a shared paint, so the kind / stop editors read the new value
        // on the very next frame.
        let paint_state = paint.as_ref().map(|p| Rc::new(RefCell::new(p.paint.clone())));
        let paint = paint.map(|mut p| {
            let Some(cell) = paint_state.clone() else { return p };
            let outer = p.on_write.clone();
            p.on_write = Rc::new(move |np, w, cx| {
                *cell.borrow_mut() = np.clone();
                outer(np, w, cx);
                // 命令队列只唤醒主窗口；取色器是独立 OS 窗口，必须自己刷新，
                // 否则切换纯色/渐变/图片/图案后旧内容会一直留在画面上（用户 2026-11-19）。
                w.refresh();
            });
            p
        });
        // 渐变打开时默认选中 stop 0（`grad_sel` 从 0 开始），picker 的当前色必须与之同步，
        // 否则第一次改 alpha 会把 BLACK 写进 stop 0（CodeRabbit #30/#29）。
        let initial = match paint.as_ref().map(|p| &p.paint.kind) {
            Some(figcraft_color::PaintKind::Gradient(g)) => g.stops.first().map_or(current, |s| picker_c32(s.color)),
            _ => current,
        };
        let cur = Rc::new(Cell::new(initial));
        let model = Rc::new(Cell::new(0usize));
        let model_anchor = Rc::new(Cell::new(None::<Bounds<Pixels>>));
        let shown = Rc::new(Cell::new((usize::MAX, [0u8; 4])));
        // 样式属性行用的是 `PaintPicker.on_write`（整份 paint 写回 `style.update`），
        // 而纯色/色相/透明度拖动只走 `pick`。这里必须把选中的颜色也揉进 paint 再调用
        // `on_write`，否则「样式列表的取色器选完颜色什么都不发生」（用户 2026-11-17）。
        let paint_write = paint.as_ref().map(|p| p.on_write.clone());
        // Every pick updates the shared colour first, so the SV square / hue / alpha strips
        // repaint from the new value instead of staying frozen at the colour they opened with.
        // Picks also feed the session-wide "Recent" row (ui-egui `remember_recent`).
        let pick: Rc<dyn Fn(Color32, &mut Window, &mut App)> = {
            let cur = cur.clone();
            let paint_state = paint_state.clone();
            let grad_sel = grad_sel.clone();
            Rc::new(move |c: Color32, window: &mut Window, cx: &mut App| {
                cur.set(c);
                window.refresh();
                remember_recent(c);
                if let (Some(state), Some(write)) = (&paint_state, &paint_write) {
                    let mut np = state.borrow().clone();
                    match &mut np.kind {
                        figcraft_color::PaintKind::Solid { color } => {
                            // 透明度和 `Paint::solid` 同一约定：色值本身不透明，opacity 承担 alpha。
                            *color = picker_color(c).with_alpha(1.0);
                            np.opacity = f32::from(c.0[3]) / 255.0;
                            np.color_alpha = np.opacity;
                        }
                        figcraft_color::PaintKind::Gradient(g) => {
                            let i = grad_sel.get().min(g.stops.len().saturating_sub(1));
                            if let Some(s) = g.stops.get_mut(i) {
                                s.color = picker_color(c);
                            }
                        }
                        _ => {}
                    }
                    write(np, window, cx);
                    // 整份 paint 已经写好：再调 `on_pick` 会用取色器打开时的 `base_paint`
                    // 快照发第二次 `selection.setPaint`，把刚改的渐变 stop / opacity 覆盖回
                    // 原值（CodeRabbit #30/#31/#32）。只有在没有整份写入器时才退回旧回调。
                    return;
                }
                on_pick(c, window, cx);
            })
        };

        // Hex / CSS text field.
        let hex = TextField::build(cx, 240.0, |tf, _| {
            tf.set_value(current.to_hex());
            tf.set_placeholder("Hex");
        });
        {
            let commit = pick.clone();
            let hm = model.clone();
            hex.update(cx, move |tf, _| {
                tf.set_on_commit(move |text, window, cx| {
                    if COLOR_MODELS.get(hm.get()) == Some(&"CSS") {
                        if let Some(c) = figcraft_color::Color::parse_css(text) {
                            commit(picker_c32(c), window, cx);
                        }
                    } else if let Some(c) = Color32::from_hex(text) {
                        commit(c, window, cx);
                    }
                });
            });
        }

        // Three numeric fields, one per channel of the active model.
        let fields: [Entity<TextField>; 3] = std::array::from_fn(|k| {
            let tf = crate::widgets::number_field(cx, 31.0, None, "", 1.0, Slot::None);
            let commit = pick.clone();
            let m = model.clone();
            let c = cur.clone();
            tf.update(cx, move |tf, _| {
                tf.set_on_commit(move |text, window, cx| {
                    let Ok(x) = text.trim().parse::<f64>() else { return };
                    let mm = m.get();
                    let Some(mut vals) = model_values(mm, c.get()) else { return };
                    vals[k] = clamp_model_value(mm, k, x);
                    commit(model_color(mm, vals, c.get()), window, cx);
                });
            });
            tf
        });

        // Alpha percentage.
        let alpha = TextField::build(cx, 44.0, |tf, _| tf.set_placeholder("%"));
        {
            let commit = pick.clone();
            let c = cur.clone();
            alpha.update(cx, move |tf, _| {
                tf.set_on_commit(move |text, window, cx| {
                    let Ok(x) = text.trim().trim_end_matches('%').trim().parse::<f64>() else { return };
                    let a = ((x / 100.0).clamp(0.0, 1.0) * 255.0).round() as u8;
                    commit(c.get().with_alpha(a), window, cx);
                });
            });
        }

        w.color = Some(ColorState {
            current: cur,
            paint,
            paint_state,
            grad_sel,
            grad_kind,
            swatch_set,
            swatch_anchor,
            blend_anchor,
            grad_kind_anchor: Rc::new(Cell::new(None)),
            sub: Rc::new(RefCell::new(None)),
            nested: false,
            uid: {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static NEXT: AtomicUsize = AtomicUsize::new(1);
                NEXT.fetch_add(1, Ordering::Relaxed)
            },
            sv_drag: Rc::new(Cell::new(false)),
            grad_drag: Rc::new(Cell::new(None)),
            grad_press: Rc::new(Cell::new(None)),
            grad_moved: Rc::new(Cell::new(false)),
            grad_bounds: Rc::new(Cell::new(None)),
            stop_fields: Rc::new(RefCell::new(Vec::new())),
            stop_shown: Rc::new(RefCell::new(Vec::new())),
            model,
            model_anchor,
            shown,
            hex,
            fields,
            alpha,
            on_pick: pick,
            contrast,
            aa_category: std::rc::Rc::new(std::cell::RefCell::new(None)),
            aa_level: std::rc::Rc::new(std::cell::RefCell::new(figcraft_color::ContrastLevel::AA)),
            aa_anchor: std::rc::Rc::new(std::cell::RefCell::new(None)),
            hsb_cache: std::rc::Rc::new(std::cell::Cell::new(None)),
        });
        // 卡片高度按面板实际内容算（见 `color_card_height`），`render` 每帧会再同步一次，
        // 所以纯色 ↔ 渐变、色板行数变化时卡片会自己长/缩。
        w.height = w.color_card_height();
        w
    }

    /// 由渐变断点 / 断点行颜色弹出的取色器：同一张卡片，只是隐藏顶部「纯色 / 渐变」
    /// 切换条（用户要求这个面板不带上那些条目）。
    pub(crate) fn color_nested(
        cx: &mut App,
        origin: Point<Pixels>,
        current: Color32,
        paint: Option<PaintPicker>,
        contrast: crate::panels::right::data::ContrastContext,
        on_pick: Rc<dyn Fn(Color32, &mut Window, &mut App)>,
        cmds: Cmds,
    ) -> Self {
        let mut w = Self::color(cx, origin, current, paint, contrast, on_pick, cmds);
        if let Some(c) = w.color.as_mut() {
            c.nested = true;
        }
        w.height = w.color_card_height();
        w
    }

    fn add_search(&mut self, cx: &mut App, hint: &str) {
        let query = self.query.clone();
        // 字体窗的搜索行自带 16/12 的左右内缩（Figma `fontPickerSearchRow`），字段要减掉，
        // 否则固定宽度会把搜索框顶出卡片。
        let inset = if self.kind == WindowKind::FontPicker { 16.0 + 12.0 } else { 2.0 * self.pad };
        let field = search_field(cx, self.width - inset, "", hint);
        // 边打边过滤（`on_change`），不是回车才生效。
        let wake = self.cmds.clone();
        field.update(cx, move |tf, _| {
            tf.set_on_change(move |text, _, _| {
                *query.borrow_mut() = text.to_lowercase();
                wake.wake();
            });
        });
        self.search = Some(field);
    }

    // ── drag：状态机在 `hold_drag`，这里只把结果写回 `place` ─────────────────────────────
    /// 按下：只记状态、**不重绘**（GPUI 靠 ElementId 匹配 click 的 down/up）。
    pub(crate) fn press(&mut self, at: Point<Pixels>) {
        self.hold.press(at, self.origin);
    }
    /// 已按下但还没越过 3pt 死区时的按下点。
    pub(crate) fn pending(&self) -> Option<Point<Pixels>> {
        self.hold.pending_point()
    }
    /// 显式 latch（跳过死区），供测试与需要立即起拖的调用方。
    pub(crate) fn start_drag(&mut self) {
        self.hold.latch();
    }
    pub(crate) fn dragging(&self) -> bool {
        self.hold.active()
    }
    /// 通用状态机给出「按下时原点 + 相对位移」的绝对位置，再按视口夹紧（egui 每步都 clamp）。
    /// 返回 true 仅当卡片确实移动了。
    pub(crate) fn move_drag(&mut self, at: Point<Pixels>) -> bool {
        let Some(step) = self.hold.advance(at) else { return false };
        let p = popover::clamp(step.place, self.width, self.height, self.viewport);
        if let Some(old) = self.place {
            let dx = f32::from(p.x) - f32::from(old.x);
            let dy = f32::from(p.y) - f32::from(old.y);
            if dx.abs() < 0.5 && dy.abs() < 0.5 {
                return false;
            }
        }
        self.place = Some(p);
        true
    }
    pub(crate) fn end_drag(&mut self) {
        self.hold.end();
    }

    // ── 子浮层（属性行点开的取色器 / 排版设置）的**独立**拖动 ────────────────────────────
    //
    // 子浮层挂在 `StyleEditState.picker`（`Rc<RefCell<..>>`）上，不占 root 的 `popup` 单槽，
    // 所以它有**自己的** `hold` / `place`：拖它只挪它自己，父卡片一动不动
    // （用户 2026-11-15：「打开的第 2 个窗口没有办法拖动，跟他绑死在一起了」）。
    // 下面几个访问器都借 `&self` + 内部 `RefCell`，好让根视图的窗口级 move / up 直接路由进来。
    /// 借进 `picker` 跑一段（子浮层存在时才执行）。渲染期它被临时取出来，这里会返回 `None`。
    fn with_sub<R>(&self, f: impl FnOnce(&mut WindowState) -> R) -> Option<R> {
        if let Some(se) = &self.style_edit {
            let slot = se.picker.clone();
            let mut b = slot.borrow_mut();
            if let Some(c) = b.as_mut() {
                return Some(f(c.as_mut()));
            }
        }
        // 取色器自己也能带一张子卡片（渐变断点色 / 样式面板）。它同样需要窗口级
        // move / up 路由，否则拖动子卡片时松手事件收不到，卡片会一直粘在指针上
        // （用户 2026-11-19：「拖出来之后我甚至都放不下去了」）。
        if let Some(cs) = &self.color {
            let slot = cs.sub.clone();
            let mut b = slot.borrow_mut();
            if let Some(c) = b.as_mut() {
                return Some(f(c.as_mut()));
            }
        }
        None
    }
    /// 子浮层已按下但还没越过死区。
    pub(crate) fn sub_pending(&self) -> bool {
        self.with_sub(|c| c.pending().is_some()).unwrap_or(false)
    }
    /// 子浮层正在被拖动。
    pub(crate) fn sub_dragging(&self) -> bool {
        self.with_sub(|c| c.dragging()).unwrap_or(false)
    }
    /// 子浮层按下：只记状态、不重绘（与 `press` 同一套 `hold_drag`，但写它自己的 `origin`）。
    pub(crate) fn sub_press(&self, at: Point<Pixels>) {
        let _ = self.with_sub(|c| c.press(at));
    }
    /// 子浮层移动：写回它自己的 `place`，父卡片的 `place` 完全不碰。
    pub(crate) fn sub_move_drag(&self, at: Point<Pixels>) -> bool {
        self.with_sub(|c| c.move_drag(at)).unwrap_or(false)
    }
    /// 子浮层收尾（抬起 / 窗口外松手 / Escape）。
    pub(crate) fn sub_end_drag(&self) {
        let _ = self.with_sub(|c| c.end_drag());
    }

    /// 仅供端到端测试读取拖拽后的卡片原点（窗口坐标）。
    #[cfg(test)]
    pub(crate) fn test_origin(&self) -> Point<Pixels> {
        self.origin
    }
    /// 仅供端到端测试读取拖拽后的卡片落点（None = 尚未拖动）。
    #[cfg(test)]
    pub(crate) fn test_place(&self) -> Option<Point<Pixels>> {
        self.place
    }
    /// 仅供端到端测试读取 resize 后的卡片尺寸（None = 还没改过尺寸）。
    #[cfg(test)]
    pub(crate) fn test_size(&self) -> Option<(f32, f32)> {
        self.editor.as_ref().and_then(|e| e.size())
    }

    // ── header title slot ───────────────────────────────────────────────────────────────────
    fn title_slot(&self) -> AnyElement {
        let t = tokens();
        match self.kind {
            // A custom title: the current colour's swatch beside the usual label.
            WindowKind::Color => {
                let c = self.color.as_ref();
                let col = c.map(|c| c.current.get()).unwrap_or(Color32::BLACK);
                let nested = c.is_some_and(|c| c.nested);
                let mut row = div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(pt(8.0))
                    .child(div().size(pt(12.0)).rounded(pt(3.0)).bg(rgba(col)).border_1().border_color(rgba(t.border)))
                    .child(div().flex_1().min_w(pt(0.0)).child(title_text(tr("Custom"))));
                // 头部「+」：在取色器**左边**再开一张样式面板，取色器本身不关。
                if let Some(c) = c
                    && !nested
                {
                    let styles = c.paint.as_ref().map(|p| p.styles.clone()).unwrap_or_default();
                    {
                        let sub = c.sub.clone();
                        let cmds = self.cmds.clone();
                        let top = self.origin.y;
                        row = row.child(IconButton::new(Icon::Plus).size(24.0).on_click(move |_, _window, cx| {
                            let mut child =
                                WindowState::styles(cmds.clone(), cx, Point { x: px(0.0), y: top }, tr("Color styles"), "paint", styles.clone());
                            child.anchor_to_button_top(top);
                            *sub.borrow_mut() = Some(Box::new(child));
                            cmds.wake();
                        }));
                    }
                }
                row.into_any_element()
            }
            // A custom title with a live control: create a style straight from the header.
            // egui `style_picker::open`：四个 slot 共用一类窗口，「+」按 slot 决定 create_type
            // 与默认名（StyleSlot::create_type/default_name）。之前硬编码 text，导致填充/描边/
            // 效果/网格窗口里点「+」都建成文本样式 —— 这里按 slot 恢复各自的类型。
            WindowKind::TextStyles => {
                let cmds = self.cmds.clone();
                let ty = self.style_create_ty;
                // 默认名沿用 EGUI `StyleSlot::default_name` 的基础词；这些键 i18n 已存在，
                // 引擎会在同名时自动加序号（Color 1 / Color 2…）。
                let (create_type, default_name) = match ty {
                    "paint" => ("paint", tr("Color")),
                    "effect" => ("effect", tr("Effect styles")),
                    "grid" => ("grid", tr("Grid")),
                    _ => ("text", tr("Text")),
                };
                div()
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(pt(4.0))
                    .child(div().flex_1().child(title_text(self.title.clone())))
                    .child(IconButton::new(Icon::Plus).size(20.0).on_click(move |_, _, _| {
                        cmds.push(Cmd { id: "style.create", params: Some(json!({ "type": create_type, "name": default_name })) });
                    }))
                    .into_any_element()
            }
            // A custom title with a count chip.
            WindowKind::Variables => {
                let n = self.rows.len();
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(pt(8.0))
                    .child(title_text(self.title.clone()))
                    .child(
                        div()
                            .px(pt(5.0))
                            .h(pt(16.0))
                            .rounded(pt(4.0))
                            .bg(rgba(t.hover))
                            .flex()
                            .items_center()
                            .text_size(pt(9.0))
                            .text_color(rgba(t.text_secondary))
                            .child(format!("{n}")),
                    )
                    .into_any_element()
            }
            // 变量编辑器把「标题 + 计数」画进自己的工具条：整窗一个插槽，顶部只有一行。
            WindowKind::VariablesTable => div().into_any_element(),
            WindowKind::Layout
            | WindowKind::Stroke
            | WindowKind::EffectSettings
            | WindowKind::StyleEditor
            | WindowKind::CornerSmoothing
            | WindowKind::ExportSettings
            | WindowKind::FontPicker => title_text(self.title.clone()).into_any_element(),
            // Type settings 的标题栏就是三页签（截图顶部没有单独的 "Type settings" 标题文字），
            // 与 lizard `type_settings_panel` 把 Tabs 放进 header 一致。
            WindowKind::TypeSettings => {
                if let Some((names, active)) = &self.tabs {
                    let tab = *active.borrow();
                    // 页签胶囊：整体垂直居中、靠左排、块间 4；每块高 24、文字左右 8，
                    // 只有选中块有底色（与参考截图一致：未选中的「基础/变量」是透明底）。
                    let mut row = div().w_full().flex().flex_row().items_center().gap(pt(4.0));
                    for (i, name) in names.iter().enumerate() {
                        let a = active.clone();
                        let wake = self.cmds.clone();
                        let on = i as u8 == tab;
                        let mut cell = div()
                            .flex_none()
                            .h(pt(24.0))
                            .px(pt(8.0))
                            .rounded(pt(4.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(pt(11.0))
                            .text_color(rgba(if on { t.text } else { t.text_secondary }))
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                                *a.borrow_mut() = i as u8;
                                wake.wake();
                            });
                        if on {
                            cell = cell.bg(rgba(t.selected_row));
                        }
                        row = row.child(cell.child(name.clone()));
                    }
                    row.into_any_element()
                } else {
                    title_text(self.title.clone()).into_any_element()
                }
            }
        }
    }

    // ── body slot ───────────────────────────────────────────────────────────────────────────
    /// 取色器卡片的当前高度：Figma 面板是「内容自然高度」，所以这里按行累加，而不是写死。
    /// 渐变模式会多出「渐变类型 + 停靠点条」两段，色板行数随当前色板集变化。
    fn color_card_height(&self) -> f32 {
        let Some(c) = self.color.as_ref() else { return HEADER_H + 500.0 };
        let mut h = 8.0;
        if c.paint.is_some() && !c.nested {
            h += 40.0;
        }
        let grad = matches!(c.paint_state.as_ref().map(|s| s.borrow().kind.clone()), Some(figcraft_color::PaintKind::Gradient(_)));
        if grad {
            // 类型行 48 + 预览条 8/48 + 「断点」标题 8/32 + 每行 32 + 底部 12
            let stops = c
                .paint_state
                .as_ref()
                .and_then(|s| match &s.borrow().kind {
                    figcraft_color::PaintKind::Gradient(g) => Some(g.stops.len()),
                    _ => None,
                })
                .unwrap_or(2) as f32;
            h += 48.0 + 8.0 + 48.0 + 8.0 + 32.0 + stops * 32.0 + 12.0;
        } else {
            // 对比度 48 + 色域 208 + 滑轨 48 + 数值 40 + 色板
            h += 48.0 + SPECTRUM + 48.0 + 40.0;
            let n = page_colors().len().max(recent_colors().len());
            let rows = n.div_ceil(CHITS_PER_ROW).clamp(1, 3) as f32;
            // 顶边 1 + 上 11 + 下拉 24 + 下 4 + 色块 rows×24 − 末行 8 + 底部 12
            h += 1.0 + 11.0 + 24.0 + 4.0 + rows * CHIT_PITCH - (CHIT_PITCH - CHIT) + 12.0;
        }
        h += 8.0;
        HEADER_H + h
    }

    /// `body_h` = 通用窗口留给 body 的高度（Full 时是整个视口减去 header）。
    fn body_element(&mut self, cx: &mut App, window: &mut Window, body_h: f32, card_w: f32, viewport: Size<Pixels>) -> AnyElement {
        match self.kind {
            WindowKind::Color => self.color_body(cx),
            WindowKind::CornerSmoothing => self.corner_smoothing_body(),
            WindowKind::FontPicker => self.list_element(cx, body_h),
            WindowKind::Layout
            | WindowKind::Stroke
            | WindowKind::TypeSettings
            | WindowKind::EffectSettings
            | WindowKind::ExportSettings
            | WindowKind::StyleEditor => self.settings_element(cx),
            WindowKind::Variables | WindowKind::TextStyles => self.list_element(cx, body_h),
            WindowKind::VariablesTable => {
                if self.editor.is_none() {
                    if let Some(data) = self.variables_data.take() {
                        let editor = VariablesEditor::new(
                            self.cmds.clone(),
                            data,
                            cx,
                            self.var_popups.clone(),
                            self.var_on_full.clone(),
                            self.var_on_close.clone(),
                        );
                        // 把在 editor 建立之前就收到的 Full/Floating 记住值灌进去。
                        if let Some(full) = self.editor_full {
                            editor.set_full(full);
                        }
                        self.editor = Some(editor);
                    }
                }
                // 卡片在窗口中的原点：面板内部的右键菜单用**窗口坐标**记录按下点，而菜单元素
                // 挂在卡片这个 `relative` 容器里，需要换算成卡片局部坐标，否则弹出位置会整体偏移。
                let origin = self.origin;
                let drag_start = self.drag_start.clone();
                match &mut self.editor {
                    Some(e) => e.render(cx, window, body_h, viewport, origin, drag_start, card_w),
                    None => div().into_any_element(),
                }
            }
        }
    }

    // ── 取色器（Figma UI3 paint 面板 1:1 还原）────────────────────────────────────────────
    // 全部数值取自 `Figma – Figma.html` 快照里的 DOM 与 `figma_app-*.css`，不是从截图量的：
    //   面板 240 宽、上下 8          `raw_components--modalPanel--CWaX0{width:240px;padding:8px 0}`
    //   类型条 40 高、padding 8、底边 1  `color_picker_v2--typeAndBlendMode--VVL-m`
    //   类型按钮 24×24、圆角 5          `paint_type_group_tabs--fillTypeButton--9gjgS`
    //   对比度行 48 高、左右 8          `color_contrast_info--colorContrastRowBase--C4Y1J`
    //   色域 208×208、圆角 5、1 px 描边  `color_controls--colorSpectrumWrapper--pdxO1` + `_1t0vmmd4`
    //   控制行左右 16、gap 12           `color_controls--controls--U5kBU`
    //   滑轨 16 高、全圆角、1 px 描边    `_1byqy9f5` / `_1byqy9f6`
    //   数值行 40 高、grid 56 + 1fr     `color_controls--colorFormatRow--QKcGI`
    //   色板顶边 1 px、上 11、色块 16     `library_color_swatch--panel--NcpH0` / `chit--chit---D2JR`

    fn color_body(&mut self, cx: &mut App) -> AnyElement {
        let Some(c) = &self.color else { return div().into_any_element() };
        let t = tokens();
        let current = c.current.get();
        let model = c.model.get();
        // Only write the fields when the model or the colour actually changed: rewriting them
        // every frame would erase what the user is typing.
        if c.shown.get() != (model, current.0) {
            c.shown.set((model, current.0));
            match model_values(model, current) {
                Some(vals) => {
                    for (k, f) in c.fields.iter().enumerate() {
                        let d = model_decimals(model, k);
                        let text = format!("{:.*}", d, vals[k]);
                        f.update(cx, |tf, _| tf.set_value(text));
                    }
                }
                None => {
                    let text = if COLOR_MODELS.get(model) == Some(&"CSS") { picker_color(current).to_css() } else { current.to_hex() };
                    c.hex.update(cx, |tf, _| tf.set_value(text));
                }
            }
            let a = (f64::from(current.0[3]) / 255.0 * 100.0).round() as u32;
            c.alpha.update(cx, |tf, _| tf.set_value(a.to_string()));
        }
        // 拖动中复用缓存的 HSB（hue 稳定，不抖）；缓存颜色和当前色不一致（外部改色）才反算。
        let hsb = c
            .hsb_cache
            .get()
            .filter(|(_, _, _, rgba)| *rgba == current.0)
            .map(|(h, s, b, _)| figcraft_color::Hsb { h, s, b })
            .unwrap_or_else(|| figcraft_color::Hsb::from_color(picker_color(current)));
        let hue = hsb.h;
        let alpha = f32::from(current.0[3]) / 255.0;
        let pick = c.on_pick.clone();
        // Eyedropper: sample on the canvas; the sample comes back through `tool.picked`.
        let eyedrop_cmds = self.cmds.clone();

        // ① 色域方块：两个线性渐变 quad（底层 白→色相，上层 透明→黑）。
        //    prepaint 记下盒子在窗口中的位置，点击时按相对坐标取 s / v。
        let sv_hit: Rc<std::cell::Cell<Option<Bounds<Pixels>>>> = Rc::new(std::cell::Cell::new(None));
        let sv_prepaint = sv_hit.clone();
        let sv_click = sv_hit.clone();
        let sv_pick = {
            let pick = pick.clone();
            let cur = c.current.clone();
            let cache = c.hsb_cache.clone();
            move |h: f32, s: f32, v: f32, window: &mut Window, cx: &mut App| {
                // RGB-only 编辑要保留上一次提交色的 alpha，否则 0.4 的纯色一拖 SV 就变全不透明
                //（CodeRabbit #30）。alpha 交给 `pick` 里的 paint 写入。
                let a = cur.get().0[3];
                let color = hsb_color(h, s, v).with_alpha(a);
                cache.set(Some((h, s, v, color.0)));
                pick(color, window, cx);
            }
        };
        // ── AA 对比度：类别/等级来自 ColorState，背景来自选区快照，白线是达标边界 ──────
        let aa_category_val = c.aa_category.borrow().unwrap_or(c.contrast.auto_category);
        let aa_level_val = *c.aa_level.borrow();
        let aa_min = figcraft_color::minimum(aa_category_val, aa_level_val);
        let aa_fg = picker_color(current);
        let aa_bg = c.contrast.bg;
        let aa_ratio = figcraft_color::contrast_ratio(aa_fg, aa_bg);
        let aa_pass = aa_min.is_some_and(|m| aa_ratio >= m);
        // graphics 无 AAA → 无线；否则反解亮/暗两侧边界，供 SV canvas 画连续白线。
        let aa_boundary = aa_min.map(|m| figcraft_color::boundary_line(aa_bg, m, hue));
        let aa_boundary_paint = aa_boundary.clone();
        let hue_c = hsb_color(hue, 1.0, 1.0);
        let sv_drag = c.sv_drag.clone();
        let sv_drag_down = sv_drag.clone();
        let sv_drag_win = sv_drag.clone();
        let sv_drag_end = sv_drag.clone();
        let sv_hit_win = sv_hit.clone();
        let sv_pick_down = sv_pick.clone();
        let sv_pick_win = sv_pick.clone();
        let spectrum = div()
            .relative()
            .w(pt(SPECTRUM))
            .h(pt(SPECTRUM))
            .child(
                div().w(pt(SPECTRUM)).h(pt(SPECTRUM)).rounded(pt(5.0)).border_1().border_color(rgba(t.border)).overflow_hidden().child(
                    gpui::canvas(
                        move |bounds, _, _| sv_prepaint.set(Some(bounds)),
                        move |bounds, _, window, _| {
                            let white = gpui::Hsla::from(rgba(Color32::from_rgb(255, 255, 255)));
                            let pure = gpui::Hsla::from(rgba(hue_c));
                            let clear = gpui::transparent_black();
                            let black = gpui::Hsla::from(rgba(Color32::from_rgb(0, 0, 0)));
                            // 90° = 左→右（饱和度 0→1），180° = 上→下（明度 1→0）。
                            window.paint_quad(gpui::fill(
                                bounds,
                                gpui::linear_gradient(90.0, gpui::linear_color_stop(white, 0.0), gpui::linear_color_stop(pure, 1.0)),
                            ));
                            window.paint_quad(gpui::fill(
                                bounds,
                                gpui::linear_gradient(180.0, gpui::linear_color_stop(clear, 0.0), gpui::linear_color_stop(black, 1.0)),
                            ));
                            // AA 达标边界白线：一条连续 stroke 折线（lyon 自动补 join + 抗锯齿）。
                            if let Some((lighter, darker)) = &aa_boundary_paint {
                                let bx = f32::from(bounds.origin.x);
                                let by = f32::from(bounds.origin.y);
                                let bw = f32::from(bounds.size.width);
                                let bh = f32::from(bounds.size.height);
                                let sc = rgba(Color32::from_rgba_unmultiplied(255, 255, 255, 217));
                                for l in [lighter, darker] {
                                    if l.len() < 2 {
                                        continue;
                                    }
                                    let mut b = gpui::PathBuilder::stroke(px(1.25));
                                    for (i, pt) in l.iter().enumerate() {
                                        let p = point(px(bx + pt.s * bw), px(by + (1.0 - pt.v) * bh));
                                        if i == 0 {
                                            b.move_to(p);
                                        } else {
                                            b.line_to(p);
                                        }
                                    }
                                    if let Ok(path) = b.build() {
                                        window.paint_path(path, sc);
                                    }
                                }
                            }
                            // 拖动监听必须挂在窗口级：指针一旦移出色域方块就收不到元素的
                            // move，手柄会卡住（用户实测「手柄拖不动 / 放不下」）。
                            {
                                let dragging = sv_drag_win.clone();
                                let hit = sv_hit_win.clone();
                                let pick = sv_pick_win.clone();
                                window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                                    if !phase.bubble() || !dragging.get() {
                                        return;
                                    }
                                    let Some(b) = hit.get() else { return };
                                    let s = ((f32::from(e.position.x) - f32::from(b.origin.x)) / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
                                    let v =
                                        1.0 - ((f32::from(e.position.y) - f32::from(b.origin.y)) / f32::from(b.size.height).max(1.0)).clamp(0.0, 1.0);
                                    pick(hue, s, v, window, cx);
                                });
                                let dragging = sv_drag_end.clone();
                                window.on_mouse_event(move |_e: &gpui::MouseUpEvent, phase, _, _| {
                                    if phase.bubble() {
                                        dragging.set(false);
                                    }
                                });
                            }
                        },
                    )
                    .size_full(),
                ),
            )
            // 拾色点：Figma `_1t0vmmd5/7` 是 16 pt 白环 + 4 pt 白边 + 中间填当前色，
            // 环外还有一圈 1 px `controlknoboffoutline`（这里用 18 pt 半透明底托出来）。
            .child(picker_knob(hsb.s * SPECTRUM, (1.0 - hsb.b) * SPECTRUM, current.with_alpha(255)))
            // 事件挂在**外层**：手柄绝对定位盖在色板上，若只挂在色板 div 上，
            // 按在手柄圆心上会点不到（用户实测手柄拖不动）。
            .on_mouse_down(MouseButton::Left, move |e, window, cx| {
                sv_drag_down.set(true);
                let Some(b) = sv_click.get() else { return };
                let s = ((f32::from(e.position.x) - f32::from(b.origin.x)) / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
                let v = 1.0 - ((f32::from(e.position.y) - f32::from(b.origin.y)) / f32::from(b.size.height).max(1.0)).clamp(0.0, 1.0);
                sv_pick_down(hue, s, v, window, cx);
            });

        // ② 色相滑轨：通用 `Slider`（受控：拖动回调写回颜色，下一帧按新色相重算位置），
        //    轨道整条自绘成 6 段 60° 线性渐变。
        let hue_pick = {
            let pick = pick.clone();
            let cur = c.current.clone();
            let cache = c.hsb_cache.clone();
            // 只改色相，保留当前饱和度/明度（EGUI 的 `hsb.h = t * 360` 其余不动）。
            let keep = (hsb.s, hsb.b);
            move |h: f32, window: &mut Window, cx: &mut App| {
                let a = cur.get().0[3];
                let color = hsb_color(h, keep.0, keep.1).with_alpha(a);
                cache.set(Some((h, keep.0, keep.1, color.0)));
                pick(color, window, cx);
            }
        };
        let hue_slider = crate::widgets::Slider::new(hue, 0.0, 360.0)
            .id(format!("color-{}-hue", c.uid))
            .step(1.0)
            .paint_track(|window, bounds, _| {
                let x0 = f32::from(bounds.origin.x);
                let y0 = f32::from(bounds.origin.y);
                let w = f32::from(bounds.size.width).max(1.0);
                let h = f32::from(bounds.size.height).max(1.0);
                for i in 0..6 {
                    let a = hsb_color(i as f32 * 60.0, 1.0, 1.0);
                    let b = hsb_color((i + 1) as f32 * 60.0, 1.0, 1.0);
                    window.paint_quad(gpui::fill(
                        Bounds { origin: point(px(x0 + w * i as f32 / 6.0), px(y0)), size: size(px(w / 6.0 + 0.5), px(h)) },
                        gpui::linear_gradient(
                            90.0,
                            gpui::linear_color_stop(gpui::Hsla::from(rgba(a)), 0.0),
                            gpui::linear_color_stop(gpui::Hsla::from(rgba(b)), 1.0),
                        ),
                    ));
                }
            })
            .on_change(move |v, window, cx| hue_pick(v, window, cx))
            .render();

        // ③ 透明度滑轨：同样用 `Slider`，轨道先棋盘再叠透明→当前色的水平渐变。
        let alpha_pick = pick.clone();
        let opaque = current.with_alpha(255);
        let alpha_slider = crate::widgets::Slider::new(alpha, 0.0, 1.0)
            .id(format!("color-{}-alpha", c.uid))
            .step(1.0 / 255.0)
            .paint_track(move |window, bounds, _| {
                crate::widgets::swatch::checker(window, bounds, 4.0);
                window.paint_quad(gpui::fill(
                    bounds,
                    gpui::linear_gradient(
                        90.0,
                        gpui::linear_color_stop(gpui::transparent_black(), 0.0),
                        gpui::linear_color_stop(gpui::Hsla::from(rgba(opaque)), 1.0),
                    ),
                ));
            })
            .on_change(move |v, window, cx| alpha_pick(opaque.with_alpha((v * 255.0).round() as u8), window, cx))
            .render();

        // ④ 数值行：格式下拉 56 + 三个通道字段（1fr）+ 不透明度 54，字段高 24。
        let model_name = COLOR_MODELS.get(model).copied().unwrap_or("Hex");
        let model_trigger = c.model_anchor.clone();
        let model_hit: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let format_select = div()
            .relative()
            .flex_none()
            .w(pt(56.0))
            .h(pt(24.0))
            .pl(pt(8.0))
            .rounded(pt(5.0))
            .border_1()
            .border_color(rgba(t.border))
            .bg(rgba(t.panel))
            .flex()
            .items_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgba(t.hover)))
            .child(div().flex_1().min_w(pt(0.0)).overflow_hidden().text_size(pt(11.0)).text_color(rgba(t.text)).child(model_name))
            .child(picker_icons::paint(PickerIcon::Chevron, rgba(t.icon), 24.0))
            .child(bounds_probe(model_hit.clone()))
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                if let Some(b) = model_hit.get() {
                    model_trigger.set(Some(b));
                    window.refresh();
                }
            });
        let mut fields_row = div().flex_1().min_w(pt(0.0)).h(pt(24.0)).flex().flex_row().items_center().gap(pt(1.0));
        let cell = |e: AnyElement| div().flex_1().min_w(pt(0.0)).h(pt(24.0)).rounded(pt(5.0)).bg(rgba(t.field)).child(e);
        if model_values(model, current).is_some() {
            for f in &c.fields {
                fields_row = fields_row.child(cell(f.clone().into_any_element()));
            }
        } else {
            fields_row = fields_row.child(cell(c.hex.clone().into_any_element()));
        }
        fields_row = fields_row.child(div().flex_none().w(pt(54.0)).h(pt(24.0)).rounded(pt(5.0)).bg(rgba(t.field)).child(c.alpha.clone()));

        // ⑤ 色板：色板集下拉 + 16 pt 色块（节奏 24，一行 9 个，与 Figma 的 216 内容宽一致）。
        let set = c.swatch_set.get();
        let (set_label, set_colors) = if set == 0 { (tr("On this page"), page_colors()) } else { (tr("Recent"), recent_colors()) };
        let set_trigger = c.swatch_anchor.clone();
        let set_hit: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let set_select = div()
            .relative()
            .w_full()
            .h(pt(24.0))
            .pl(pt(8.0))
            .rounded(pt(5.0))
            .border_1()
            .border_color(rgba(t.border))
            .bg(rgba(t.panel))
            .flex()
            .items_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgba(t.hover)))
            .child(div().flex_1().min_w(pt(0.0)).overflow_hidden().text_size(pt(11.0)).text_color(rgba(t.text)).child(set_label))
            .child(picker_icons::paint(PickerIcon::Chevron, rgba(t.icon), 24.0))
            .child(bounds_probe(set_hit.clone()))
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                if let Some(b) = set_hit.get() {
                    set_trigger.set(Some(b));
                    window.refresh();
                }
            });
        let mut grid = div().px(pt(12.0)).flex().flex_col().gap(pt(CHIT_PITCH - CHIT));
        for row in set_colors.chunks(CHITS_PER_ROW) {
            let mut r = div().flex().flex_row().items_center().gap(pt(CHIT_PITCH - CHIT));
            for col in row {
                let p = pick.clone();
                let col = *col;
                r = r.child(
                    crate::widgets::swatch(figcraft_color::Paint::solid(picker_color(col)), CHIT).on_click(move |window, cx| p(col, window, cx)),
                );
            }
            grid = grid.child(r);
        }
        let swatch_panel = div()
            .w(pt(PANEL_W))
            .border_t_1()
            .border_color(rgba(t.border))
            .pt(pt(11.0))
            .child(div().px(pt(16.0)).pb(pt(4.0)).child(set_select))
            .child(grid)
            .child(div().h(pt(12.0)));

        // ⑥ 对比度行：Figma `color_contrast_info` 的圆片 + 比值 + AA 圆标 + 设置下拉。
        //    对比基准是选区快照里的真实背景（父级纯色 fill / 页面背景），不是画布默认色。
        let ratio_label = format!("{aa_ratio:.2} : 1");
        let bg_dot = aa_bg;
        let ratio_canvas = gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let r = f32::from(bounds.size.width).min(f32::from(bounds.size.height)) * 0.5;
                let cx = f32::from(bounds.origin.x) + r;
                let cy = f32::from(bounds.origin.y) + r;
                let bg_rgba = rgba(crate::widgets::swatch::color32(bg_dot.with_alpha(1.0)));
                window.paint_quad(gpui::fill(Bounds { origin: point(px(cx - r), px(cy - r)), size: size(px(r), px(2.0 * r)) }, bg_rgba));
                window.paint_quad(gpui::fill(Bounds { origin: point(px(cx), px(cy - r)), size: size(px(r), px(2.0 * r)) }, rgba(Color32::WHITE)));
            },
        )
        .size(pt(14.0));

        // AA 圆标：未达标带斜杠，点击取白线上最近达标色并应用（复用取色器现有 pick）。
        let aa_pick = pick.clone();
        let aa_candidate = if !aa_pass { aa_min.and_then(|m| figcraft_color::autocorrect_candidate(aa_fg, aa_bg, m, hue)) } else { None };
        let mut aa_trigger = div()
            .h(pt(20.0))
            .px(pt(6.0))
            .rounded(pt(4.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(pt(4.0))
            .bg(rgba(if aa_pass { t.field } else { t.hover }))
            .hover(|s| s.bg(rgba(t.selected_row)))
            .cursor_pointer()
            .child(
                div()
                    .size(pt(12.0))
                    .rounded_full()
                    .border_1()
                    .border_color(rgba(if aa_pass { t.text } else { t.text_secondary }))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if aa_pass { "✓" } else { "/" }),
            )
            .child(div().text_size(pt(11.0)).text_color(rgba(t.text)).child("AA"));
        if let Some(cand) = aa_candidate {
            aa_trigger = aa_trigger.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let [r, g, b, a] = cand.to_rgba8();
                aa_pick(Color32::from_rgba_unmultiplied(r, g, b, a), window, cx);
            });
        }

        // 设置图标：**通用 24×24 图标按钮**（IconButton），点开 AA 分类/等级下拉。
        // 锚点用按钮真实矩形（探针），不是鼠标点位置，菜单才能准确对齐按钮。
        let aa_hit: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let aa_hit_click = aa_hit.clone();
        let aa_anchor = c.aa_anchor.clone();
        let aa_anchor_store = aa_anchor.clone();
        let aa_wake = self.cmds.clone();
        let settings_trigger = with_button_bounds(
            IconButton::new(Icon::Settings).size(24.0).on_click(move |_e, _, _| {
                if let Some(b) = aa_hit_click.get() {
                    *aa_anchor_store.borrow_mut() = Some(b);
                }
                aa_wake.wake();
            }),
            aa_hit,
        );

        let contrast_row = div()
            .w(pt(PANEL_W))
            .h(pt(48.0))
            .px(pt(8.0))
            .flex()
            .items_center()
            .gap(pt(6.0))
            .child(div().size(pt(14.0)).child(ratio_canvas))
            .child(div().flex_1().text_size(pt(11.0)).text_color(rgba(t.text)).child(ratio_label))
            .child(aa_trigger)
            .child(settings_trigger);

        // AA 设置下拉：若本帧锚点为 Some，构造分类 4 项 + 等级 2 项菜单推到浮层 overlays。
        if let Some(rect) = *aa_anchor.borrow() {
            let cat = c.aa_category.clone();
            let lvl = c.aa_level.clone();
            let anchor_close = aa_anchor.clone();
            let wake2 = self.cmds.clone();
            let auto_label: SharedString = format!("自动（{}）", aa_category_label(c.contrast.auto_category)).into();
            let large_label: SharedString = tr("大文本").into();
            let normal_label: SharedString = tr("普通文本").into();
            let graphics_label: SharedString = tr("图形").into();
            let aa_label: SharedString = format!("AA · Essential ({:.1} : 1)", aa_min.unwrap_or(3.0)).into();
            let aaa_label: SharedString = tr("AAA · 仅限文本").into();
            let row_auto = auto_label.clone();
            let row_large = large_label.clone();
            let row_normal = normal_label.clone();
            let row_graphics = graphics_label.clone();
            let row_aa = aa_label.clone();
            let row_aaa = aaa_label.clone();
            let has_aaa = figcraft_color::minimum(aa_category_val, figcraft_color::ContrastLevel::AAA).is_some();
            let on_pick_menu = Rc::new(move |picked: &str, _w: &mut Window, _cx: &mut App| {
                if picked == aaa_label.as_ref() && !has_aaa {
                    *anchor_close.borrow_mut() = None;
                    wake2.wake();
                    return;
                }
                if picked == auto_label.as_ref() {
                    *cat.borrow_mut() = None;
                } else if picked == large_label.as_ref() {
                    *cat.borrow_mut() = Some(figcraft_color::ContrastCategory::LargeText);
                } else if picked == normal_label.as_ref() {
                    *cat.borrow_mut() = Some(figcraft_color::ContrastCategory::NormalText);
                } else if picked == graphics_label.as_ref() {
                    *cat.borrow_mut() = Some(figcraft_color::ContrastCategory::Graphics);
                } else if picked == aa_label.as_ref() {
                    *lvl.borrow_mut() = figcraft_color::ContrastLevel::AA;
                } else if picked == aaa_label.as_ref() {
                    *lvl.borrow_mut() = figcraft_color::ContrastLevel::AAA;
                }
                *anchor_close.borrow_mut() = None;
                wake2.wake();
            });
            let anchor_dismiss = aa_anchor.clone();
            let wake3 = self.cmds.clone();
            let dismiss = Rc::new(move |_: &mut Window, _: &mut App| {
                *anchor_dismiss.borrow_mut() = None;
                wake3.wake();
            });
            let sel_cat = *c.aa_category.borrow();
            let sel_lvl = *c.aa_level.borrow();
            let rows = vec![
                MenuRow { label: row_auto, trail: None, key: None, checked: Some(sel_cat.is_none()), separator_before: false },
                MenuRow {
                    label: row_large,
                    trail: None,
                    key: None,
                    checked: Some(sel_cat == Some(figcraft_color::ContrastCategory::LargeText)),
                    separator_before: false,
                },
                MenuRow {
                    label: row_normal,
                    trail: None,
                    key: None,
                    checked: Some(sel_cat == Some(figcraft_color::ContrastCategory::NormalText)),
                    separator_before: false,
                },
                MenuRow {
                    label: row_graphics,
                    trail: None,
                    key: None,
                    checked: Some(sel_cat == Some(figcraft_color::ContrastCategory::Graphics)),
                    separator_before: false,
                },
                MenuRow {
                    label: row_aa,
                    trail: None,
                    key: None,
                    checked: Some(sel_lvl == figcraft_color::ContrastLevel::AA),
                    separator_before: true,
                },
                MenuRow {
                    label: row_aaa,
                    trail: None,
                    key: None,
                    checked: Some(sel_lvl == figcraft_color::ContrastLevel::AAA),
                    separator_before: false,
                },
            ];
            // 通用菜单 + **宽度自适应**（None，不再硬编码 180）；与按钮**中心对齐**、
            // 顶部在按钮底部下方 4pt（BottomCenter 的默认 gap 就是 4）。
            self.push_overlay(
                crate::widgets::menu::menu_popup_in_aligned(
                    rect,
                    None,
                    rows,
                    on_pick_menu,
                    dismiss.clone(),
                    f32::from(self.viewport.width),
                    f32::from(self.viewport.height),
                    crate::widgets::RectAlign::BottomCenter,
                ),
                dismiss,
            );
        }

        // ── 组装（顺序与 Figma 一致：类型条 → 渐变编辑 / 或纯色编辑）
        // 渐变页只保留「类型行 + 预览条 + 断点列表」，不再带色域/色相/透明度/色板，
        // 否则切换后旧内容全留在上面（用户 2026-11-19）。
        let kind_now = c.paint_state.as_ref().map(|s| s.borrow().kind.clone());
        let is_grad = matches!(kind_now, Some(figcraft_color::PaintKind::Gradient(_)));
        // 顶部不额外留白：类型条紧贴标题栏下方（Figma 取色器内容 257 = 类型条 41 + 渐变编辑 216）。
        // 点卡片内空白（非输入框）时让输入框失焦提交：TextEdit 点自身会 stop_propagation，
        // 所以这里只在点空白时触发 blur（用户反复反馈的「输入完点别处提交不了」）。
        let mut col = div().w(pt(PANEL_W)).flex().flex_col().on_mouse_down(MouseButton::Left, |_e, window, _cx| {
            window.blur();
        });
        if c.paint.is_some() && !c.nested {
            col = col.child(self.picker_toolbar(c));
        }
        if is_grad {
            if let (Some(p), Some(state)) = (&c.paint, &c.paint_state) {
                let grad = match &state.borrow().kind {
                    figcraft_color::PaintKind::Gradient(g) => Some(g.clone()),
                    _ => None,
                };
                if let Some(g) = grad {
                    // 两段各自管自己的内缩：类型行 pl16/pr8，预览条 px16，标题/断点行 px8。
                    col = col.child(self.gradient_kind_row(c, p, state));
                    col = col.child(self.gradient_stops(cx, c, p, state, g, c.grad_sel.clone(), c.current.clone()));
                }
            }
        } else {
            col = col.child(contrast_row);
            col = col.child(div().px(pt(16.0)).child(spectrum));
            col = col.child(
                div()
                    .w(pt(PANEL_W))
                    .h(pt(48.0))
                    .px(pt(16.0))
                    .flex()
                    .items_center()
                    .gap(pt(12.0))
                    .child(
                        div()
                            .flex_none()
                            .w(pt(24.0))
                            .h(pt(24.0))
                            .rounded(pt(5.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(t.hover)))
                            .child(picker_icons::paint(PickerIcon::Dropper, rgba(t.icon), 24.0))
                            .on_mouse_down(MouseButton::Left, move |_, _, _| {
                                eyedrop_cmds.push(Cmd { id: "edit.pickColor", params: Some(json!({ "apply": false })) });
                            }),
                    )
                    .child(div().flex_1().min_w(pt(0.0)).h(pt(48.0)).flex().flex_col().justify_between().child(hue_slider).child(alpha_slider)),
            );
            col = col.child(div().w(pt(PANEL_W)).h(pt(40.0)).px(pt(16.0)).flex().items_center().gap(pt(8.0)).child(format_select).child(fields_row));
            col = col.child(swatch_panel);
        }

        // The model list is the shared dropdown menu (ui-egui's model dropdown), parked on the
        // window root so the popover card cannot clip it.
        if let Some(anchor) = c.model_anchor.get() {
            let rows: Vec<MenuRow> = COLOR_MODELS
                .iter()
                .map(|n| MenuRow { label: (*n).into(), trail: None, checked: Some(*n == model_name), separator_before: false, key: None })
                .collect();
            let set_model = c.model.clone();
            let close = c.model_anchor.clone();
            let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new({
                let close = close.clone();
                move |label, _w, _cx| {
                    if let Some(i) = COLOR_MODELS.iter().position(|n| *n == label) {
                        set_model.set(i);
                    }
                    close.set(None);
                }
            });
            let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_w, _cx| close.set(None));
            self.push_overlay(
                menu_popup_in(anchor, Some(56.0), rows, on_pick, dismiss.clone(), f32::from(self.viewport.width), f32::from(self.viewport.height)),
                dismiss,
            );
        }
        // The blend-mode menu, on the window root like the model menu.
        if let Some(anchor) = c.blend_anchor.get()
            && let (Some(p), Some(state)) = (&c.paint, &c.paint_state)
        {
            let current_blend = state.borrow().blend;
            let rows: Vec<MenuRow> = figcraft_color::BlendMode::ALL
                .iter()
                .map(|m| MenuRow { label: m.label().into(), trail: None, checked: Some(*m == current_blend), separator_before: false, key: None })
                .collect();
            let write = p.on_write.clone();
            let st = state.clone();
            let close = c.blend_anchor.clone();
            let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new({
                let close = close.clone();
                move |label, window, cx| {
                    if let Some(m) = figcraft_color::BlendMode::ALL.iter().find(|m| m.label() == label) {
                        let mut np = st.borrow().clone();
                        np.blend = *m;
                        write(np, window, cx);
                    }
                    close.set(None);
                }
            });
            let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_w, _cx| close.set(None));
            // 混色（水滴）菜单：**左边缘对齐按钮左缘**、顶部在按钮底部下方 4pt
            // （通用 Menu 默认 BottomStart + gap 4），宽度跟随按钮。
            self.push_overlay(
                menu_popup_in(anchor, None, rows, on_pick, dismiss.clone(), f32::from(self.viewport.width), f32::from(self.viewport.height)),
                dismiss,
            );
        }
        // The swatch-set menu (在此页面 / 最近使用), same parking rule.
        if let Some(anchor) = c.swatch_anchor.get() {
            let rows: Vec<MenuRow> = [("On this page", 0u8), ("Recent", 1u8)]
                .into_iter()
                .map(|(name, i)| MenuRow { label: tr(name).into(), trail: None, checked: Some(i == set), separator_before: false, key: None })
                .collect();
            let set_cell = c.swatch_set.clone();
            let close = c.swatch_anchor.clone();
            let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new({
                let close = close.clone();
                move |label, _w, _cx| {
                    let page = tr("On this page");
                    set_cell.set(if label == page { 0 } else { 1 });
                    close.set(None);
                }
            });
            let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_w, _cx| close.set(None));
            self.push_overlay(
                menu_popup_in(anchor, Some(PANEL_W), rows, on_pick, dismiss.clone(), f32::from(self.viewport.width), f32::from(self.viewport.height)),
                dismiss,
            );
        }
        // 渐变类型下拉（线性 / 径向 / 角度 / 菱形）：与其它浮层内菜单一样挂在窗口根。
        if let Some(anchor) = c.grad_kind_anchor.get()
            && let (Some(p), Some(state)) = (&c.paint, &c.paint_state)
        {
            let active = match &state.borrow().kind {
                figcraft_color::PaintKind::Gradient(g) => match g.kind {
                    figcraft_color::GradientKind::Linear => 0,
                    figcraft_color::GradientKind::Radial => 1,
                    figcraft_color::GradientKind::Angular => 2,
                    figcraft_color::GradientKind::Diamond => 3,
                },
                _ => 0,
            };
            let labels = ["Linear", "Radial", "Angular", "Diamond"];
            let rows: Vec<MenuRow> = labels
                .iter()
                .enumerate()
                .map(|(i, l)| MenuRow { label: (*l).into(), trail: None, checked: Some(i == active), separator_before: false, key: None })
                .collect();
            let write = p.on_write.clone();
            let st = state.clone();
            let close = c.grad_kind_anchor.clone();
            let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new({
                let close = close.clone();
                move |label, window, cx| {
                    if let Some(i) = labels.iter().position(|l| *l == label) {
                        let cur = st.borrow().clone();
                        let base = cur.preview_color();
                        let mut np = cur;
                        np.kind = figcraft_color::PaintKind::Gradient(figcraft_color::Gradient::default_for(GRADIENT_KINDS[i.min(3)], base));
                        write(np, window, cx);
                    }
                    close.set(None);
                }
            });
            let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_w, _cx| close.set(None));
            // 与上面的通用 dropdown 按钮（96）保持同一视觉尺度，略放宽到 104 让长项
            // （Angular / Diamond）不贴边。
            self.push_overlay(
                menu_popup_in(anchor, Some(104.0), rows, on_pick, dismiss.clone(), f32::from(self.viewport.width), f32::from(self.viewport.height)),
                dismiss,
            );
        }
        col.into_any_element()
    }

    /// 填充类型条（对齐 EGUI `colorpicker.rs::paint_type_row`）：
    /// 纯色 / 渐变 / 图片 / 图案 四格 24 pt 图标按钮 + 右侧混合模式按钮，底边 1 px 分隔。
    fn picker_toolbar(&self, c: &ColorState) -> gpui::Div {
        let t = tokens();
        let kind_idx = match c.paint_state.as_ref().map(|s| s.borrow().kind.clone()) {
            Some(figcraft_color::PaintKind::Solid { .. }) => 0usize,
            Some(figcraft_color::PaintKind::Gradient(_)) => 1,
            Some(figcraft_color::PaintKind::Image(_)) => 2,
            Some(figcraft_color::PaintKind::Pattern(_)) => 3,
            _ => 0,
        };
        let write = c.paint.as_ref().map(|p| p.on_write.clone());
        let state = c.paint_state.clone();
        let grad_kind = c.grad_kind.clone();
        let mut tabs = div().flex().items_center().gap(pt(4.0));
        let icons = [
            (0usize, PickerIcon::Solid, None),
            (1usize, PickerIcon::Gradient, None),
            (2usize, PickerIcon::Solid, Some(Icon::Image)),
            (3usize, PickerIcon::Solid, Some(Icon::ViewGrid)),
        ];
        for (i, picker_icon, icon) in icons {
            let on = i == kind_idx;
            let mut b =
                div().w(pt(24.0)).h(pt(24.0)).rounded(pt(5.0)).flex().items_center().justify_center().cursor_pointer().hover(|s| s.bg(rgba(t.hover)));
            b = match icon {
                Some(icon) => b.child(crate::widgets::icon::paint(icon, rgba(t.icon), 24.0)),
                None => b.child(picker_icons::paint(picker_icon, rgba(t.icon), 24.0)),
            };
            if on {
                b = b.bg(rgba(t.hover));
            }
            let write_b = write.clone();
            let state_b = state.clone();
            let gk = grad_kind.clone();
            b = b.on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let (Some(w), Some(st)) = (write_b.as_ref(), state_b.as_ref()) else { return };
                let cur = st.borrow().clone();
                let base = cur.preview_color();
                let mut np = cur;
                np.kind = match i {
                    0 => figcraft_color::PaintKind::Solid { color: base },
                    1 => figcraft_color::PaintKind::Gradient(figcraft_color::Gradient::default_for(GRADIENT_KINDS[gk.get().min(3)], base)),
                    2 => figcraft_color::Paint::image("", figcraft_color::ImageScaleMode::Fill).kind,
                    _ => figcraft_color::PaintKind::Pattern(figcraft_color::PatternPaint::new(0)),
                };
                w(np, window, cx);
            });
            tabs = tabs.child(b);
        }
        // 混合模式：水滴按钮，菜单挂窗口根（与数值行 / 色板下拉同一套锚点）。
        let blend_trigger = c.blend_anchor.clone();
        let blend_hit: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let blend_btn = div()
            .relative()
            .w(pt(24.0))
            .h(pt(24.0))
            .rounded(pt(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|s| s.bg(rgba(t.hover)))
            .child(picker_icons::paint(PickerIcon::Blend, rgba(t.icon), 24.0))
            .child(bounds_probe(blend_hit.clone()))
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                if let Some(b) = blend_hit.get() {
                    blend_trigger.set(Some(b));
                    window.refresh();
                }
            });
        div()
            .w(pt(PANEL_W))
            .h(pt(40.0))
            .px(pt(8.0))
            .border_b_1()
            .border_color(rgba(t.border))
            .flex()
            .items_center()
            .justify_between()
            .child(tabs)
            .child(div().flex().items_center().gap(pt(4.0)).child(blend_btn))
    }

    /// 渐变类型：线性 / 径向 / 角度 / 菱形。Figma 的渐变面板用**下拉菜单**切类型，
    /// 右侧两枚 24 pt 按钮分别是「交换」和「旋转 90°」。
    fn gradient_kind_row(&self, c: &ColorState, p: &PaintPicker, state: &Rc<RefCell<figcraft_color::Paint>>) -> gpui::Div {
        let labels = ["Linear", "Radial", "Angular", "Diamond"];
        let active = match &state.borrow().kind {
            figcraft_color::PaintKind::Gradient(g) => match g.kind {
                figcraft_color::GradientKind::Linear => 0,
                figcraft_color::GradientKind::Radial => 1,
                figcraft_color::GradientKind::Angular => 2,
                figcraft_color::GradientKind::Diamond => 3,
            },
            _ => 0,
        };
        let anchor = c.grad_kind_anchor.clone();
        let dropdown = dropdown::Dropdown::new(labels[active], 96.0, None).on_click(move |rect: Bounds<Pixels>, window, _cx| {
            anchor.set(Some(rect));
            window.refresh();
        });
        let write = p.on_write.clone();
        // 交换：两端 stop 互换（位置镜像），与 Figma 的「翻转渐变」一致。
        let swap = IconButton::new(Icon::FlipH).size(24.0).on_click({
            let write = write.clone();
            let st = state.clone();
            move |_, window, cx| {
                let mut np = st.borrow().clone();
                if let figcraft_color::PaintKind::Gradient(g) = &mut np.kind {
                    for s in &mut g.stops {
                        s.position = 1.0 - s.position;
                    }
                }
                write(np, window, cx);
            }
        });
        // 旋转：把渐变主轴旋转 90°。
        let rotate = IconButton::new(Icon::Rotate).size(24.0).on_click({
            let write = write.clone();
            let st = state.clone();
            move |_, window, cx| {
                let mut np = st.borrow().clone();
                if let figcraft_color::PaintKind::Gradient(g) = &mut np.kind {
                    let (fx, fy) = g.from;
                    let rot = |(x, y): (f64, f64)| (fx - (y - fy), fy + (x - fx));
                    g.to = rot(g.to);
                    g.width = rot(g.width);
                }
                write(np, window, cx);
            }
        });
        div()
            .w(pt(PANEL_W))
            .h(pt(48.0))
            .pl(pt(16.0))
            .pr(pt(8.0))
            .flex()
            .items_center()
            .gap(pt(4.0))
            .child(dropdown)
            .child(div().flex_1())
            .child(swap)
            .child(rotate)
    }

    /// 渐变预览条 + 断点手柄 + 「断点」标题行 + 断点列表（百分比 / 颜色 / 减号）。
    /// 点击预览条添加断点、拖动手柄改位置、点击手柄或颜色控件弹出嵌套取色器。
    fn gradient_stops(
        &self,
        cx: &mut App,
        c: &ColorState,
        p: &PaintPicker,
        state: &Rc<RefCell<figcraft_color::Paint>>,
        g: figcraft_color::Gradient,
        sel: Rc<Cell<usize>>,
        current: Rc<Cell<Color32>>,
    ) -> gpui::Div {
        let t = tokens();
        let write = p.on_write.clone();
        let bounds = c.grad_bounds.clone();
        let drag = c.grad_drag.clone();
        let press = c.grad_press.clone();
        let moved = c.grad_moved.clone();
        let sub = c.sub.clone();
        let cmds = self.cmds.clone();

        // 断点行的通用输入控件（百分比 / Hex / 透明度）。每帧重建元素树，实体必须跨帧
        // 复用；断点数量变化时整体重建，避免闭包里捕获的索引错位。
        let fields: Vec<StopFields> = {
            let mut cache = c.stop_fields.borrow_mut();
            if cache.len() != g.stops.len() {
                cache.clear();
                c.stop_shown.borrow_mut().clear();
                for (i, s) in g.stops.iter().enumerate() {
                    let hex = format!(
                        "#{:02X}{:02X}{:02X}",
                        (s.color.r * 255.0).round() as u8,
                        (s.color.g * 255.0).round() as u8,
                        (s.color.b * 255.0).round() as u8
                    );
                    let shown = (format!("{}", (s.position * 100.0).round() as i64), hex.clone(), format!("{}", (s.color.a * 100.0).round() as i64));
                    cache.push(stop_fields(cx, state, &write, i, &shown));
                    c.stop_shown.borrow_mut().push(shown);
                }
            }
            cache.clone()
        };
        // 外部改动（拖手柄 / 拖动色阶）后把数值同步回输入框；只在真的变了才写，避免打字被重置。
        {
            let mut shown = c.stop_shown.borrow_mut();
            for (i, s) in g.stops.iter().enumerate() {
                let hex = format!(
                    "#{:02X}{:02X}{:02X}",
                    (s.color.r * 255.0).round() as u8,
                    (s.color.g * 255.0).round() as u8,
                    (s.color.b * 255.0).round() as u8
                );
                let now = (format!("{}", (s.position * 100.0).round() as i64), hex, format!("{}", (s.color.a * 100.0).round() as i64));
                let Some(f) = fields.get(i) else { continue };
                if shown.get(i) == Some(&now) {
                    continue;
                }
                f.pct.update(cx, |tf, _| tf.set_value(now.0.clone()));
                f.hex.update(cx, |te, _| te.set_value(now.1.clone()));
                f.opacity.update(cx, |te, _| te.set_value(now.2.clone()));
                if let Some(slot) = shown.get_mut(i) {
                    *slot = now;
                }
            }
        }

        // ① 预览条 208×32：底板棋盘格 + 断点之间真正的两色线性渐变（不是 24 段近似色带）。
        let ramp_g = g.clone();
        let ramp_bounds = bounds.clone();
        // canvas 直绘不受父级圆角裁剪，圆角靠绘制末尾用面板底色补四角（见下）。
        let ramp_panel_bg = rgba(t.panel);
        let ramp = div().absolute().left(px(0.0)).top(px(16.0)).w(pt(208.0)).h(pt(32.0)).rounded(pt(5.0)).overflow_hidden().child(
            gpui::canvas(
                move |b, _, _| ramp_bounds.set(Some(b)),
                move |b, _, window, _| {
                    let w = f32::from(b.size.width).max(1.0);
                    let h = f32::from(b.size.height).max(1.0);
                    // 透明色需要棋盘格垫底，否则看起来像纯色。
                    let light = gpui::Hsla::from(rgba(Color32::from_rgb(255, 255, 255)));
                    let dark = gpui::Hsla::from(rgba(Color32::from_rgb(0xCC, 0xCC, 0xCC)));
                    let cell: f32 = 8.0;
                    let mut y = 0.0;
                    let mut cy = 0;
                    while y < h {
                        let mut x = 0.0;
                        let mut cx_ = 0;
                        while x < w {
                            let quad = Bounds {
                                origin: point(px(f32::from(b.origin.x) + x), px(f32::from(b.origin.y) + y)),
                                size: size(px((cell).min(w - x)), px((cell).min(h - y))),
                            };
                            window.paint_quad(gpui::fill(quad, if (cx_ + cy) % 2 == 0 { light } else { dark }));
                            x += cell;
                            cx_ += 1;
                        }
                        y += cell;
                        cy += 1;
                    }
                    let mut ss = ramp_g.stops.clone();
                    ss.sort_by(|a, b| a.position.partial_cmp(&b.position).unwrap_or(std::cmp::Ordering::Equal));
                    let Some(first) = ss.first() else { return };
                    let last = ss.last().unwrap();
                    let stop_hsla = |c: figcraft_color::Color| gpui::Hsla::from(rgba(picker_c32(c)));
                    // 首断点之前 / 末断点之后是纯色。
                    let x0 = first.position.clamp(0.0, 1.0) * w;
                    if x0 > 0.0 {
                        window.paint_quad(gpui::fill(Bounds { origin: b.origin, size: size(px(x0), b.size.height) }, stop_hsla(first.color)));
                    }
                    for pair in ss.windows(2) {
                        let (a, c) = (&pair[0], &pair[1]);
                        let xa = a.position.clamp(0.0, 1.0) * w;
                        let xb = c.position.clamp(0.0, 1.0) * w;
                        if xb <= xa {
                            continue;
                        }
                        let quad = Bounds { origin: point(px(f32::from(b.origin.x) + xa), b.origin.y), size: size(px(xb - xa), b.size.height) };
                        window.paint_quad(gpui::fill(
                            quad,
                            gpui::linear_gradient(
                                90.0,
                                gpui::linear_color_stop(stop_hsla(a.color), 0.0),
                                gpui::linear_color_stop(stop_hsla(c.color), 1.0),
                            ),
                        ));
                    }
                    let xe = last.position.clamp(0.0, 1.0) * w;
                    if xe < w {
                        window.paint_quad(gpui::fill(
                            Bounds { origin: point(px(f32::from(b.origin.x) + xe), b.origin.y), size: size(px(w - xe), b.size.height) },
                            stop_hsla(last.color),
                        ));
                    }
                    // 四角 rx5 圆角：canvas 直绘不被父级 overflow 圆角裁剪，用面板底色
                    // 盖掉四个直角三角（弧形斜边近似 1/4 圆）。
                    let bx = f32::from(b.origin.x);
                    let by = f32::from(b.origin.y);
                    let bw = f32::from(b.size.width);
                    let bh = f32::from(b.size.height);
                    let rr = 5.0_f32;
                    let corners = [
                        [(bx, by), (bx + rr, by), (bx, by + rr)],
                        [(bx + bw, by), (bx + bw - rr, by), (bx + bw, by + rr)],
                        [(bx + bw, by + bh), (bx + bw - rr, by + bh), (bx + bw, by + bh - rr)],
                        [(bx, by + bh), (bx + rr, by + bh), (bx, by + bh - rr)],
                    ];
                    for c in corners {
                        let mut pb = gpui::PathBuilder::fill();
                        pb.move_to(point(px(c[0].0), px(c[0].1)));
                        pb.line_to(point(px(c[1].0), px(c[1].1)));
                        pb.curve_to(point(px(c[2].0), px(c[2].1)), point(px(c[0].0), px(c[0].1)));
                        pb.close();
                        if let Ok(path) = pb.build() {
                            window.paint_path(path, ramp_panel_bg);
                        }
                    }
                },
            )
            .size_full(),
        );
        // 点预览条 = 在该位置插入断点（颜色取当前位置的采样色）。
        let ramp = {
            let write = write.clone();
            let state = state.clone();
            let sel = sel.clone();
            let bounds = bounds.clone();
            let cur = current.clone();
            ramp.on_mouse_down(MouseButton::Left, move |e: &gpui::MouseDownEvent, window, cx| {
                let Some(b) = bounds.get() else { return };
                let local_x = f32::from(e.position.x) - f32::from(b.origin.x);
                let pos = (local_x / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
                let mut np = state.borrow().clone();
                if let figcraft_color::PaintKind::Gradient(gg) = &mut np.kind {
                    // 兜底：点在已有断点手柄的热区（±16pt）内就不新建——否则拖手柄时
                    // 事件漏到滑条上会多插一个点（用户实测）。拖动交给手柄自己的逻辑。
                    if gg.stops.iter().any(|s| (s.position as f32 * f32::from(b.size.width) - local_x).abs() <= 16.0) {
                        return;
                    }
                    let color = gg.sample(pos);
                    gg.stops.push(figcraft_color::ColorStop { position: pos, color });
                    sel.set(gg.stops.len() - 1);
                    cur.set(picker_c32(color));
                }
                write(np, window, cx);
            })
        };

        // 显示顺序按位置升序（与 Figma 一致）；存储顺序保持稳定，闭包仍用原索引。
        let mut order: Vec<usize> = (0..g.stops.len()).collect();
        order.sort_by(|&a, &b| g.stops[a].position.partial_cmp(&g.stops[b].position).unwrap_or(std::cmp::Ordering::Equal));

        let mut area = div().relative().w(pt(208.0)).h(pt(48.0)).child(ramp);
        for &i in &order {
            let s = &g.stops[i];
            let x = (s.position as f32 * 208.0 - 16.0).clamp(-16.0, 192.0);
            let sel_c = sel.clone();
            let drag_c = drag.clone();
            let press_c = press.clone();
            let moved_c = moved.clone();
            let cur_c = current.clone();
            let stop_color = s.color;
            let on = i == sel.get();
            // 手柄 = Figma 的「水滴 tag」：**一条连续 path**（顶部四角 rx5 圆角矩形 +
            // 底部收成三角尖，尖端正对 32pt 渐变条中心线）。选中时**整个手柄**填充主题色，
            // 未选中填充白色；中心叠一枚通用方形颜色按钮。早先拆成「圆角矩形 + 三角」两块，
            // 接缝处不连续、也不像箭头（用户指出）。
            let tag_color = if on { rgba(t.accent) } else { rgba(Color32::WHITE) };
            // 手柄按用户给的 32×38 SVG 精确画：主体是 24×24 圆角矩形（x=4..28, y=0..24,
            // rx5），底部 12 宽三角（x=10..22）收到尖 (16,30)。整个 32×38 就是热区，
            // 拖手柄不会漏到滑条上误插断点。
            let tag = gpui::canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let x0 = f32::from(b.origin.x);
                    let y0 = f32::from(b.origin.y);
                    let build = |pb: &mut gpui::PathBuilder, dy: f32| {
                        let at = |x: f32, y: f32| point(px(x0 + x), px(y0 + y + dy));
                        pb.move_to(at(23.0, 0.0));
                        pb.line_to(at(9.0, 0.0));
                        pb.cubic_bezier_to(at(4.0, 5.0), at(6.23858, 0.0), at(4.0, 2.23858));
                        pb.line_to(at(4.0, 19.0));
                        pb.cubic_bezier_to(at(9.0, 24.0), at(4.0, 21.7614), at(6.23858, 24.0));
                        pb.line_to(at(10.0, 24.0));
                        pb.line_to(at(16.0, 30.0));
                        pb.line_to(at(22.0, 24.0));
                        pb.line_to(at(23.0, 24.0));
                        pb.cubic_bezier_to(at(28.0, 19.0), at(25.7614, 24.0), at(28.0, 21.7614));
                        pb.line_to(at(28.0, 5.0));
                        pb.cubic_bezier_to(at(23.0, 0.0), at(28.0, 2.23858), at(25.7614, 0.0));
                        pb.close();
                    };
                    // SVG filter0_d：dy=4、25% 黑投影，先阴影再本体。
                    let mut sp = gpui::PathBuilder::fill();
                    build(&mut sp, 4.0);
                    if let Ok(path) = sp.build() {
                        window.paint_path(path, rgba(Color32::from_black_alpha(64)));
                    }
                    let mut pb = gpui::PathBuilder::fill();
                    build(&mut pb, 0.0);
                    if let Ok(path) = pb.build() {
                        window.paint_path(path, tag_color);
                    }
                },
            )
            .absolute()
            .left(px(0.0))
            .top(px(0.0))
            .w(pt(32.0))
            .h(pt(38.0));
            let mut handle = div()
                .absolute()
                .left(px(x))
                .top(pt(2.0))
                .w(pt(32.0))
                .h(pt(38.0))
                .cursor_pointer()
                .child(tag)
                // 中心：通用方形颜色按钮（14×14，位于 tag 主体中心）。
                .child(
                    div()
                        .absolute()
                        .left(pt(9.0))
                        .top(pt(5.0))
                        .w(pt(14.0))
                        .h(pt(14.0))
                        .child(crate::widgets::swatch(figcraft_color::Paint::solid(stop_color), 14.0)),
                )
                .on_mouse_down(MouseButton::Left, move |e: &gpui::MouseDownEvent, _, _| {
                    sel_c.set(i);
                    drag_c.set(Some(i));
                    press_c.set(Some(e.position));
                    moved_c.set(false);
                    cur_c.set(picker_c32(stop_color));
                });
            area = area.child(handle);
        }
        // 拖动改位置；没拖动就松开 = 点断点 → 弹出嵌套取色器。
        // 监听必须挂在窗口级：指针一旦移出色条范围，元素级 move/up 就收不到，
        // 手柄会卡在拖动状态（用户实测「色块拖不动 / 放不下」）。
        {
            let state_move = state.clone();
            let write_move = write.clone();
            let drag_move = drag.clone();
            let press_move = press.clone();
            let moved_move = moved.clone();
            let bounds_move = bounds.clone();
            let state_up = state.clone();
            let drag_up = drag.clone();
            let press_up = press.clone();
            let moved_up = moved.clone();
            let sub_up = sub.clone();
            let cmds_up = cmds.clone();
            let write_up = write.clone();
            area = area.child(
                gpui::canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let m_state = state_move.clone();
                        let m_write = write_move.clone();
                        let m_drag = drag_move.clone();
                        let m_press = press_move.clone();
                        let m_moved = moved_move.clone();
                        let m_bounds = bounds_move.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                            if !phase.bubble() {
                                return;
                            }
                            if let Some(start) = m_press.get() {
                                let dx = f32::from(e.position.x) - f32::from(start.x);
                                let dy = f32::from(e.position.y) - f32::from(start.y);
                                if dx.abs() + dy.abs() > 2.0 {
                                    m_moved.set(true);
                                }
                            }
                            let Some(i) = m_drag.get() else { return };
                            let Some(b) = m_bounds.get() else { return };
                            let x = ((f32::from(e.position.x) - f32::from(b.origin.x)) / f32::from(b.size.width).max(1.0)).clamp(0.0, 1.0);
                            let mut np = m_state.borrow().clone();
                            if let figcraft_color::PaintKind::Gradient(gg) = &mut np.kind
                                && let Some(st) = gg.stops.get_mut(i)
                            {
                                st.position = x;
                            }
                            m_write(np, window, cx);
                        });
                        let u_state = state_up.clone();
                        let u_write = write_up.clone();
                        let u_drag = drag_up.clone();
                        let u_press = press_up.clone();
                        let u_moved = moved_up.clone();
                        let u_sub = sub_up.clone();
                        let u_cmds = cmds_up.clone();
                        window.on_mouse_event(move |e: &gpui::MouseUpEvent, phase, window, cx| {
                            if !phase.bubble() {
                                return;
                            }
                            let i = u_drag.get();
                            if i.is_none() {
                                return;
                            }
                            u_drag.set(None);
                            u_press.set(None);
                            if u_moved.replace(false) {
                                return;
                            }
                            let Some(i) = i else { return };
                            let color = match &u_state.borrow().kind {
                                figcraft_color::PaintKind::Gradient(gg) => gg.stops.get(i).map(|s| s.color),
                                _ => None,
                            };
                            let Some(color) = color else { return };
                            open_stop_color(u_sub.clone(), u_state.clone(), u_write.clone(), i, color, e.position, u_cmds.clone(), window, cx);
                        });
                    },
                )
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .size_full(),
            );
        }

        // ② 「断点」标题行：标题 + 右侧加号（添加断点）。
        let header = {
            let write_add = write.clone();
            let state_add = state.clone();
            let sel_add = sel.clone();
            let cur_add = current.clone();
            let plus = IconButton::new(Icon::Plus).size(24.0).on_click(move |_, window, cx| {
                let mut np = state_add.borrow().clone();
                if let figcraft_color::PaintKind::Gradient(gg) = &mut np.kind {
                    // 在最大空档的中点插入，避免和已有断点重合。
                    let mut ps: Vec<f32> = gg.stops.iter().map(|s| s.position).collect();
                    ps.push(0.0);
                    ps.push(1.0);
                    ps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let mut gap = 0.0f32;
                    let mut pos = 0.5f32;
                    for w in ps.windows(2) {
                        let d = w[1] - w[0];
                        if d > gap {
                            gap = d;
                            pos = (w[0] + w[1]) * 0.5;
                        }
                    }
                    let color = gg.sample(pos);
                    gg.stops.push(figcraft_color::ColorStop { position: pos, color });
                    sel_add.set(gg.stops.len() - 1);
                    cur_add.set(picker_c32(color));
                }
                write_add(np, window, cx);
            });
            div()
                .w(pt(PANEL_W))
                .h(pt(32.0))
                .mt(pt(8.0))
                .flex()
                .items_center()
                .pl(pt(16.0))
                .pr(pt(8.0))
                .child(div().flex_1().text_size(pt(11.0)).text_color(rgba(t.text)).child(tr("Stops")))
                .child(plus)
        };

        // ③ 断点列表：每行 32，左百分比输入、中颜色控件（圆点 + Hex + 透明度输入）、右减号。
        let mut rows = div().flex().flex_col();
        for (i, s) in g.stops.iter().enumerate() {
            let on = i == sel.get();
            let sub_c = sub.clone();
            let state_c = state.clone();
            let write_c = write.clone();
            let cmds_c = cmds.clone();
            let sel_c = sel.clone();
            let cur_c = current.clone();
            let stop_color = s.color;
            let hex = format!(
                "#{:02X}{:02X}{:02X}",
                (s.color.r * 255.0).round() as u8,
                (s.color.g * 255.0).round() as u8,
                (s.color.b * 255.0).round() as u8
            );
            let (pct_el, hex_ed, op_ed) = match fields.get(i).cloned() {
                Some(f) => (f.pct.clone(), f.hex.clone(), f.opacity.clone()),
                None => {
                    let pct = number_field_ranged(cx, 39.0, None, "%", 1.0, Slot::None, Some(0.0), Some(100.0));
                    let (h, o) = crate::widgets::ColorField::color_row_editors(cx, &hex, 100, |_, _, _| {}, |_, _, _| {});
                    (pct, h, o)
                }
            };
            // 行首百分比：通用数字输入框（0–100，提交写回 stop.position）。
            // 中间：通用颜色行 `ColorField`（16 pt 色块 + Hex 编辑器 + 透明度数值 / 「%」单元），
            // 不再自己拼 TextField / number_field；色块点击弹出嵌套取色器。
            let cf = crate::widgets::ColorField::new(
                cx,
                figcraft_color::Paint::solid(stop_color),
                crate::widgets::color_field::ColorText::Hex(SharedString::from(hex)),
                136.0,
                hex_ed,
                op_ed,
            )
            .on_swatch(move |pos: Point<Pixels>, window, cx| {
                sel_c.set(i);
                cur_c.set(picker_c32(stop_color));
                open_stop_color(sub_c.clone(), state_c.clone(), write_c.clone(), i, stop_color, pos, cmds_c.clone(), window, cx);
            });
            let minus = IconButton::new(Icon::Minus).size(24.0).on_click({
                let state_m = state.clone();
                let write_m = write.clone();
                move |_, window, cx| {
                    let mut np = state_m.borrow().clone();
                    if let figcraft_color::PaintKind::Gradient(gg) = &mut np.kind
                        && gg.stops.len() > 2
                    {
                        gg.stops.remove(i.min(gg.stops.len() - 1));
                    }
                    write_m(np, window, cx);
                }
            });
            let mut row = div()
                .h(pt(32.0))
                .w(pt(PANEL_W))
                .flex()
                .items_center()
                .gap(pt(8.0))
                .pl(pt(8.0))
                .pr(pt(8.0))
                .child(div().flex_none().w(pt(40.0)).flex().items_center().child(pct_el))
                .child(div().flex_1().min_w(pt(0.0)).child(cf))
                .child(minus);
            if on {
                row = row.bg(rgba(t.selected_row));
            }
            rows = rows.child(row);
        }

        div()
            .w(pt(PANEL_W))
            .flex()
            .flex_col()
            // 手柄盒顶与编辑区顶部对齐（spec：盒顶 y=129），预览条在盒内 +16；
            // 这里不再额外加顶部 margin，否则整条会再往下偏 16。
            .child(div().w(pt(PANEL_W)).h(pt(48.0)).pl(pt(16.0)).child(area))
            .child(header)
            .child(rows)
            .child(div().h(pt(12.0)))
    }

    fn list_element(&self, cx: &mut App, body_h: f32) -> AnyElement {
        let t = tokens();
        // 字体窗按 Figma `fontPicker*`：卡片内边距 0，搜索行 40（左 16 / 右 12 / 底 1px 线），
        // 行高 28、左 12 / 右 18，**列表自身滚动**（`overflow-y:scroll`，`flex-grow:1`）。
        // 其它列表窗（变量 / 样式）沿用原来的 8pt 内缩与 24pt 行，只加一个高度上限 + 滚动。
        let fonts = self.kind == WindowKind::FontPicker;
        let mut col = div().w_full().flex().flex_col();
        if fonts {
            // Figma 的 body 是固定 429px：搜索行与列表一起撑满，列表吃剩余高度。
            col = col.h(pt(body_h));
        }
        if let Some(search) = &self.search {
            let mut row = div().w_full().flex().items_center();
            row =
                if fonts { row.h(pt(40.0)).pl(pt(16.0)).pr(pt(12.0)).border_b_1().border_color(rgba(t.border)) } else { row.h(pt(32.0)).pb(pt(4.0)) };
            col = col.child(row.child(search.clone()));
        }
        let mut list = div().id("window-list").w_full().flex().flex_col().overflow_y_scroll();
        list = if fonts {
            list.flex_1().min_h(px(0.0))
        } else {
            // 其它列表：内容多时滚动，内容少时仍 hug。上限必须是**常量**——若用 `body_h`
            // 派生，`measured_h` 会把它连同卡片高度一起每帧撑大（同样的「越滚越大」）。
            list.max_h(px(LIST_MAX_H))
        };
        if !fonts {
            list = list.gap(pt(2.0));
        }
        let q = self.query.borrow().trim().to_lowercase();
        let mut shown = 0usize;
        let mut last_group: Option<SharedString> = None;
        for row in &self.rows {
            if !q.is_empty() && !row.label.to_lowercase().contains(&q) {
                continue;
            }
            if row.group != last_group {
                if let Some(g) = &row.group {
                    list = list.child(div().mt(pt(4.0)).text_size(pt(9.0)).text_color(rgba(t.text_secondary)).child(g.clone()));
                }
                last_group = row.group.clone();
            }
            let cmds = self.cmds.clone();
            let cmd = row.cmd;
            let params = row.params.clone();
            let trail = row.trail.clone();
            let font_family = row.font_family.clone();
            // A font-picker row renders its own name in that family — the item is its own preview.
            // Figma `fontPickerItemRow` 的正文是 11px。
            let label_el = if let Some(fam) = font_family {
                div().flex_1().min_w(pt(0.0)).overflow_hidden().text_size(pt(11.0)).font_family(SharedString::from(fam)).child(row.label.clone())
            } else {
                div().flex_1().min_w(pt(0.0)).overflow_hidden().child(row.label.clone())
            };
            let row_h = if row.font_family.is_some() { pt(28.0) } else { pt(24.0) };
            // 字体行：`padding: 0 18px 0 12px`、悬停用面板 hover（不是菜单 hover）。
            let mut row_el =
                div().h(row_h).w_full().flex().flex_row().items_center().text_size(pt(11.0)).on_mouse_down(MouseButton::Left, move |_, _, _| {
                    cmds.push(Cmd { id: cmd, params: Some(params.clone()) });
                });
            row_el = if fonts {
                row_el.pl(pt(12.0)).pr(pt(18.0)).text_color(rgba(t.text)).hover(|s| s.bg(rgba(t.hover)))
            } else {
                row_el.px(pt(8.0)).rounded(pt(4.0)).text_color(rgba(t.menu_text)).hover(|s| s.bg(rgba(t.menu_hover)))
            };
            list = list.child(
                row_el.child(label_el).children(trail.map(|tr| div().text_color(rgba(if fonts { t.text_secondary } else { t.menu_dim })).child(tr))),
            );
            shown += 1;
        }
        if shown == 0 {
            list = list.child(
                div()
                    .py(pt(12.0))
                    .px(pt(12.0))
                    .text_size(pt(11.0))
                    .text_color(rgba(if fonts { t.text_secondary } else { t.menu_dim }))
                    .child(if self.rows.is_empty() { "No items yet." } else { "No matching items" }),
            );
        }
        col = col.child(list);
        let _ = cx;
        col.into_any_element()
    }

    fn settings_element(&self, cx: &mut App) -> AnyElement {
        let t = tokens();
        // Type settings 卡片 pad 为了让标题栏与 X 垂直居中改成 8；正文这里补左右各 4，
        // 把视觉内边距补回设计的 12（Layout / Stroke 仍是 pad 12，不加）。
        // body 插槽里的项间距统一由 `popover_body` 给（8pt），不在每个弹窗各写一个 gap。
        // TypeSettings 的 pad 少 4，这里补回左右各 4 的视觉内边距。
        let mut col = if self.kind == WindowKind::TypeSettings {
            crate::widgets::popover::popover_body().px(pt(4.0))
        } else if self.kind == WindowKind::StyleEditor {
            // lizard 4 个样式编辑器用 `ui.spacing_mut().item_spacing.y = 0`，各段之间是精确的
            // add_space(16/8/24/8/10/20)，属性行之间更是 0 间距。通用 popover_body 的 8pt gap
            // 会和每行自带的 margin 叠加，必须换成 gap=0 的容器，由 `Setting::Spacer` 逐段控制。
            div().w_full().flex().flex_col()
        } else {
            crate::widgets::popover::popover_body()
        };
        // Type settings 的页签已画在标题栏插槽里；Layout / Stroke 仍沿用正文顶部的页签行。
        if self.kind != WindowKind::TypeSettings {
            if let Some((names, active)) = &self.tabs {
                let tab = *active.borrow();
                let a = active.clone();
                let cmds = self.cmds.clone();
                // The stroke popover's Basic / Brush switch is also the stroke's *type*: Basic
                // clears the brush, Brush puts one back. Other popovers only switch pages.
                let is_stroke = self.kind == WindowKind::Stroke;
                let items = names.iter().map(|n| SegItem::Text(n.clone())).collect();
                let seg = Segmented::new(items).active(Some(tab as usize)).on_pick(move |i, _, _| {
                    *a.borrow_mut() = i as u8;
                    if is_stroke {
                        let preset = if i == 0 {
                            Value::Null
                        } else {
                            figcraft_abr::infos().first().map(|b| serde_json::json!(b.id.clone())).unwrap_or(Value::Null)
                        };
                        cmds.push(Cmd { id: "stroke.brush", params: Some(serde_json::json!({ "preset": preset })) });
                    }
                });
                col = col.child(seg);
            }
        }
        let tab = self.tabs.as_ref().map(|(_, a)| *a.borrow()).unwrap_or(0);
        let list = self.settings.borrow();
        for setting in list.iter() {
            if let Some(el) = self.render_setting(cx, setting, tab) {
                col = col.child(el);
            }
        }
        let _ = cx;
        // Details 正文很长（六个分组 + 十几条开关），限高滚动；Basics 短，不触发滚动条。
        // `overflow_y_scroll` 属于 `StatefulInteractiveElement`，需要一个元素 id，且调用后类型
        // 变成 `Stateful<Div>`，不能再赋回 `Div` 的 `col`，故在这里单独收尾。
        if self.kind == WindowKind::TypeSettings {
            col.id("type-settings-scroll").max_h(pt(468.0)).overflow_y_scroll().into_any_element()
        } else {
            col.into_any_element()
        }
    }

    /// 渲染设置窗口的一行；`ExpandedRows` 会递归调用它画被 `>` 展开的子行。
    fn render_setting(&self, cx: &mut App, setting: &Setting, tab: u8) -> Option<AnyElement> {
        let t = tokens();
        let type_ui = self.kind == WindowKind::TypeSettings;
        match setting {
            Setting::Spacer { tab: st, h } => {
                if *st != tab {
                    return None;
                }
                // 精确留白（lizard `ui.add_space(x)`）：gap=0 容器里唯一的行距来源。
                Some(div().w_full().h(pt(*h)).flex_none().into_any_element())
            }
            Setting::Caption { tab: st, text } => {
                if *st != tab {
                    return None;
                }
                Some(div().w_full().pt(pt(6.0)).pb(pt(2.0)).text_size(pt(10.0)).text_color(rgba(t.text)).child(text.clone()).into_any_element())
            }
            Setting::GroupTitle { tab: st, text } => {
                if *st != tab {
                    return None;
                }
                Some(
                    div()
                        .w_full()
                        .pt(pt(8.0))
                        .pb(pt(4.0))
                        .text_size(pt(10.5))
                        .font_weight(crate::panels::UI_FONT_REGULAR)
                        .text_color(rgba(t.text))
                        .child(text.clone())
                        .into_any_element(),
                )
            }
            Setting::Divider { tab: st } => {
                if *st != tab {
                    return None;
                }
                Some(div().w_full().h(px(1.0)).my(pt(4.0)).bg(rgba(t.border)).into_any_element())
            }
            Setting::Preview { text } => {
                // 预览框为 Basics / Details 共用；Variable 页只画可变字体轴。
                if tab == 2 {
                    return None;
                }
                Some(
                    div()
                        .w_full()
                        .h(pt(120.0))
                        .rounded(pt(5.0))
                        .bg(rgba(t.hover))
                        .flex()
                        .items_start()
                        .p(pt(16.0))
                        .text_size(pt(11.0))
                        .text_color(rgba(t.text_secondary))
                        .child(text.clone())
                        .into_any_element(),
                )
            }
            Setting::Swatch { tab: st, label, paint, index } => {
                if *st != tab {
                    return None;
                }
                // lizard `property_row`：行高 `ROW_H = 32`，色板 14×14 居中在 `x + 12`，
                // 文字从 `x + 26` 起、11 pt。
                let mut row = div()
                    .w_full()
                    .h(pt(32.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div().flex_none().w(pt(24.0)).flex().justify_center().child(
                            div().size(pt(14.0)).rounded(pt(3.0)).overflow_hidden().child(crate::widgets::swatch::swatch(paint.clone(), 14.0)),
                        ),
                    )
                    .child(
                        div().flex_1().min_w(px(0.0)).overflow_hidden().pl(pt(2.0)).text_size(pt(11.0)).text_color(rgba(t.text)).child(label.clone()),
                    );
                // 属性行点开取色器（egui `Act::EditPaint` → `open_picker(Style { id, index })`），
                // 写回走 `style.update {value: paints}`（整组 paints，顺序不变）。
                if let Some(idx) = *index
                    && let Some(edit) = self.style_edit.as_ref()
                {
                    let popups = edit.popups.clone();
                    let cmds = self.cmds.clone();
                    let sid = edit.id.clone();
                    let all = edit.paints.clone();
                    let current = paint.solid_color().map(crate::widgets::swatch::color32).unwrap_or(crate::canvas::geom::Color32::BLACK);
                    row = row.hover(|s| s.bg(rgba(t.hover))).cursor_pointer().on_mouse_down(
                        MouseButton::Left,
                        move |e: &MouseDownEvent, _w, cx: &mut App| {
                            let write = {
                                let cmds = cmds.clone();
                                let sid = sid.clone();
                                let all = all.clone();
                                move |np: figcraft_color::Paint, _w: &mut Window, _cx: &mut App| {
                                    let mut ps = all.clone();
                                    if let Some(slot) = ps.get_mut(idx) {
                                        *slot = np;
                                    }
                                    cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": ps })) });
                                }
                            };
                            let picker = PaintPicker { paint: all[idx].clone(), on_write: Rc::new(write), styles: Vec::new() };
                            let on_pick: Rc<dyn Fn(crate::canvas::geom::Color32, &mut Window, &mut App)> = Rc::new(|_, _, _| {});
                            // 走右栏统一的打开入口：触发标识 + 再点关闭 + 精确顶部一套逻辑。
                            crate::panels::right::sections::open_detached_with(
                                popups.clone(),
                                format!("style-paint-{sid}-{idx}"),
                                None,
                                cx,
                                |anchor, cx| Self::color(cx, anchor, current, Some(picker), Default::default(), on_pick, cmds.clone()),
                            );
                        },
                    );
                }
                Some(row.into_any_element())
            }
            Setting::PaintRow { tab: st, style_id, index, paint, hex_ed, op_ed } => {
                if *st != tab {
                    return None;
                }
                // 与右栏填充行**同一套**通用控件：`ColorField`（Input 模板：24 pt 色块 +
                // hex + 44 pt 透明度数值 + 24 pt「%」格）跨两列，行尾是统一网格的两颗 24 pt
                // 图标。原先这里是手写 div（16 + 16 色块 + 220 hex + 24 + 24），图标距卡片
                // 右缘只剩 4 pt，和 effect / grid 行对不齐（用户 2026-11-15：「布局不对，
                // 不是有封装好的控件，为什么不用呢」）。
                let sid = style_id.clone();
                let idx = *index;
                let hex = paint.solid_color().map(|c| c.to_hex()).unwrap_or_default();
                let cmds = self.cmds.clone();
                let visible = paint.visible;
                let eye = {
                    let cmds = cmds.clone();
                    let sid = sid.clone();
                    let paints = self.style_edit.as_ref().map(|e| e.paints.clone()).unwrap_or_default();
                    IconButton::new(if visible { Icon::Eye } else { Icon::EyeOff }).on_click(move |_, _, _| {
                        let mut ps = paints.clone();
                        if let Some(p) = ps.get_mut(idx) {
                            p.visible = !visible;
                        }
                        cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": ps })) });
                    })
                };
                let minus = {
                    let cmds = cmds.clone();
                    let sid = sid.clone();
                    let paints = self.style_edit.as_ref().map(|e| e.paints.clone()).unwrap_or_default();
                    IconButton::new(Icon::Minus).on_click(move |_, _, _| {
                        let mut ps = paints.clone();
                        if idx < ps.len() {
                            ps.remove(idx);
                        }
                        cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": ps })) });
                    })
                };
                // 色块点击 → 取色器（`Act::EditPaint`）：写回同样走 `style.update` 的整组 paints。
                let current = paint.solid_color().map(crate::widgets::swatch::color32).unwrap_or(crate::canvas::geom::Color32::BLACK);
                let mut cf = crate::widgets::color_field::ColorField::new(
                    cx,
                    paint.clone(),
                    crate::widgets::color_field::ColorText::Hex(SharedString::from(hex)),
                    STYLE_ROW_FIELD_W,
                    hex_ed.clone(),
                    op_ed.clone(),
                );
                if let Some(edit) = self.style_edit.as_ref() {
                    let slot = edit.picker.clone();
                    let all = edit.paints.clone();
                    let write_cmds = self.cmds.clone();
                    let write_sid = sid.clone();
                    let write: Rc<dyn Fn(figcraft_color::Paint, &mut Window, &mut App)> = Rc::new(move |np, _w, _cx| {
                        let mut ps = all.clone();
                        if let Some(slot) = ps.get_mut(idx) {
                            *slot = np;
                        }
                        write_cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": write_sid, "value": ps })) });
                    });
                    let pick = PaintPicker { paint: paint.clone(), on_write: write, styles: Vec::new() };
                    let on_pick: Rc<dyn Fn(crate::canvas::geom::Color32, &mut Window, &mut App)> = Rc::new(|_, _, _| {});
                    let open_cmds = self.cmds.clone();
                    // 跨帧稳定探针：与右栏色块同一机制，按下即量到真实顶部。
                    // 每个 paint 行要有自己的探针 key，否则同一样式的多行互相覆盖 bounds，
                    // 点前面一行会从另一行的顶部弹出（CodeRabbit PR#30/#31）。
                    let anchor_key = format!("style-editor-paint-{}-{}", sid, idx);
                    let hit = window_trigger_top(&anchor_key);
                    let anchored_key = anchor_key.clone();
                    cf = cf.on_swatch(move |pos: Point<Pixels>, window, cx: &mut App| {
                        // 往**左**让 8 pt：取色器右缘贴着面板左缘排开（`open_pos` 会把它放在
                        // 锚点左侧），两个卡片并排而不是一开一关。
                        let top = anchored_top(&anchored_key, window);
                        let anchor = Point { x: pos.x - px(8.0), y: pos.y };
                        let mut child = Self::color(cx, anchor, current, Some(pick.clone()), Default::default(), on_pick.clone(), open_cmds.clone());
                        child.anchor_to_button_top(top);
                        *slot.borrow_mut() = Some(Box::new(child));
                        open_cmds.wake();
                    });
                    return Some(with_button_bounds(crate::widgets::panel_section::field_row_full_trailing(cf, vec![eye, minus]), hit));
                }
                Some(crate::widgets::panel_section::field_row_full_trailing(cf, vec![eye, minus]).into_any_element())
            }
            Setting::PaintPreview { paint } => {
                // lizard `style_paint_preview`：304×120 区，内缩 16 后的 272×104 **整块**铺色；
                // 底是棋盘格，颜色带 alpha 时透出来。之前用 `swatch(paint, 104.0)` 只画了
                // 104×104 的方形，右边一大片空白——这是最明显的布局偏差。
                let solid = paint.solid_color().map(|c| crate::widgets::swatch::color32(c.with_alpha(c.a * paint.opacity)));
                let border = rgba(t.border);
                Some(
                    div()
                        .w_full()
                        .h(pt(120.0))
                        .flex()
                        .items_start()
                        .justify_center()
                        .pt(pt(16.0))
                        .child(
                            div().w(pt(272.0)).h(pt(104.0)).border_1().border_color(border).overflow_hidden().child(
                                gpui::canvas(
                                    |_, _, _| {},
                                    move |bounds, _, window, _| {
                                        crate::widgets::swatch::checker(window, bounds, 12.0);
                                        if let Some(c) = solid {
                                            window.paint_quad(gpui::fill(bounds, rgba(c)));
                                        }
                                    },
                                )
                                .size_full(),
                            ),
                        )
                        .into_any_element(),
                )
            }
            Setting::Multiline { tab: st, label, field, rows } => {
                if *st != tab {
                    return None;
                }
                // lizard `editor`：caption 与输入框紧贴（`item_spacing.y = 0`），
                // 输入框高度 = `desired_rows × 24`；之后 `add_space(8.0)`。
                Some(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .mb(pt(8.0))
                        .child(div().w_full().text_size(pt(10.0)).text_color(rgba(t.text_secondary)).child(label.clone()))
                        .child(div().w_full().h(pt(24.0 * rows)).child(field.clone()))
                        .into_any_element(),
                )
            }
            Setting::Field { tab: st, label, field } => {
                if *st != tab {
                    return None;
                }
                // lizard `style_metadata`（4 个样式编辑器共用）：行宽 304、行高 24；
                // 标签在 `x + 16`（11 pt `t.text_secondary`），输入框在 `x + 112`（宽 176）。
                // 两行之间的 add_space(8) 由 `Setting::Spacer` 给，这里不能再带 mb（否则叠加成 16）。
                Some(
                    div()
                        .w_full()
                        .h(pt(24.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(
                            div().w(pt(112.0)).flex_none().pl(pt(16.0)).text_size(pt(11.0)).text_color(rgba(t.text_secondary)).child(label.clone()),
                        )
                        .child(div().w(pt(176.0)).h(pt(24.0)).child(field.clone()))
                        .into_any_element(),
                )
            }
            Setting::Number { tab: st, label, field } => {
                if *st != tab {
                    return None;
                }
                if type_ui {
                    let control = div().w(pt(72.0)).h(pt(24.0)).flex().items_center().child(field.clone()).into_any_element();
                    Some(type_row(label, &t, control))
                } else {
                    Some(
                        div()
                            .w_full()
                            .h(pt(28.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(pt(6.0))
                            .child(div().flex_none().w(pt(76.0)).text_size(pt(9.0)).text_color(rgba(t.text_secondary)).child(label.clone()))
                            .child(div().flex_1().child(field.clone()))
                            .into_any_element(),
                    )
                }
            }
            Setting::Slider { tab: st, label, value, min, max, step, suffix, on_change } => {
                if *st != tab {
                    return None;
                }
                // 与 `Setting::Number` 的 Type settings 分支同一行样式：左标签 + 右控件。
                let cb = on_change.clone();
                let control = crate::widgets::Slider::new(*value, *min, *max)
                    .step(*step)
                    .width(120.0)
                    .show_value()
                    .precision(0)
                    .suffix(suffix.clone())
                    .on_change(move |v, window, cx| cb(v, window, cx))
                    .render()
                    .into_any_element();
                Some(type_row(label, &t, control))
            }
            Setting::Options { tab: st, label, active, options } => {
                if *st != tab {
                    return None;
                }
                let seg = self.options_segmented(options, *active, label);
                if !type_ui {
                    // Layout / Stroke：沿用原来的「放不下就标签上移」自适应行。
                    let inline_w = 134.0_f32;
                    let per_cell = inline_w / options.len().max(1) as f32;
                    let stacked = options.iter().any(|o| est_text_w(&o.label) + 10.0 > per_cell);
                    let cap = div().text_size(pt(9.0)).text_color(rgba(t.text_secondary)).child(label.clone());
                    return Some(if stacked {
                        div().w_full().flex().flex_col().gap(pt(4.0)).py(pt(2.0)).child(cap).child(seg).into_any_element()
                    } else {
                        div()
                            .w_full()
                            .min_h(pt(24.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(pt(6.0))
                            .child(div().flex_none().w(pt(76.0)).text_size(pt(9.0)).text_color(rgba(t.text_secondary)).child(label.clone()))
                            .child(div().flex_1().min_w(px(0.0)).child(seg))
                            .into_any_element()
                    });
                }
                let bar = options.iter().map(|o| est_text_w(&o.label) + 16.0).sum::<f32>().max(48.0);
                let seg = seg.fixed_bar(bar);
                if label.is_empty() { Some(seg.into_any_element()) } else { Some(type_row(label, &t, seg.into_any_element())) }
            }
            Setting::IconOptions { tab: st, label, active, bar_w, options, drill_in } => {
                if *st != tab {
                    return None;
                }
                let mut seg = Segmented::new(options.iter().map(|o| o.item()).collect()).active(*active).fixed_bar(*bar_w);
                let cmds = self.cmds.clone();
                let settings = self.settings.clone();
                let opts = options.clone();
                let key = label.clone();
                seg = seg.on_pick(move |i, _, _| {
                    let Some(opt) = opts.get(i) else { return };
                    cmds.push(Cmd { id: opt.cmd, params: Some(opt.now()) });
                    // 抬起格同步（命令引发的重绘会读到新值）。
                    if let Ok(mut list) = settings.try_borrow_mut() {
                        for s in list.iter_mut() {
                            if let Setting::IconOptions { label: l, active: a, .. } = s {
                                if *l == key {
                                    *a = Some(i);
                                }
                            }
                        }
                    }
                });
                let mut control: AnyElement = seg.into_any_element();
                if let Some(flag) = drill_in {
                    let f = flag.clone();
                    let wake = self.cmds.clone();
                    let chev = IconButton::new(Icon::ChevronRight).size(16.0).on_click(move |_, _, _| {
                        let mut b = f.borrow_mut();
                        *b = !*b;
                        drop(b);
                        wake.wake();
                    });
                    control = div().flex().flex_row().items_center().gap(pt(4.0)).child(control).child(chev).into_any_element();
                }
                Some(type_row(label, &t, control))
            }
            Setting::Dropdown { tab: st, label, current, anchor, options } => {
                if *st != tab {
                    return None;
                }
                let key = format!("type-settings-{}", label);
                let cur = current.borrow().clone();
                // 触发按钮：通用 `widgets::dropdown`（Input 模板同源 / bare 常态 / 发丝描边 / chevron），
                // 不再自己拼 div。点击把按钮窗口矩形回填为菜单锚点。
                let at = anchor.clone();
                let wake = self.cmds.clone();
                let trigger = dropdown(cur, 96.0, None).menu(key).on_click(move |rect, _window, _cx| {
                    *at.borrow_mut() = Some(rect);
                    wake.wake();
                });

                // 菜单：通用 `menu_popup_in`（`Menu::show` 的菜单表面 / hover / 勾选列 / 防溢出）。
                // 浮层占着根视图唯一的 popup 槽，故菜单由 `render()` 作为窗口级兄弟元素返回。
                let shown = *anchor.borrow();
                if let Some(rect) = shown {
                    let rows: Vec<MenuRow> =
                        options.iter().map(|o| MenuRow::new(o.label.clone()).checked(Some(o.label == *current.borrow()))).collect();
                    let opts = options.clone();
                    let cmds = self.cmds.clone();
                    let cur_state = current.clone();
                    let at2 = anchor.clone();
                    let wake2 = self.cmds.clone();
                    // 效果小窗：切换类型后下面那批数值行还属于旧类型，拿旧键写回会被引擎拒收。
                    // 选中即关窗，重新点开时按新类型重建设置项；其它窗口的行为不变。
                    let close_window = if self.kind == WindowKind::EffectSettings { self.dismiss.borrow().clone() } else { None };
                    let on_pick = Rc::new(move |picked: &str, w: &mut Window, cx: &mut App| {
                        if let Some(o) = opts.iter().find(|o| o.label == picked) {
                            *cur_state.borrow_mut() = o.label.clone();
                            cmds.push(Cmd { id: o.cmd, params: Some(o.now()) });
                        }
                        *at2.borrow_mut() = None;
                        wake2.wake();
                        if let Some(cb) = &close_window {
                            cb(w, cx);
                        }
                    });
                    let at3 = anchor.clone();
                    let wake3 = self.cmds.clone();
                    let dismiss = Rc::new(move |_w: &mut Window, _cx: &mut App| {
                        *at3.borrow_mut() = None;
                        wake3.wake();
                    });
                    // 3 项的 Wrap style 不会溢出，vw/vh 传 0 关掉防溢出即可。
                    self.push_overlay(menu_popup_in(rect, None, rows, on_pick, dismiss.clone(), 0.0, 0.0), dismiss);
                }
                Some(type_row(label, &t, trigger.into_any_element()))
            }
            Setting::Toggle { tab: st, label, on, supported, on_cmd, off_cmd } => {
                if *st != tab {
                    return None;
                }
                let idx = if *on.borrow() { 1 } else { 0 };
                let mut seg = Segmented::new(vec![SegItem::Svg(TypIcon::None), SegItem::Icon(Icon::Check)]).active(Some(idx)).fixed_bar(48.0);
                if *supported {
                    let cmds = self.cmds.clone();
                    let state = on.clone();
                    let oc = on_cmd.clone();
                    let fc = off_cmd.clone();
                    seg = seg.on_pick(move |i, _, _| {
                        let val = i == 1;
                        *state.borrow_mut() = val;
                        let opt = if val { &oc } else { &fc };
                        cmds.push(Cmd { id: opt.cmd, params: Some(opt.now()) });
                    });
                }
                let row = type_row(label, &t, seg.into_any_element());
                Some(if *supported { row } else { div().w_full().opacity(0.45).child(row).into_any_element() })
            }
            Setting::SubLabel { tab: st, text } => {
                if *st != tab {
                    return None;
                }
                Some(div().w_full().pt(pt(4.0)).text_size(pt(10.0)).text_color(rgba(t.text_secondary)).child(text.clone()).into_any_element())
            }
            Setting::Radio { tab: st, label, on, cmd } => {
                if *st != tab {
                    return None;
                }
                // egui `widgets::radio`：一个小圆环 + 中间实心点 + 文案；点未选中项才推命令。
                let ring = div()
                    .w(pt(12.0))
                    .h(pt(12.0))
                    .rounded_full()
                    .border(pt(1.0))
                    .border_color(rgba(t.text_secondary))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(*on, |d| d.child(div().w(pt(6.0)).h(pt(6.0)).rounded_full().bg(rgba(t.accent))));
                let radio_id: SharedString = format!("export-radio-{}", label).into();
                let row = div().id(radio_id).w_full().h(pt(24.0)).flex().flex_row().items_center().gap(pt(6.0)).child(ring).child(
                    div().text_size(pt(11.0)).text_color(rgba(if *on { t.text } else { t.text_secondary })).child(label.clone()),
                );
                if *on {
                    Some(row.into_any_element())
                } else {
                    let cmds = self.cmds.clone();
                    let opt = cmd.clone();
                    Some(
                        row.cursor_pointer()
                            .on_click(move |_e, _w, _cx| {
                                cmds.push(Cmd { id: opt.cmd, params: Some(opt.now()) });
                            })
                            .into_any_element(),
                    )
                }
            }
            Setting::ExpandedRows { tab: st, flag, rows } => {
                if *st != tab || !*flag.borrow() {
                    return None;
                }
                let mut body = div().w_full().flex().flex_col().gap(pt(2.0)).pl(pt(2.0));
                for row in rows {
                    if let Some(el) = self.render_setting(cx, row, tab) {
                        body = body.child(el);
                    }
                }
                Some(body.into_any_element())
            }
            // ── 本地样式 4 编辑器的新增渲染分支（对齐 lizard `style_picker`）──────────
            // ⚠️ 这里不能再放 `_ => None` 兜底：它会排在下面所有分支之前，把预览、Spacer、
            //    SectionTitle、属性行全部吞掉（曾导致 4 个弹窗只剩 Name/Description/Delete）。
            Setting::TextPreview { text } => {
                // lizard `text_style_editor`：304×120 灰底，渲染 "Rag 123" 采样字（按 `font_size` 缩放）。
                let size = Self::json_num(text, &["fontSize", "font_size"]).unwrap_or(24.0).clamp(6.0, 60.0);
                Some(
                    div()
                        .w_full()
                        .h(pt(120.0))
                        .bg(rgba(t.hover))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(pt(size as f32))
                        .text_color(rgba(t.text))
                        .child(SharedString::from("Rag 123"))
                        .into_any_element(),
                )
            }
            Setting::EffectPreview { effects } => {
                // 预览区 304×152，居中一个 **120×120 白色圆、50% 不透明度** 的样本，
                // 并按 `effects` 里实际启用（`visible != false`）的效果逐条渲染：
                // Drop shadow / Inner shadow / Layer blur / Background blur / Noise。
                // 真机渲染是 GPU 材质（`apply_wgsl_materials`），这里没有离屏缓冲，所以
                // 模糊用「多层同心圆 / 同心环 + 每层 1/N 不透明度」近似，叠加后得到从中心
                // 到边缘逐渐衰减的软边；与画布 `painter::paint_circle` 同一图元（正方 +
                // 四角半径 = r 的 `paint_quad`，贝塞尔圆在 0.2.1 会画成一坨）。
                let list = effects.clone();
                Some(
                    div()
                        .w_full()
                        .h(pt(152.0))
                        .relative()
                        .overflow_hidden()
                        .child(
                            gpui::canvas(
                                |_, _, _| {},
                                move |bounds, _, window, _| {
                                    crate::widgets::swatch::checker(window, bounds, 8.0);
                                    let x0 = f32::from(bounds.origin.x);
                                    let y0 = f32::from(bounds.origin.y);
                                    let w = f32::from(bounds.size.width);
                                    let h = f32::from(bounds.size.height);
                                    let cx = x0 + w / 2.0;
                                    let cy = y0 + h / 2.0;
                                    // 样本圆 Ø120（预览区 152 高，上下各留 16 给效果扩散）。
                                    let r = 60.0_f32;
                                    // 圆填充。
                                    let disc = |window: &mut Window, ccx: f32, ccy: f32, rr: f32, c: Color32| {
                                        if rr <= 0.0 || c.0[3] == 0 {
                                            return;
                                        }
                                        let rect = Bounds { origin: point(px(ccx - rr), px(ccy - rr)), size: size(px(rr * 2.0), px(rr * 2.0)) };
                                        let quad = gpui::fill(rect, rgba(c)).corner_radii(Corners {
                                            top_left: px(rr),
                                            top_right: px(rr),
                                            bottom_right: px(rr),
                                            bottom_left: px(rr),
                                        });
                                        window.paint_quad(quad);
                                    };
                                    // 环形描边（闭合折线，lyon 会补 join，不会碎成弧块）。
                                    let ring = |window: &mut Window, ccx: f32, ccy: f32, rr: f32, width: f32, c: Color32| {
                                        if rr <= 0.0 || width <= 0.0 || c.0[3] == 0 {
                                            return;
                                        }
                                        let segments = ((rr * 2.5) as usize).clamp(16, 96);
                                        let mut b = PathBuilder::stroke(px(width));
                                        for i in 0..segments {
                                            let t = i as f32 / segments as f32 * std::f32::consts::TAU;
                                            let p = point(px(ccx + rr * t.cos()), px(ccy + rr * t.sin()));
                                            if i == 0 {
                                                b.move_to(p);
                                            } else {
                                                b.line_to(p);
                                            }
                                        }
                                        b.close();
                                        if let Ok(path) = b.build() {
                                            window.paint_path(path, rgba(c));
                                        }
                                    };
                                    let is = |e: &Value, k: &str| e.get("type").and_then(Value::as_str).is_some_and(|v| v.eq_ignore_ascii_case(k));
                                    let fnum = |e: &Value, k: &str, d: f32| e.get(k).and_then(Value::as_f64).map(|v| v as f32).unwrap_or(d);
                                    // `Shadow::color` 是 `Color{r,g,b,a}`（0..1），UI 改写后可能是
                                    // `#RRGGBB[AA]` 字符串，两种都认。
                                    let ecolor = |e: &Value, d: Color32| -> Color32 {
                                        let Some(v) = e.get("color") else { return d };
                                        if let Some(s) = v.as_str() {
                                            return Color32::from_hex(s).unwrap_or(d);
                                        }
                                        let f = |k: &str, dv: f32| v.get(k).and_then(Value::as_f64).map(|x| x as f32).unwrap_or(dv);
                                        if v.is_object() {
                                            let a = f("a", 1.0);
                                            return Color32::from_rgba_unmultiplied(
                                                (f("r", 0.0) * 255.0).clamp(0.0, 255.0) as u8,
                                                (f("g", 0.0) * 255.0).clamp(0.0, 255.0) as u8,
                                                (f("b", 0.0) * 255.0).clamp(0.0, 255.0) as u8,
                                                (a * 255.0).clamp(0.0, 255.0) as u8,
                                            );
                                        }
                                        d
                                    };
                                    let offset_of = |e: &Value| -> (f32, f32) {
                                        match e.get("offset").and_then(Value::as_array) {
                                            Some(a) => (
                                                a.first().and_then(Value::as_f64).unwrap_or(0.0) as f32,
                                                a.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32,
                                            ),
                                            None => (0.0, 0.0),
                                        }
                                    };
                                    let vis: Vec<&Value> = list
                                        .as_array()
                                        .map(|a| a.iter().filter(|e| e.get("visible").and_then(Value::as_bool).unwrap_or(true)).collect())
                                        .unwrap_or_default();

                                    // ① 投影：偏移 + spread 后按 blur 半径多层外扩，边缘自然衰减。
                                    for e in &vis {
                                        if !is(e, "dropShadow") {
                                            continue;
                                        }
                                        let (ox, oy) = offset_of(e);
                                        let blur = fnum(e, "radius", 4.0).max(0.0);
                                        let core = (r + fnum(e, "spread", 0.0)).max(1.0);
                                        let c = ecolor(e, Color32::from_rgba_unmultiplied(0, 0, 0, 64));
                                        let steps = 14usize;
                                        for i in (0..steps).rev() {
                                            let t = (i as f32 + 1.0) / steps as f32;
                                            let rr = core + blur * t;
                                            // 每层 1/steps 的不透明度：中心被 14 层叠满后 ≈ 效果色本身
                                            // 的 alpha（不能再乘系数，否则半透明样本圆会被糊成实心灰）。
                                            let a = (c.0[3] as f32 / steps as f32).clamp(1.0, 255.0) as u8;
                                            disc(window, cx + ox, cy + oy, rr, Color32::from_rgba_unmultiplied(c.0[0], c.0[1], c.0[2], a));
                                        }
                                    }

                                    // ② 样本圆：白 50%；叠了 layer blur 就用多层柔化圆代替硬边。
                                    let layer_blur =
                                        vis.iter().filter(|e| is(e, "layerBlur")).map(|e| fnum(e, "radius", 4.0).max(0.0)).fold(0.0_f32, f32::max);
                                    if layer_blur > 0.5 {
                                        let steps = 16usize;
                                        let a = (128.0 / steps as f32 * 1.25).clamp(1.0, 255.0) as u8;
                                        for i in (0..steps).rev() {
                                            let t = (i as f32 + 1.0) / steps as f32;
                                            let rr = r + layer_blur * t * 0.85;
                                            disc(window, cx, cy, rr, Color32::from_rgba_unmultiplied(255, 255, 255, a));
                                        }
                                    } else {
                                        disc(window, cx, cy, r, Color32::from_rgba_unmultiplied(255, 255, 255, 128));
                                    }

                                    // ③ 背景模糊：真实是把圆背后的内容虚化，这里用圆内一层淡白雾示意。
                                    let bg_blur = vis
                                        .iter()
                                        .filter(|e| is(e, "backgroundBlur"))
                                        .map(|e| fnum(e, "radius", 4.0).max(0.0))
                                        .fold(0.0_f32, f32::max);
                                    if bg_blur > 0.5 {
                                        let a = (bg_blur * 2.2).clamp(6.0, 92.0) as u8;
                                        disc(window, cx, cy, r, Color32::from_rgba_unmultiplied(255, 255, 255, a));
                                    }

                                    // ④ 内阴影：圆内缘画一圈由外向内衰减的暗环（按 offset 轻微偏移）。
                                    for e in &vis {
                                        if !is(e, "innerShadow") {
                                            continue;
                                        }
                                        let blur = fnum(e, "radius", 4.0).clamp(1.0, r);
                                        let (ox, oy) = offset_of(e);
                                        let c = ecolor(e, Color32::from_rgba_unmultiplied(0, 0, 0, 64));
                                        let steps = 16usize;
                                        let step = blur / steps as f32;
                                        for i in 0..steps {
                                            let t = i as f32 / steps as f32;
                                            let rr = r - step * (i as f32 + 0.5);
                                            if rr <= 0.5 {
                                                break;
                                            }
                                            let a = (c.0[3] as f32 * (1.0 - t * 0.85) / 2.6).clamp(0.0, 255.0) as u8;
                                            ring(
                                                window,
                                                cx + ox * 0.35,
                                                cy + oy * 0.35,
                                                rr,
                                                step + 0.7,
                                                Color32::from_rgba_unmultiplied(c.0[0], c.0[1], c.0[2], a),
                                            );
                                        }
                                    }

                                    // ⑤ 噪点：圆内按 density 撒颗粒，尺寸取 `size`，颜色取效果色。
                                    for e in &vis {
                                        if !is(e, "noise") {
                                            continue;
                                        }
                                        let grain = fnum(e, "size", 0.5).clamp(0.6, 8.0);
                                        let density = fnum(e, "density", 0.5).clamp(0.0, 1.0);
                                        let c = ecolor(e, Color32::from_rgba_unmultiplied(0, 0, 0, 64));
                                        let count = (density * 900.0) as usize;
                                        let mut seed: u32 = 0x1234_5678;
                                        for _ in 0..count {
                                            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                                            let ux = ((seed >> 8) & 0xFFFF) as f32 / 65535.0;
                                            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                                            let uy = ((seed >> 8) & 0xFFFF) as f32 / 65535.0;
                                            let gx = cx + (ux * 2.0 - 1.0) * r;
                                            let gy = cy + (uy * 2.0 - 1.0) * r;
                                            if (gx - cx).powi(2) + (gy - cy).powi(2) > r * r {
                                                continue;
                                            }
                                            window.paint_quad(gpui::fill(
                                                Bounds { origin: point(px(gx), px(gy)), size: size(px(grain), px(grain)) },
                                                rgba(c),
                                            ));
                                        }
                                    }
                                },
                            )
                            .size_full(),
                        )
                        .into_any_element(),
                )
            }
            Setting::GridPreview { grids } => {
                // 图标按 Figma 导出的 6 个 SVG（`Frame 204`–`209`）1:1 绘制：
                //   204 / 205 / 206 = 46×46（2 pt 黑框）+ 内部 9 点阵 / 三竖条 / 三横条；
                //   207 / 208 / 209 = 同一块放在 50×50 里，**再多画右下角那条 L 形折线** ——
                //   即「这个样式里有多条网格」的标记。所以「有多个才带折线」是直接读设计稿的。
                let list = grids.as_array().cloned().unwrap_or_default();
                let first = list.iter().find(|g| g.get("visible").and_then(|v| v.as_bool()).unwrap_or(true)).or_else(|| list.first());
                let kind = first.and_then(|g| g.get("kind")).and_then(|k| k.as_str()).unwrap_or("grid").to_string();
                let multi = list.len() > 1;
                Some(
                    div()
                        .w_full()
                        .h(pt(152.0))
                        .relative()
                        .overflow_hidden()
                        .child(
                            gpui::canvas(
                                |_, _, _| {},
                                move |bounds, _, window, _| {
                                    crate::widgets::swatch::checker(window, bounds, 8.0);
                                    let x0 = f32::from(bounds.origin.x);
                                    let y0 = f32::from(bounds.origin.y);
                                    let w = f32::from(bounds.size.width);
                                    let h = f32::from(bounds.size.height);
                                    // 单条：46×46 的图标本身；多条：50×50 的框（多出来的是那条折线）。
                                    let side = if multi { 50.0 } else { 46.0 };
                                    let left = x0 + (w - side) / 2.0;
                                    let top = y0 + (h - side) / 2.0;
                                    let p = |x: f32, y: f32| point(px(x), px(y));
                                    let sz = |v: f32, hv: f32| size(px(v), px(hv));
                                    let black = rgba(crate::canvas::geom::Color32::BLACK);
                                    let white = rgba(crate::canvas::geom::Color32::WHITE);
                                    // 46×46 黑框（stroke-width 2 居中在 x=1/y=1 → 边框占 0..2）+ 白底。
                                    window.paint_quad(gpui::fill(gpui::Bounds { origin: p(left, top), size: sz(46.0, 46.0) }, black));
                                    window.paint_quad(gpui::fill(gpui::Bounds { origin: p(left + 2.0, top + 2.0), size: sz(42.0, 42.0) }, white));
                                    // 内部图形：点阵 6×6 每格 12；竖条 6×30；横条 30×6（均从 8 起）。
                                    match kind.as_str() {
                                        "columns" => {
                                            for i in 0..3 {
                                                let bx = left + 8.0 + 12.0 * i as f32;
                                                window.paint_quad(gpui::fill(gpui::Bounds { origin: p(bx, top + 8.0), size: sz(6.0, 30.0) }, black));
                                            }
                                        }
                                        "rows" => {
                                            for i in 0..3 {
                                                let by = top + 8.0 + 12.0 * i as f32;
                                                window.paint_quad(gpui::fill(gpui::Bounds { origin: p(left + 8.0, by), size: sz(30.0, 6.0) }, black));
                                            }
                                        }
                                        _ => {
                                            for r in 0..3 {
                                                for c in 0..3 {
                                                    let bx = left + 8.0 + 12.0 * c as f32;
                                                    let by = top + 8.0 + 12.0 * r as f32;
                                                    window.paint_quad(gpui::fill(gpui::Bounds { origin: p(bx, by), size: sz(6.0, 6.0) }, black));
                                                }
                                            }
                                        }
                                    }
                                    // 多条：右下折线（`M50 50H4V48H48V4H50V50Z` → 底部 4..50 与右侧 4..50
                                    // 各一条 2 pt 的带），与主块之间留 2 pt 缝 —— 像后面还压着一层。
                                    if multi {
                                        window.paint_quad(gpui::fill(gpui::Bounds { origin: p(left + 4.0, top + 48.0), size: sz(46.0, 2.0) }, black));
                                        window.paint_quad(gpui::fill(gpui::Bounds { origin: p(left + 48.0, top + 4.0), size: sz(2.0, 46.0) }, black));
                                    }
                                },
                            )
                            .size_full(),
                        )
                        .into_any_element(),
                )
            }
            Setting::SectionTitle { tab: st, text, h, style_id, array, menu, anchor } => {
                if *st != tab {
                    return None;
                }
                // lizard：Properties 标题（x+16、semibold 11pt）与「+」按钮在**同一行** 304×32；
                // 「Add fill」等只是按钮 tooltip，不是可见文字。
                let mut row = div()
                    .w_full()
                    .h(pt(*h))
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .pl(pt(16.0))
                            .text_size(pt(11.0))
                            .font_weight(crate::panels::UI_FONT_REGULAR)
                            .text_color(rgba(t.text))
                            .child(text.clone()),
                    )
                    .child(div().flex_1().min_w(px(0.0)));
                if !menu.is_empty() {
                    let write: Rc<dyn Fn(Value)> = {
                        let cmds = self.cmds.clone();
                        let sid = style_id.clone();
                        let arr = array.clone();
                        Rc::new(move |item: Value| {
                            let mut a = arr.clone();
                            if let Value::Array(items) = &mut a {
                                items.push(item);
                            } else {
                                a = Value::Array(vec![item]);
                            }
                            cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": a })) });
                        })
                    };
                    let single = menu.len() <= 1;
                    let push_direct = write.clone();
                    let direct_item = menu.first().map(|(_, v)| v.clone()).unwrap_or(Value::Null);
                    let open_anchor = anchor.clone();
                    let wake = self.cmds.clone();
                    let on_add = move |e: &MouseDownEvent, _w: &mut Window, _cx: &mut App| {
                        if single {
                            push_direct(direct_item.clone());
                        } else {
                            *open_anchor.borrow_mut() = Some(Bounds { origin: e.position, size: size(px(24.0), px(24.0)) });
                            wake.wake();
                        }
                    };
                    if let Some(rect) = *anchor.borrow() {
                        let rows: Vec<MenuRow> = menu.iter().map(|(l, _)| MenuRow::new(l.clone())).collect();
                        let items = menu.clone();
                        let w = write.clone();
                        let at2 = anchor.clone();
                        let wake2 = self.cmds.clone();
                        let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new(move |picked: &str, _win, _cx| {
                            if let Some((_, v)) = items.iter().find(|(l, _)| *l == picked) {
                                w(v.clone());
                            }
                            *at2.borrow_mut() = None;
                            wake2.wake();
                        });
                        let at3 = anchor.clone();
                        let wake3 = self.cmds.clone();
                        let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_win, _cx| {
                            *at3.borrow_mut() = None;
                            wake3.wake();
                        });
                        self.push_overlay(
                            menu_popup_in(
                                rect,
                                None,
                                rows,
                                on_pick,
                                dismiss.clone(),
                                f32::from(self.viewport.width),
                                f32::from(self.viewport.height),
                            ),
                            dismiss,
                        );
                    }
                    row = row.child(
                        div()
                            .size(pt(24.0))
                            .mr(pt(16.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .child(crate::widgets::icon::paint(Icon::Plus, rgba(t.text_secondary), 14.0))
                            .on_mouse_down(MouseButton::Left, on_add),
                    );
                }
                Some(row.into_any_element())
            }
            Setting::TextControls {
                tab: st,
                family,
                families,
                font_style,
                font_styles,
                size_field,
                line_field,
                spacing_field,
                style_id,
                text,
                family_anchor,
                style_anchor,
            } => {
                if *st != tab {
                    return None;
                }
                // lizard `text_style_editor` 的 `ui.scope(item_spacing = (8,8))`：每行先
                // add_space(16)，行距 8；三行分别是 248 / 120+120 / 120+120+Dots。
                // 「>」/Dots 打开的 Type settings 面板 Monet 还没搬，这里只留 24 宽的占位。
                let make_select = |key: &str,
                                   current: &Rc<RefCell<SharedString>>,
                                   width: f32,
                                   fld: &'static str,
                                   options: &[SharedString],
                                   anchor: &Rc<RefCell<Option<Bounds<Pixels>>>>|
                 -> AnyElement {
                    let cur = current.borrow().clone();
                    let at = anchor.clone();
                    let wake = self.cmds.clone();
                    let trigger = dropdown(cur, width, None).menu(key.to_string()).on_click(move |rect, _w, _cx| {
                        *at.borrow_mut() = Some(rect);
                        wake.wake();
                    });
                    if let Some(rect) = *anchor.borrow() {
                        let rows: Vec<MenuRow> = options.iter().map(|o| MenuRow::new(o.clone()).checked(Some(*o == *current.borrow()))).collect();
                        let sid = style_id.clone();
                        let ts = text.clone();
                        let cmds = self.cmds.clone();
                        let cur_state = current.clone();
                        let at2 = anchor.clone();
                        let wake2 = self.cmds.clone();
                        let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new(move |picked: &str, _win, _cx| {
                            let updated = {
                                let mut v = ts.borrow_mut();
                                v[fld] = json!(picked);
                                v.clone()
                            };
                            cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": updated })) });
                            *cur_state.borrow_mut() = SharedString::from(picked.to_string());
                            *at2.borrow_mut() = None;
                            wake2.wake();
                        });
                        let at3 = anchor.clone();
                        let wake3 = self.cmds.clone();
                        let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_win, _cx| {
                            *at3.borrow_mut() = None;
                            wake3.wake();
                        });
                        self.push_overlay(
                            menu_popup_in(
                                rect,
                                None,
                                rows,
                                on_pick,
                                dismiss.clone(),
                                f32::from(self.viewport.width),
                                f32::from(self.viewport.height),
                            ),
                            dismiss,
                        );
                    }
                    trigger.into_any_element()
                };
                let family_select = make_select("style-family", family, 248.0, "font_family", families, family_anchor);
                let style_select = make_select("style-weight", font_style, 120.0, "font_style", font_styles, style_anchor);
                // 数字框必须在 120 宽的 flex_none 容器里：直接作为 flex_row 子项会被交叉轴/剩余
                // 空间拉宽，第二行「字号」会一路撑到卡片右边（lizard 是固定 ControlWidth::Fixed(120)）。
                let fixed = |w: f32, el: AnyElement| div().w(pt(w)).h(pt(24.0)).flex_none().child(el).into_any_element();
                let row = |el: AnyElement| {
                    div().w_full().h(pt(24.0)).flex().flex_row().items_center().pl(pt(16.0)).gap(pt(8.0)).child(el).into_any_element()
                };
                let mut r2 = div().w_full().h(pt(24.0)).flex().flex_row().items_center().pl(pt(16.0)).gap(pt(8.0)).child(style_select);
                r2 = r2.child(fixed(120.0, size_field.clone().into_any_element()));
                let mut r3 = div()
                    .w_full()
                    .h(pt(24.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .pl(pt(16.0))
                    .gap(pt(8.0))
                    .child(fixed(120.0, line_field.clone().into_any_element()))
                    .child(fixed(120.0, spacing_field.clone().into_any_element()));
                // 行尾 Dots：打开 Type settings —— 和「选中文本节点」的同一个面板，只是写入
                // 目标换成本样式（`style.update`）。它作为**本卡片的子浮层**排在左侧，不占
                // root 的 `popup` 单槽，所以点开它不会把样式面板本身关掉。
                let open_type = {
                    let slot = self.style_edit.as_ref().map(|e| e.picker.clone());
                    let queue = self.cmds.clone();
                    let wake = self.cmds.clone();
                    let sid = style_id.clone();
                    let ts = text.clone();
                    move |y: Pixels, cx: &mut App| {
                        let Some(slot) = slot.clone() else { return };
                        let data = style_text_data(&ts.borrow());
                        let child = Self::type_settings(style_write_queue(&queue, sid.clone(), ts.clone()), &data, cx, point(px(0.0), y));
                        *slot.borrow_mut() = Some(Box::new(child));
                        wake.wake();
                    }
                };
                r3 = r3.child(
                    div()
                        .size(pt(24.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, move |e: &MouseDownEvent, _w, cx| open_type(e.position.y, cx))
                        .child(crate::widgets::icon::paint(Icon::Menu, rgba(t.text_secondary), 14.0)),
                );
                Some(
                    div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(pt(8.0))
                        .child(row(family_select))
                        .child(r2.into_any_element())
                        .child(r3.into_any_element())
                        .into_any_element(),
                )
            }
            Setting::EffectRow { tab: st, style_id, index, label, ty: _, visible, effect: _, array, anchor } => {
                if *st != tab {
                    return None;
                }
                // 右栏效果行的**同一套**：统一网格（`field_row_full_trailing`，左 16 / 右 8 内缩）
                // + `Dropdown::fill()`（带 chevron 的那颗字段按钮）+ 两颗 `IconButton`。
                // 原先这里手写 `div().flex_row()`、标签是裸文字、图标顶到卡片右缘，和右栏同一行
                // 长得完全不一样（用户 2026-11-15：「布局不对，而且没有复用已有组件」）。
                let sid = style_id.clone();
                let idx = *index;
                let vis = *visible;
                let arr = array.clone();
                let cmds = self.cmds.clone();
                let eye = {
                    let arr = arr.clone();
                    let cmds = cmds.clone();
                    let sid = sid.clone();
                    IconButton::new(if vis { Icon::Eye } else { Icon::EyeOff }).on_click(move |_, _, _| {
                        let mut a = arr.clone();
                        if let Value::Array(items) = &mut a
                            && let Some(e) = items.get_mut(idx)
                        {
                            e["visible"] = json!(!vis);
                        }
                        cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": a })) });
                    })
                };
                let minus = {
                    let arr = arr.clone();
                    let cmds = cmds.clone();
                    let sid = sid.clone();
                    IconButton::new(Icon::Minus).on_click(move |_, _, _| {
                        let mut a = arr.clone();
                        if let Value::Array(items) = &mut a
                            && idx < items.len()
                        {
                            items.remove(idx);
                        }
                        cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": a })) });
                    })
                };
                // 类型下拉：菜单与「+」同源（`Effect::TYPES`），选中就把这一条换成该类型的新对象，
                // `visible` 保持不变。点开与收起都走 `overlays`（浮层内菜单的唯一通道）。
                let at = anchor.clone();
                let wake = cmds.clone();
                let dd = dropdown(label.clone(), STYLE_ROW_FIELD_W, None).fill().on_click(move |rect, _w, _cx| {
                    *at.borrow_mut() = Some(rect);
                    wake.wake();
                });
                if let Some(rect) = *anchor.borrow() {
                    let rows: Vec<MenuRow> =
                        figcraft_doc::Effect::TYPES.iter().map(|(l, _)| MenuRow::new(tr(l)).checked(Some(tr(l) == label.as_ref()))).collect();
                    let arr2 = arr.clone();
                    let cmds2 = cmds.clone();
                    let sid2 = sid.clone();
                    let at2 = anchor.clone();
                    let wake2 = cmds.clone();
                    let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new(move |picked: &str, _w, _cx| {
                        if let Some((_, kind)) = figcraft_doc::Effect::TYPES.iter().find(|(l, _)| tr(l) == picked)
                            && let Some(mut e) = figcraft_doc::Effect::of_type(kind)
                        {
                            e.visible = vis;
                            let mut a = arr2.clone();
                            if let Value::Array(items) = &mut a
                                && let Some(slot) = items.get_mut(idx)
                                && let Ok(v) = serde_json::to_value(e)
                            {
                                *slot = v;
                            }
                            cmds2.push(Cmd { id: "style.update", params: Some(json!({ "style": sid2, "value": a })) });
                        }
                        *at2.borrow_mut() = None;
                        wake2.wake();
                    });
                    let at3 = anchor.clone();
                    let wake3 = cmds.clone();
                    let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_w, _cx| {
                        *at3.borrow_mut() = None;
                        wake3.wake();
                    });
                    self.push_overlay(
                        menu_popup_in(rect, None, rows, on_pick, dismiss.clone(), f32::from(self.viewport.width), f32::from(self.viewport.height)),
                        dismiss,
                    );
                }
                Some(crate::widgets::panel_section::field_row_full_trailing(dd, vec![eye, minus]).into_any_element())
            }
            Setting::GridRow { tab: st, style_id, index, label, visible, grids, anchor } => {
                if *st != tab {
                    return None;
                }
                // 与 `EffectRow` 同一套行（网格 + 下拉 + 眼睛 + 减号），只有下拉的菜单不同。
                let sid = style_id.clone();
                let idx = *index;
                let vis = *visible;
                let arr = grids.clone();
                let cmds = self.cmds.clone();
                let eye = {
                    let arr = arr.clone();
                    let cmds = cmds.clone();
                    let sid = sid.clone();
                    IconButton::new(if vis { Icon::Eye } else { Icon::EyeOff }).on_click(move |_, _, _| {
                        let mut a = arr.clone();
                        if let Value::Array(items) = &mut a
                            && let Some(e) = items.get_mut(idx)
                        {
                            e["visible"] = json!(!vis);
                        }
                        cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": a })) });
                    })
                };
                let minus = {
                    let arr = arr.clone();
                    let cmds = cmds.clone();
                    let sid = sid.clone();
                    IconButton::new(Icon::Minus).on_click(move |_, _, _| {
                        let mut a = arr.clone();
                        if let Value::Array(items) = &mut a
                            && idx < items.len()
                        {
                            items.remove(idx);
                        }
                        cmds.push(Cmd { id: "style.update", params: Some(json!({ "style": sid, "value": a })) });
                    })
                };
                // 类型下拉：Grid / Columns / Rows（与「+」菜单同源），换类型时保留 `visible` 与颜色。
                let at = anchor.clone();
                let wake = cmds.clone();
                let dd = dropdown(label.clone(), STYLE_ROW_FIELD_W, None).fill().on_click(move |rect, _w, _cx| {
                    *at.borrow_mut() = Some(rect);
                    wake.wake();
                });
                if let Some(rect) = *anchor.borrow() {
                    // 类型菜单：Grid / Columns / Rows（与「+」菜单同源）。行标签是「5 columns」
                    // 这类带数量的描述，无法与菜单文字直接比对，所以这一栏不画勾选标记。
                    let kinds: Vec<(&'static str, figcraft_doc::LayoutGrid)> = vec![
                        ("Grid", figcraft_doc::LayoutGrid::default()),
                        ("Columns", figcraft_doc::LayoutGrid::columns()),
                        ("Rows", figcraft_doc::LayoutGrid::rows()),
                    ];
                    let rows: Vec<MenuRow> = kinds.iter().map(|(l, _)| MenuRow::new(tr(l))).collect();
                    let arr2 = arr.clone();
                    let cmds2 = cmds.clone();
                    let sid2 = sid.clone();
                    let at2 = anchor.clone();
                    let wake2 = cmds.clone();
                    let on_pick: Rc<dyn Fn(&str, &mut Window, &mut App)> = Rc::new(move |picked: &str, _w, _cx| {
                        if let Some((_, proto)) = kinds.iter().find(|(l, _)| tr(l) == picked) {
                            let mut a = arr2.clone();
                            if let Value::Array(items) = &mut a
                                && let Some(slot) = items.get_mut(idx)
                            {
                                // 换类型时保留用户调过的颜色。
                                let color = slot.get("color").cloned();
                                if let Ok(mut v) = serde_json::to_value(proto) {
                                    if let (Some(c), Some(o)) = (color, v.as_object_mut()) {
                                        o.insert("color".into(), c);
                                    }
                                    *slot = v;
                                }
                            }
                            cmds2.push(Cmd { id: "style.update", params: Some(json!({ "style": sid2, "value": a })) });
                        }
                        *at2.borrow_mut() = None;
                        wake2.wake();
                    });
                    let at3 = anchor.clone();
                    let wake3 = cmds.clone();
                    let dismiss: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_w, _cx| {
                        *at3.borrow_mut() = None;
                        wake3.wake();
                    });
                    self.push_overlay(
                        menu_popup_in(rect, None, rows, on_pick, dismiss.clone(), f32::from(self.viewport.width), f32::from(self.viewport.height)),
                        dismiss,
                    );
                }
                Some(crate::widgets::panel_section::field_row_full_trailing(dd, vec![eye, minus]).into_any_element())
            }
        }
    }

    /// 文字分段条：抬起格同步写回 retained `active`（命令引发的重绘会读到新值）。
    fn options_segmented(&self, options: &[Opt], active: Option<usize>, key: &SharedString) -> Segmented {
        let seg = Segmented::new(options.iter().map(|o| o.item()).collect()).active(active);
        let cmds = self.cmds.clone();
        let settings = self.settings.clone();
        let opts = options.to_vec();
        let key = key.clone();
        seg.on_pick(move |i, _, _| {
            let Some(opt) = opts.get(i) else { return };
            cmds.push(Cmd { id: opt.cmd, params: Some(opt.now()) });
            if let Ok(mut list) = settings.try_borrow_mut() {
                for s in list.iter_mut() {
                    if let Setting::Options { label: l, active: a, .. } = s {
                        if *l == key {
                            *a = Some(i);
                        }
                    }
                }
            }
        })
    }
    /// Draws the shared window and (for the colour picker) the picker's body over its slot.
    /// Returns the elements in paint order; the root view puts them above its backdrop.
    pub(crate) fn render(
        &mut self,
        cx: &mut App,
        window: &mut Window,
        viewport: Size<Pixels>,
        dismiss: Rc<dyn Fn(&mut Window, &mut App)>,
        drag: Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>,
        drag_move: Rc<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)>,
        right_edge: Pixels,
    ) -> Vec<AnyElement> {
        // 视口变了（窗口缩放）要重新夹取；否则定位一次后钉住原点。
        let viewport_changed = self.viewport != viewport;
        self.viewport = viewport;
        // 变量编辑器把拖动绑在**自己的头部左侧**（不是整张卡片），所以要把回调传进 body。
        self.drag_start = Some(drag.clone());
        // Full 呈现：egui 的 `variables_panel::full(panel)` —— 只占「面板 + 画布」那块（左侧 rail
        // 保留），`Frame::NONE` 贴边铺满；Floating 用构造时的宽高与拖拽位置。
        let full = self.kind == WindowKind::VariablesTable && self.variables_full();
        let open = anchored_open(self.open, right_edge, self.exact_top);
        // 取色器的高度跟着内容走（纯色 ↔ 渐变、色板行数）。必须在读 `self.height` 之前算，
        // 否则切换模式的当帧卡片会短一截、下一帧才补上。
        if self.kind == WindowKind::Color {
            self.height = self.color_card_height();
        }
        // 构造期的 `height` 只是估算（取色器估成 532，实际约 307）：估算偏大时 `clamp` 会把
        // 卡片顶在上边界、怎么拖都不动。上一帧量到过真实高度就用它（变量表有自己的尺寸，
        // 由 `editor.size()` 决定，不参与）。
        //
        // ⛔ 固定高度的窗口（字体窗：body 恒 429）绝不能吃这个回写：body 高度 = `height`、
        // 列表又 `flex_1` 撑满，于是「量到 469 → height=469 → 卡片 509 → 再量 509…」每帧
        // 长 40pt，表现为「越滚越大」。固定高的窗口自己说了算。
        if self.editor.is_none() && !self.fixed_height() {
            let real = self.measured_h.get();
            if real > 0.0 {
                self.height = real;
            }
        }
        // 变量窗口允许用户拖 8 个 handle 改尺寸（`EditorState.size`）。
        let (base_w, base_h) = self.editor.as_ref().and_then(|e| e.size()).unwrap_or((self.width, self.height));
        // Full 从左侧 rail 的右缘开始，宽 = 视口 − rail，这样左栏图标不会消失。
        let rail = if full { crate::panels::RAIL_W } else { 0.0 };
        // 全屏只在「面板 + 画布」区，不能盖住顶部自定义标题栏（`TITLE_H`）——
        // 用户 2026-11-17：全屏会侵入标题栏。高度与原点一起扣掉标题栏。
        // 标题栏下沿还有 1pt 分隔线（`top_row` 的 `border_t`，占 `[TITLE_H, TITLE_H+1)`），
        // 卡片也必须从它下面开始，否则会把这条线盖掉（用户第二轮 2026-11-17）。
        let title_h = if full { crate::title_bar::TITLE_H + 1.0 } else { 0.0 };
        let width = if full { (f32::from(viewport.width) - rail).max(1.0) } else { base_w };
        let height = if full { (f32::from(viewport.height) - title_h).max(1.0) } else { base_h };
        // Full 是 `Frame::NONE`：内边距 0、无圆角/阴影；Floating 保留通用 16（变量表构造时是 0）。
        let pad = if full { 0.0 } else { self.pad };
        let body_h = if full {
            // 卡片原点已在 `title_h`（标题栏下沿），`height` 也已扣掉 `title_h`，
            // body 用 `height` 才刚好铺到视口底部；用完整 `viewport.height` 会让底部
            // 多出标题栏那截被裁掉。两份 PR 在此处各有修复，合并时都保留。
            height
        } else if self.fixed_height() {
            // 固定高窗口的 `height` 是整卡高度（含 40pt 头部），body 要把它减掉。
            (height - crate::widgets::popover::HEADER_H - 2.0 * self.pad).max(120.0)
        } else {
            height - 2.0 * self.pad
        };
        let at = if full {
            // 贴着 rail 右缘、标题栏下缘，保留左侧图标栏（egui 的 full 就是「面板+画布」那块）。
            Some(Point { x: px(rail), y: px(title_h) })
        } else {
            self.place
        };
        // 先算**当帧**卡片原点：面板内部右键菜单记的是窗口坐标，body_element 要用它换算成
        // 卡片局部坐标。origin() 只依赖 open/at/width/height/flush，可用一个 probe 提前求值，
        // 否则首帧或拖动后会差一帧。
        // 定位只做一次：`measured_h` 会在下一帧把 `height` 修正成真实高度，若每帧重新
        // `open_pos` / `clamp`，卡片就会在那一帧跳一下（鼠标一动才重绘 → 看起来像「刷新」）。
        // 钉住原点，除非用户拖动（`at` = `place`）或视口尺寸变了。
        //
        // ⚠️ 但要等**真实高度量到之后**才能钉：首帧 `height` 还是构造期估算（样式窗估 320，
        // 实际约 150），拿它去 `open_pos` 判断「底部溢出」会误判 → 窗口被抬到按钮上方；
        // 若这一帧就 pin 住，错误位置就永久固化。固定高的窗口（字体窗）`height` 本身就是
        // 真值，不受此约束。量到真高后 `height` 已是真实值，此时钉住才正确。
        // 钉住原点：一旦定位过一次，后续帧（含量到真实高度的那一帧、hover 消失触发的
        // notify 重绘）都复用同一原点，不再回到 `open_pos` 重算。
        //
        // ⛔ 原先要求 `height_measured` 才钉，于是「首帧 Open::At 顶部对齐按钮」→「次帧量到
        // 真高改走 Open::Anchor + 触底抬升」位置不一致，窗口跳一下。变量窗/字体窗高度大又靠
        // 下、抬升明显，且绑定按钮 hover 消失正好在那帧 notify，于是「鼠标移开就往下跑」。
        // 首帧的 Open::At 顶部严格对齐按钮、右缘贴面板左缘，正是 egui「top on y, right edge
        // on x」的正确位置（用户确认点开时是好的）；把它钉住即可，触底可通过拖拽处理。
        let pinned = self.placed_once && at.is_none() && !viewport_changed;
        let prev_origin = self.origin;
        // 首帧还没量到真实高度：不要用估算高度走 `open_pos` 的溢出抬升（会把卡片冤枉地
        // 顶到按钮上方）。直接把卡片右上角放在锚点上 —— 顶部严格对齐按钮、右缘贴面板，
        // 与 EGUI「top on y, right edge on x」一致；是否真的溢出留给量到真高后判断。
        let open = if at.is_none() && !self.fixed_height() && self.measured_h.get() <= 0.0 {
            match open {
                Open::Anchor(a) => Open::At(Point { x: px(f32::from(a.x) - width), y: a.y }),
                other => other,
            }
        } else {
            open
        };
        let mut probe = popover_window(open).width(width).pad(pad).flush(full).height(height);
        if let Some(p) = at {
            probe = probe.at(p);
        } else if pinned {
            probe = probe.at(prev_origin);
        }
        self.origin = probe.origin(viewport);
        // 浮层内通用下拉的菜单每帧重建：先清空上一帧，body_element 里的 Dropdown 会重新登记。
        self.overlays.borrow_mut().clear();
        self.overlay_dismiss.borrow_mut().clear();
        *self.dismiss.borrow_mut() = Some(dismiss.clone());
        // 用户：变量窗口不要外层通用 header。内层 VariablesEditor::header 自己画了完整的
        // "Variables" 标题 + 计数 + 侧栏开关 + Import/Export + 搜索/过滤 + Full 按钮，外层
        // 那条 40pt 空 title 把整排操作顶到 X 下面一行、侧栏被夹在中间——full / floating 都在犯。
        // 一律不挂外层 title；X 由 popover 独立绝对定位浮在右上角，内层 header 已为它留 48pt。
        let no_header = self.kind == WindowKind::VariablesTable;
        let mut pop = popover_window(open).width(width).pad(pad).flush(full).surface(self.surface).height(height);
        if !no_header {
            pop = pop.title(self.title_slot());
        }
        pop = pop
            .body(self.body_element(cx, window, body_h.max(120.0), width, viewport))
            .on_drag_start(move |e, window, cx| drag(e, window, cx))
            .on_drag_move(move |e, window, cx| drag_move(e, window, cx))
            .on_measure({
                let m = self.measured_h.clone();
                move |h: Pixels| {
                    m.set(f32::from(h));
                }
            });
        if let Some(p) = at {
            pop = pop.at(p);
        }
        if pinned {
            pop = pop.at(prev_origin);
        }
        let (origin, card) = pop.show(viewport, dismiss);
        self.origin = origin;
        // 首帧的 Open::At 位置已正确（顶部对齐按钮），这一帧就算定位完成可以钉住。
        self.placed_once = true;
        // 菜单卡片与浮层卡片同挂窗口根（els），所以菜单用窗口坐标，不被浮层的滚动裁剪。
        // 只要有菜单打开，就在最底层垫一层覆盖整窗的透明 backdrop：点击菜单以外的任何位置
        // 都会关闭菜单（弹窗窗口里没有主窗口那套 root popup backdrop，之前只能靠菜单逐项
        // 自己判断，点别处不失焦）。这是浮层内菜单的统一行为。
        let mut els = Vec::new();
        // ⚠️ backdrop 必须在 card **之后** push：GPUI 按数组顺序定 z-order，先前把 backdrop
        // 放在 card 前面，结果 card 盖在遮罩上，点卡片内任意空白都被 card 吃掉、永远关不掉
        // 菜单（用户实测）。菜单 overlays 最后 push，保持在遮罩之上仍可点选。
        els.push(card);
        {
            let dismissers: Vec<Rc<dyn Fn(&mut Window, &mut App)>> = self.overlay_dismiss.borrow().clone();
            if !dismissers.is_empty() {
                els.push(crate::widgets::popup_backdrop(Rc::new(move |window: &mut Window, cx: &mut App| {
                    for dismiss in &dismissers {
                        dismiss(window, cx);
                    }
                    // 独立弹窗不会像主窗口那样自动重绘，锚点清空后必须自己请求一帧。
                    window.refresh();
                })));
            }
        }
        els.extend(self.overlays.borrow_mut().drain(..));
        // 子浮层：属性行点开的取色器（或排版设置）。它是**本窗口自己**的一层，排在卡片左侧，
        // 不占 root 的 `popup` 单槽，所以样式面板不会被替换掉。渲染期间先把它从 `RefCell`
        // 里取出来（`render` 要 `&mut self`），画完再放回去，保留它自己的拖动 / 状态。
        // 先把子浮层从 `RefCell` 里**取出来**（借用当帧结束），画完再放回去：
        // `if let ... = slot.borrow_mut().take()` 会让那笔借用活到整个 if 体里，
        // 末尾放回时撞上 "RefCell already borrowed"。
        let sub = self.style_edit.as_ref().map(|e| e.picker.clone()).and_then(|slot| {
            let taken = slot.borrow_mut().take();
            taken.map(|c| (slot, c))
        });
        if let Some((slot, mut child)) = sub {
            let closer: Rc<dyn Fn(&mut Window, &mut App)> = {
                let slot = slot.clone();
                let wake = self.cmds.clone();
                Rc::new(move |_w: &mut Window, _cx: &mut App| {
                    *slot.borrow_mut() = None;
                    wake.wake();
                })
            };
            // 子浮层**自己拖自己**：按下 / 移动都写它自己的 `hold` / `place`，父卡片的 `place`
            // 一点都不动。这里给的是元素级回调（指针还在子卡片上时生效）；指针离开子卡片后
            // 由根视图的窗口级路由接上（`CanvasView::move_popup_drag` 先看 `sub_*`）。
            let sub_press: Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)> = {
                let slot = slot.clone();
                Rc::new(move |e: &MouseDownEvent, _w: &mut Window, _cx: &mut App| {
                    // 只记状态、不唤醒：按下的那次重绘会让 down/up 的 ElementId 对不上。
                    if let Some(c) = slot.borrow_mut().as_mut() {
                        c.press(e.position);
                    }
                })
            };
            let sub_move: Rc<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)> = {
                let slot = slot.clone();
                let wake = self.cmds.clone();
                Rc::new(move |e: &MouseMoveEvent, _w: &mut Window, _cx: &mut App| {
                    let moved = slot.borrow_mut().as_mut().map(|c| c.move_drag(e.position)).unwrap_or(false);
                    if moved {
                        wake.wake();
                    }
                })
            };
            // 子浮层的锚点是**本卡片左缘再往左 8**：`anchored_open` 会把锚点当成卡片的右缘，
            // 于是取色器整张排在样式面板左边、两张卡并排。直接传父窗口的 `right_edge` 会让它
            // 右缘对齐同一个位置，正好压在样式面板上。
            let child_edge = px(f32::from(self.origin.x) - 8.0);
            els.extend(child.render(cx, window, viewport, closer, sub_press, sub_move, child_edge));
            *slot.borrow_mut() = Some(child);
        }
        // 取色器自己的子浮层（断点颜色窗口 / 头部「+」的样式面板）：同样画在本卡片左侧。
        let color_sub = self.color.as_ref().map(|c| c.sub.clone()).and_then(|slot| {
            let taken = slot.borrow_mut().take();
            taken.map(|c| (slot, c))
        });
        if let Some((slot, mut child)) = color_sub {
            let closer: Rc<dyn Fn(&mut Window, &mut App)> = {
                let slot = slot.clone();
                let wake = self.cmds.clone();
                Rc::new(move |_w: &mut Window, _cx: &mut App| {
                    *slot.borrow_mut() = None;
                    wake.wake();
                })
            };
            let sub_press: Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)> = {
                let slot = slot.clone();
                Rc::new(move |e: &MouseDownEvent, _w: &mut Window, _cx: &mut App| {
                    if let Some(c) = slot.borrow_mut().as_mut() {
                        c.press(e.position);
                    }
                })
            };
            let sub_move: Rc<dyn Fn(&MouseMoveEvent, &mut Window, &mut App)> = {
                let slot = slot.clone();
                let wake = self.cmds.clone();
                Rc::new(move |e: &MouseMoveEvent, _w: &mut Window, _cx: &mut App| {
                    let moved = slot.borrow_mut().as_mut().map(|c| c.move_drag(e.position)).unwrap_or(false);
                    if moved {
                        wake.wake();
                    }
                })
            };
            let child_edge = px(f32::from(self.origin.x) - 8.0);
            els.extend(child.render(cx, window, viewport, closer, sub_press, sub_move, child_edge));
            *slot.borrow_mut() = Some(child);
        }
        els
    }
}

/// 估算一段选项文字在 9.5 pt 下的宽度（CJK 一字约一个字号宽，拉丁约 0.55 字号）。
///
/// 只用于判断「这一行的多选器要不要改成上下堆叠」，所以宁可估宽一点：估算偏大只会多加一行，
/// 估算偏小就会把字切掉。
fn est_text_w(s: &str) -> f32 {
    s.chars().map(|c| if c.is_ascii() { 5.5 } else { 10.0 }).sum()
}

/// Type settings 的一行：左标签 + 弹性留白 + 右控件（贴合参考截图的右对齐布局）。
fn type_row(label: &SharedString, t: &crate::theme::Tokens, control: AnyElement) -> AnyElement {
    div()
        .w_full()
        .min_h(pt(24.0))
        .flex()
        .flex_row()
        .items_center()
        .gap(pt(6.0))
        .child(div().flex_none().min_w(px(0.0)).text_size(pt(10.0)).text_color(rgba(t.text)).child(label.clone()))
        .child(div().flex_1().min_w(px(0.0)))
        .child(div().flex_none().child(control))
        .into_any_element()
}

/// 把一个数字框的提交接到 `node.set { props: { key: value } }`（Type settings 的数值行共用）。
///
/// 提交值按 `[min, max]` 夹紧 —— egui 那边是 `value.clamp(...)`，越界的键入不该写进文档。
/// 空文本视为「不动」：`Auto` 类的值（装饰线粗细）本来就允许留空。
/// clone 一份 effect JSON，只改一个顶层键，作为 `effect.set` 的 `effect` 参数。
/// CodeRabbit PR#10: write the edited effect back to `shared` like `effect_number` does — otherwise
/// toggling then editing a number submits the number's snapshot and silently drops the toggle.
fn effect_with(shared: &Rc<RefCell<Value>>, key: &str, value: Value) -> Value {
    let mut e = shared.borrow().clone();
    if let Some(o) = e.as_object_mut() {
        o.insert(key.to_string(), value);
    }
    *shared.borrow_mut() = e.clone();
    e
}

/// 一条数值设置项：显示值 = 存储值 × `scale`（密度 / 强度这类 0..1 用 100 显示成百分比），
/// 提交时反算、夹紧，然后只改一个键推 `effect.set`。`at = Some(i)` 用于 `offset: [x, y]`。
#[allow(clippy::too_many_arguments)]
fn effect_number(
    cx: &mut App,
    cmds: &Cmds,
    index: usize,
    shared: &Rc<RefCell<Value>>,
    key: &'static str,
    at: Option<usize>,
    label: &'static str,
    suffix: &str,
    step: f64,
    scale: f64,
    min: f64,
    max: f64,
) -> Setting {
    let raw = {
        let effect = shared.borrow();
        match at {
            Some(i) => effect.get(key).and_then(Value::as_array).and_then(|a| a.get(i)).and_then(Value::as_f64),
            None => effect.get(key).and_then(Value::as_f64),
        }
        .unwrap_or(0.0)
    };
    let field = number_field(cx, 72.0, Some(raw * scale), suffix, step, Slot::None);
    let live = shared.clone();
    let queue = cmds.clone();
    field.update(cx, move |tf, _| {
        tf.set_on_commit(move |text, _, _| {
            let Ok(v) = text.trim().parse::<f64>() else { return };
            if !v.is_finite() {
                return;
            }
            let v = (v / scale).clamp(min, max);
            // 从**最新**共享 effect 起改，而不是开窗时的快照，避免冲掉同窗其它控件的编辑。
            let mut e = live.borrow().clone();
            let Some(o) = e.as_object_mut() else { return };
            match at {
                Some(i) => {
                    let Some(arr) = o.get_mut(key).and_then(Value::as_array_mut) else { return };
                    if let Some(cell) = arr.get_mut(i) {
                        *cell = json!(v);
                    }
                }
                None => {
                    o.insert(key.to_string(), json!(v));
                }
            }
            *live.borrow_mut() = e.clone();
            queue.push(Cmd { id: "effect.set", params: Some(json!({ "index": index, "effect": e })) });
        });
    });
    Setting::Number { tab: 0, label: SharedString::from(label), field }
}

/// 按效果类型生成设置项（与 egui `effects.rs::effect_settings` 的字段对应；颜色需取色器，
/// 暂不搬入小窗）。所有写回都走 `effect.set`，参数只含当前类型自己的键。
fn effect_setting_rows(cx: &mut App, cmds: &Cmds, index: usize, shared: &Rc<RefCell<Value>>, ty: &str) -> Vec<Setting> {
    let mut out: Vec<Setting> = Vec::new();
    macro_rules! n {
        ($key:expr, $at:expr, $label:expr, $suffix:expr, $step:expr, $scale:expr, $min:expr, $max:expr) => {
            out.push(effect_number(cx, cmds, index, shared, $key, $at, $label, $suffix, $step, $scale, $min, $max));
        };
    }
    macro_rules! tgl {
        ($key:expr, $label:expr) => {{
            let cur = shared.borrow().get($key).and_then(Value::as_bool).unwrap_or(false);
            let s_on = shared.clone();
            let s_off = shared.clone();
            out.push(Setting::Toggle {
                tab: 0,
                label: SharedString::from($label),
                on: Rc::new(RefCell::new(cur)),
                supported: true,
                on_cmd: Opt::dynamic("On", "effect.set", Rc::new(move || json!({ "index": index, "effect": effect_with(&s_on, $key, json!(true)) }))),
                off_cmd: Opt::dynamic("Off", "effect.set", Rc::new(move || json!({ "index": index, "effect": effect_with(&s_off, $key, json!(false)) }))),
            });
        }};
    }
    macro_rules! choice {
        ($key:expr, $label:expr, $values:expr) => {{
            let vals: &[(&str, &str)] = $values;
            let cur = shared.borrow().get($key).and_then(Value::as_str).unwrap_or("").to_string();
            let opts: Vec<Opt> = vals
                .iter()
                .map(|(l, v)| {
                    let s = shared.clone();
                    let val = Value::String(v.to_string());
                    Opt::dynamic(*l, "effect.set", Rc::new(move || json!({ "index": index, "effect": effect_with(&s, $key, val.clone()) })))
                })
                .collect();
            let active = vals.iter().position(|(_, v)| *v == cur.as_str());
            out.push(Setting::Options { tab: 0, label: SharedString::from($label), active, options: opts });
        }};
    }
    match ty {
        "dropShadow" | "innerShadow" => {
            n!("offset", Some(0), "X", "", 1.0, 1.0, -10_000.0, 10_000.0);
            n!("offset", Some(1), "Y", "", 1.0, 1.0, -10_000.0, 10_000.0);
            n!("radius", None, "Blur", "", 1.0, 1.0, 0.0, 10_000.0);
            n!("spread", None, "Spread", "", 1.0, 1.0, -10_000.0, 10_000.0);
            if ty == "dropShadow" {
                tgl!("show_behind", "Show behind transparent areas");
            }
        }
        "layerBlur" | "backgroundBlur" => {
            choice!("blur_type", "Blur type", &[("Uniform", "uniform"), ("Progressive", "progressive")]);
            n!("radius", None, "Blur", "", 1.0, 1.0, 0.0, 10_000.0);
            n!("start_radius", None, "Start", "", 1.0, 1.0, 0.0, 10_000.0);
        }
        "noise" => {
            choice!("mode", "Mode", &[("Mono", "mono"), ("Duo", "duo"), ("Multi", "multi")]);
            n!("size", None, "Size", "", 0.1, 1.0, 0.01, 1_000.0);
            n!("density", None, "Density", "%", 1.0, 100.0, 0.0, 1.0);
            n!("opacity", None, "Opacity", "%", 1.0, 100.0, 0.0, 1.0);
        }
        "texture" => {
            n!("size", None, "Size", "", 0.1, 1.0, 0.01, 1_000.0);
            n!("radius", None, "Radius", "", 1.0, 1.0, 0.0, 10_000.0);
            tgl!("clip_to_shape", "Clip to shape");
        }
        "glass" => {
            n!("light_angle", None, "Light angle", "\u{00b0}", 1.0, 1.0, -180.0, 180.0);
            n!("light_intensity", None, "Intensity", "%", 1.0, 100.0, 0.0, 1.0);
            n!("refraction", None, "Refraction", "%", 1.0, 100.0, 0.0, 1.0);
            n!("depth", None, "Depth", "", 1.0, 1.0, 1.0, 1_000.0);
            n!("dispersion", None, "Dispersion", "%", 1.0, 100.0, 0.0, 1.0);
            n!("frost", None, "Frost", "", 1.0, 1.0, 0.0, 100.0);
            n!("splay", None, "Splay", "%", 1.0, 100.0, 0.0, 1.0);
        }
        _ => {}
    }
    out
}

fn wire_set(field: &Entity<TextField>, cx: &mut App, cmds: &Cmds, key: &'static str, min: f64, max: f64) {
    let queue = cmds.clone();
    field.update(cx, move |tf, _| {
        tf.set_on_commit(move |text, _, _| {
            let Ok(v) = text.trim().parse::<f64>() else { return };
            if !v.is_finite() {
                return;
            }
            let v = v.clamp(min, max);
            queue.push(Cmd { id: "node.set", params: Some(json!({ "props": { key: v } })) });
        });
    });
}

/// Placement rule from `ui-egui/src/popover.rs`: the card's right edge sits on the Design
/// panel's left edge, its top on the button that opened it — "lifted so its bottom keeps off the
/// screen's bottom". The pointer's x must never decide where a detached window opens.
///
/// When the trigger reports its own bounds we use that exact top. Otherwise (keyboard / command
/// opens) the old approximation stays: the press point minus half of a 24 pt button.
fn anchored_open(open: Open, right_edge: Pixels, exact_top: Option<Pixels>) -> Open {
    match open {
        Open::Anchor(a) => {
            let y = exact_top.unwrap_or_else(|| px((f32::from(a.y) - BUTTON_HALF).max(0.0)));
            Open::Anchor(Point { x: right_edge, y })
        }
        other => other,
    }
}

/// `Hsb` → `Color32`（与 `widgets/menu.rs::hsb_color` 同式，取色器主体自己算色用）。
fn hsb_color(h: f32, s: f32, b: f32) -> Color32 {
    crate::widgets::swatch::color32(figcraft_color::Hsb { h, s, b }.to_color())
}

/// 断点行的通用输入控件：位置百分比（`number_field`）+ 颜色行（`ColorField` 的两个裸编辑器，
/// Hex 与透明度都走 `color_row_editors`，数值行为 / 范围 / 拖「%」全在通用控件里）。
fn stop_fields(
    cx: &mut App,
    state: &Rc<RefCell<figcraft_color::Paint>>,
    write: &Rc<dyn Fn(figcraft_color::Paint, &mut Window, &mut App)>,
    index: usize,
    shown: &(String, String, String),
) -> StopFields {
    let pct = number_field_ranged(cx, 39.0, shown.0.parse().ok(), "%", 1.0, Slot::None, Some(0.0), Some(100.0));
    pct.update(cx, |tf, _| {
        let state = state.clone();
        let write = write.clone();
        tf.set_on_commit(move |text, window, cx| {
            let Ok(v) = text.trim().parse::<f64>() else { return };
            let mut np = state.borrow().clone();
            if let figcraft_color::PaintKind::Gradient(g) = &mut np.kind
                && let Some(s) = g.stops.get_mut(index)
            {
                s.position = (v / 100.0).clamp(0.0, 1.0) as f32;
            }
            write(np, window, cx);
        });
    });

    let (hex, opacity) = crate::widgets::ColorField::color_row_editors(
        cx,
        &shown.1,
        shown.2.parse().unwrap_or(100.0_f64).round() as i64,
        {
            let state = state.clone();
            let write = write.clone();
            move |text: &str, window, cx| {
                let Some(c) = figcraft_color::Color::from_hex(text.trim()) else { return };
                let mut np = state.borrow().clone();
                if let figcraft_color::PaintKind::Gradient(g) = &mut np.kind
                    && let Some(s) = g.stops.get_mut(index)
                {
                    // Hex 只管 RGB，透明度交给同一行的透明度单元格。
                    s.color = figcraft_color::Color::new(c.r, c.g, c.b, s.color.a);
                }
                write(np, window, cx);
            }
        },
        {
            let state = state.clone();
            let write = write.clone();
            move |v: f64, window, cx| {
                let mut np = state.borrow().clone();
                if let figcraft_color::PaintKind::Gradient(g) = &mut np.kind
                    && let Some(s) = g.stops.get_mut(index)
                {
                    s.color = s.color.with_alpha((v / 100.0).clamp(0.0, 1.0) as f32);
                }
                write(np, window, cx);
            }
        },
    );

    StopFields { pct, hex, opacity }
}

/// 在父取色器**左侧**弹出某个渐变断点的颜色窗口（隐藏顶部切换条），并把选色写回断点。
#[allow(clippy::too_many_arguments)]
fn open_stop_color(
    sub: Rc<RefCell<Option<Box<WindowState>>>>,
    state: Rc<RefCell<figcraft_color::Paint>>,
    write: Rc<dyn Fn(figcraft_color::Paint, &mut Window, &mut App)>,
    index: usize,
    color: figcraft_color::Color,
    at: Point<Pixels>,
    cmds: Cmds,
    _window: &mut Window,
    cx: &mut App,
) {
    let write_back: Rc<dyn Fn(figcraft_color::Paint, &mut Window, &mut App)> = {
        let state = state.clone();
        let write = write.clone();
        Rc::new(move |np: figcraft_color::Paint, window: &mut Window, cx: &mut App| {
            let sc = np.solid_color().unwrap_or(color);
            let mut full = state.borrow().clone();
            if let figcraft_color::PaintKind::Gradient(g) = &mut full.kind
                && let Some(s) = g.stops.get_mut(index)
            {
                s.color = sc;
            }
            write(full, window, cx);
        })
    };
    let picker = PaintPicker { paint: figcraft_color::Paint::solid(color), on_write: write_back, styles: Vec::new() };
    let on_pick: Rc<dyn Fn(Color32, &mut Window, &mut App)> = Rc::new(|_, _, _| {});
    let mut child = WindowState::color_nested(cx, at, picker_c32(color), Some(picker), Default::default(), on_pick, cmds.clone());
    child.anchor_to_button_top(at.y);
    *sub.borrow_mut() = Some(Box::new(child));
    cmds.wake();
}

/// 面板内容宽（= 240）。所有行的宽度都用它，行内再自己给 8 / 16 的内缩。
const PANEL_W: f32 = 240.0;
/// 色域方块边长（208 = 240 − 16×2）。
const SPECTRUM: f32 = 208.0;
/// 色块边长与节奏（16 + 8）。
const CHIT: f32 = 16.0;
const CHIT_PITCH: f32 = 24.0;
/// 一行放几个色块（216 / 24）。
const CHITS_PER_ROW: usize = 9;

/// 四种渐变类型，按 Figma 分段控件的顺序排（线性 / 径向 / 角度 / 菱形）。
const GRADIENT_KINDS: [figcraft_color::GradientKind; 4] = [
    figcraft_color::GradientKind::Linear,
    figcraft_color::GradientKind::Radial,
    figcraft_color::GradientKind::Angular,
    figcraft_color::GradientKind::Diamond,
];

/// Figma 的拾色点 / 滑块（`_1q2ok2r5/6` + `_1t0vmmd5/7`）：16 pt 白环、中间填实色，
/// 环外一圈 1 px 半透明描边（用 18 pt 的半透明底托出 `controlknoboffoutline`）。
/// `x`/`y` 是环心的位置（相对父容器的 `relative` 原点）。
fn picker_knob(x: f32, y: f32, fill: Color32) -> gpui::Div {
    div().absolute().left(px(x - 9.0)).top(px(y - 9.0)).w(pt(18.0)).h(pt(18.0)).rounded_full().bg(rgba(Color32::from_black_alpha(20))).child(
        div()
            .absolute()
            .left(pt(1.0))
            .top(pt(1.0))
            .w(pt(16.0))
            .h(pt(16.0))
            .rounded_full()
            .border(pt(4.0))
            .border_color(rgba(Color32::WHITE))
            .bg(rgba(fill)),
    )
}

/// 记录元素矩形（弹窗菜单锚点 / 浮窗精确顶部）的透明探针：prepaint 存盒子，绘制阶段什么都不画。
pub(crate) fn bounds_probe(hit: Rc<Cell<Option<Bounds<Pixels>>>>) -> gpui::Div {
    div().absolute().left_0().top_0().w_full().h_full().child(gpui::canvas(move |bounds, _, _| hit.set(Some(bounds)), |_, _, _, _| {}))
}

/// Attach a bounds probe as a child of the trigger element itself. Never wrap in a new `div()`:
/// a wrapper would collapse `w_full()` fields to their content width (the colour input shrank to
/// a "lump"), and would steal the positioning context of `absolute` buttons. The probe is
/// absolute/full-size, so it takes no space and only measures.
pub(crate) fn with_button_bounds<E>(el: E, hit: Rc<Cell<Option<Bounds<Pixels>>>>) -> AnyElement
where
    E: IntoElement,
    E::Element: ParentElement + Styled,
{
    el.into_element().relative().child(bounds_probe(hit)).into_any()
}

/// 开窗按钮的统一封装：探针写进按 `key` 跨帧稳定的 cell，点击时由 [`anchored_top`] 读同一个
/// cell。右栏每个打开 detached 窗口的按钮都用它 —— 这是「按钮顶部精确对位」的唯一复用件。
///
/// 与裸 `with_button_bounds` 的区别只在 cell 的来源：裸版本每帧新建 cell，点击当帧探针还没
/// 写入，`button_top` 只能退回鼠标位置（错）；这里 cell 跨帧存活，永远量得到真实按钮顶部。
pub(crate) fn with_window_trigger<E>(el: E, key: &str) -> AnyElement
where
    E: IntoElement,
    E::Element: ParentElement + Styled,
{
    with_button_bounds(el, window_trigger_top(key))
}

/// Inject the probe into an element that is **already positioned** (typically `absolute`).
/// Never call `.relative()` here: it would override `absolute` and drop the element into the
/// normal flow — that is what broke the variable-binding button that sits at `right: 0` of its
/// field. The probe itself is absolute/full-size and measures its nearest positioned ancestor.
pub(crate) fn with_absolute_bounds<E>(el: E, hit: Rc<Cell<Option<Bounds<Pixels>>>>) -> AnyElement
where
    E: IntoElement,
    E::Element: ParentElement,
{
    el.into_element().child(bounds_probe(hit)).into_any()
}

/// Exact button top for a window opened from `hit`, falling back to the press point when the
/// probe has not run yet (e.g. a keyboard open on the first frame).
pub(crate) fn button_top(hit: &Rc<Cell<Option<Bounds<Pixels>>>>, window: &Window) -> Pixels {
    hit.get().map(|b| b.origin.y).unwrap_or_else(|| window.mouse_position().y - px(BUTTON_HALF))
}

/// WCAG 相对亮度（sRGB，按 0.2126 / 0.7152 / 0.0722 加权）。
fn relative_luminance(c: Color32) -> f32 {
    let ch = |v: u8| {
        let s = f32::from(v) / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * ch(c.0[0]) + 0.7152 * ch(c.0[1]) + 0.0722 * ch(c.0[2])
}

/// 两个不透明色的对比度（1.0 … 21.0）。
/// Figma 下拉里「自动（图形）」括号内的判定结果文案。
fn aa_category_label(c: figcraft_color::ContrastCategory) -> &'static str {
    match c {
        figcraft_color::ContrastCategory::LargeText => "大文本",
        figcraft_color::ContrastCategory::NormalText => "普通文本",
        figcraft_color::ContrastCategory::Graphics => "图形",
        figcraft_color::ContrastCategory::Mixed => "混合",
    }
}

fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 浮层内菜单的定位/关闭不变量（用户 2026-11-19 逐条指定）：
    /// - AA 设置菜单：通用控件 + 宽度自适应(None) + 与按钮**中心对齐**(BottomCenter)；
    /// - 混色(水滴)菜单：**左边缘对齐按钮左缘**（默认 BottomStart）+ 下方 4pt；
    /// - 渐变类型菜单：宽度 104（不硬编码 120）；
    /// - 遮罩在卡片**之后**（否则点卡片内空白关不掉菜单）。
    #[test]
    fn picker_menus_align_close_and_width_invariants() {
        let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/popovers.rs"));
        // 源码切片必须落在字符边界上：按字节固定 +500 会在中文注释中间截断并 panic。
        fn window(src: &str, start: usize, len: usize) -> &str {
            let mut end = (start + len).min(src.len());
            while end > start && !src.is_char_boundary(end) {
                end -= 1;
            }
            &src[start..end]
        }
        // AA 菜单块：从注释到 push_overlay 结束。
        let aa = src.find("// AA 设置下拉：若本帧锚点为 Some").expect("aa menu block");
        let aa_end = src[aa..].find("// ── 组装").map(|d| aa + d).expect("aa menu end");
        let aa_block = &src[aa..aa_end];
        assert!(aa_block.contains("menu_popup_in_aligned"), "AA menu must use the aligned generic menu");
        assert!(aa_block.contains("RectAlign::BottomCenter"), "AA menu must be centre-aligned to its button");
        assert!(!aa_block.contains("Some(180.0)"), "AA menu must not hardcode 180 width (auto-size instead)");
        // 混色菜单：左对齐 = 直接用默认 menu_popup_in（BottomStart）。
        let blend = src.find("// 混色（水滴）菜单").expect("blend comment");
        let blend_block = window(src, blend, 500);
        assert!(blend_block.contains("menu_popup_in("), "blend menu must use default BottomStart (left-aligned)");
        assert!(!blend_block.contains("BottomEnd"), "blend menu must NOT be right-aligned");
        // 渐变类型菜单宽度 104，不硬编码 120。
        let grad = src.find("// 与上面的通用 dropdown 按钮（96）").expect("grad comment");
        let grad_block = window(src, grad, 500);
        assert!(grad_block.contains("Some(104.0)"), "gradient-kind menu width should be 104");
        assert!(!grad_block.contains("Some(120.0)"), "gradient-kind menu must not hardcode 120");
        // backdrop 必须排在 card 之后。
        let push_backdrop = src.find("els.push(crate::widgets::popup_backdrop").expect("backdrop push");
        let push_card = src.find("els.push(card)").expect("card push");
        assert!(push_card < push_backdrop, "backdrop must be pushed AFTER card so it sits above and catches clicks");
    }

    #[test]
    fn the_five_kinds_are_distinct() {
        let kinds = [WindowKind::Layout, WindowKind::Stroke, WindowKind::Color, WindowKind::Variables, WindowKind::TextStyles];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                assert_eq!(a == b, i == j);
            }
        }
    }

    #[test]
    fn a_row_carries_its_command() {
        let r = Row::new("Brand/Primary", "variable.bind", json!({ "field": "fill" })).group(tr("Collection")).trail(tr("Color"));
        assert_eq!(r.cmd, "variable.bind");
        assert_eq!(r.group.as_ref().map(|g| g.to_string()), Some("Collection".to_string()));
        assert_eq!(r.params["field"], "fill");
    }

    #[test]
    fn every_window_opens_against_the_design_panel_not_the_pointer() {
        // The press was out in the right panel; only its y (button top) may survive.
        let open = anchored_open(Open::Anchor(Point { x: px(2589.0), y: px(503.0) }), px(980.0), None);
        match open {
            Open::Anchor(p) => {
                assert_eq!(f32::from(p.x), 980.0, "right edge belongs on the panel edge");
                assert_eq!(f32::from(p.y), 503.0 - BUTTON_HALF, "fallback guesses the button top");
            }
            _ => panic!("an anchor must stay an anchor"),
        }
        // All five kinds are built as anchors now; none pins its top-left to the pointer.
        assert!(matches!(open, Open::Anchor(_)));
    }

    #[test]
    fn an_exact_button_top_wins_over_the_press_point() {
        // A click near the bottom edge of a 24 pt button: pointer − 12 would be 11 pt too low.
        let open = anchored_open(Open::Anchor(Point { x: px(2589.0), y: px(514.0) }), px(980.0), Some(px(502.0)));
        match open {
            Open::Anchor(p) => {
                assert_eq!(f32::from(p.x), 980.0, "left edge stays on the panel");
                assert_eq!(f32::from(p.y), 502.0, "top is the button's, not the pointer's");
            }
            _ => panic!("an anchor must stay an anchor"),
        }
    }

    #[test]
    fn a_drag_never_parks_the_card_outside_the_window() {
        let wake: Rc<dyn Fn()> = Rc::new(|| {});
        let mut w = WindowState::base(WindowKind::Layout, tr("Layout"), Cmds::new(&wake), Open::Anchor(Point { x: px(0.0), y: px(0.0) }));
        w.viewport = size(px(1200.0), px(800.0));
        w.width = WIDTH;
        w.height = 200.0;
        w.origin = Point { x: px(400.0), y: px(100.0) };
        w.press(Point { x: px(420.0), y: px(120.0) });
        w.start_drag();
        // Pointer leaves the window and comes back at an absurd coordinate: the card stays put.
        assert!(w.move_drag(Point { x: px(99_999.0), y: px(-99_999.0) }));
        let p = w.place.expect("drag recorded a place");
        assert!(f32::from(p.x) <= 1200.0 - WIDTH - 8.0);
        assert!(f32::from(p.y) >= 8.0);
        // A drag that has ended (e.g. the button was released outside) must not move the card.
        w.end_drag();
        assert!(!w.move_drag(Point { x: px(700.0), y: px(400.0) }));
    }

    /// The Floating variables window routes its drag through `variables_route_move` (what the root
    /// `on_mouse_move` calls via `move_popup_drag`). A press only records the grab origin; a move
    /// inside the 3pt dead zone must not latch, and a move past it must relocate `place`.
    #[test]
    fn variables_route_move_latches_past_dead_zone_and_relocates() {
        let wake: Rc<dyn Fn()> = Rc::new(|| {});
        let mut w = WindowState::variables_table(
            Cmds::new(&wake),
            Point { x: px(100.0), y: px(100.0) },
            VariablesData { collections: Vec::new(), libraries: Vec::new() },
            None,
        );
        w.viewport = size(px(1280.0), px(860.0));
        w.origin = Point { x: px(100.0), y: px(100.0) };
        // Press records the grab origin; no drag yet.
        w.press(Point { x: px(120.0), y: px(120.0) });
        assert!(w.variables_active(), "pending must mark the window active");
        // A 2px move stays inside the 3pt dead zone: must not latch, must not move.
        assert!(!w.variables_route_move(Point { x: px(121.0), y: px(121.0) }), "still inside dead zone");
        assert!(!w.dragging());
        assert!(w.pending().is_some());
        // A 100px move clears the dead zone: latches, then relocates `place`.
        let at = Point { x: px(220.0), y: px(220.0) };
        assert!(w.variables_route_move(at), "card should move");
        assert!(w.dragging());
        assert!(w.pending().is_none(), "start_drag consumed pending");
        let p = w.place.expect("place set");
        // grab = press - origin = (20,20); raw = at - grab = (200,200); inside viewport → unchanged.
        assert!((f32::from(p.x) - 200.0).abs() < 0.5, "place.x ≈ 200, got {}", f32::from(p.x));
        assert!((f32::from(p.y) - 200.0).abs() < 0.5, "place.y ≈ 200, got {}", f32::from(p.y));
        // Ending the drag clears every transient state (matches `variables_end_drag`).
        w.variables_end_drag();
        assert!(!w.dragging());
        assert!(w.pending().is_none());
        assert!(!w.variables_active());
    }

    // ── Type settings（Basics / Details 1:1 布局重写）─────────────────────────────

    #[test]
    fn opt_glyph_becomes_an_svg_cell_and_plain_label_stays_text() {
        let g = Opt::new("Align left", "node.set", json!({})).with_glyph(crate::widgets::typ_icons::TypIcon::AlignLeft);
        assert!(matches!(g.item(), SegItem::Svg(crate::widgets::typ_icons::TypIcon::AlignLeft)));
        let plain = Opt::new("Auto", "node.set", json!({}));
        assert!(matches!(plain.item(), SegItem::Text(ref t) if t == &SharedString::from("Auto")));
    }

    // 把 data-driven 的分组结果摊平成 (kind, tag-ish) 便于断言：只数 GroupTitle / Toggle / Divider。
    fn group_summary(settings: &[Setting]) -> (Vec<String>, usize, usize) {
        let mut titles = Vec::new();
        let mut toggles = 0;
        let mut dividers = 0;
        for s in settings {
            match s {
                Setting::GroupTitle { text, .. } => titles.push(text.to_string()),
                Setting::Toggle { .. } => toggles += 1,
                Setting::Divider { .. } => dividers += 1,
                _ => panic!("feature groups should only contain title/toggle/divider"),
            }
        }
        (titles, toggles, dividers)
    }

    #[test]
    fn feature_rows_are_driven_by_what_the_font_scan_returns() {
        let features = std::collections::BTreeMap::new();
        // 扫描只报 3 个 tag：liga(Letter forms) / tnum(Numbers) / ss03(Stylistic sets)。
        let supported = ["liga", "tnum", "ss03"].iter().map(|s| s.to_string()).collect();
        let rows = WindowState::type_feature_groups(1, &features, &supported);
        let (titles, toggles, dividers) = group_summary(&rows);
        assert_eq!(toggles, 3, "只有扫描到的 3 个 tag 生成开关，其余一律不出现");
        assert_eq!(titles.len(), 3, "只显示有行的组");
        assert_eq!(dividers, 3, "每组收尾一条分隔线");
        // 组序与 lizard 一致：Letter case → Numbers → Letter forms → …；这里只出现
        // Numbers / Letter forms / Stylistic sets 三组，跳过空组（Letter case / Horizontal / Other）。
        assert!(titles.iter().any(|t| t.contains("Number") || t.contains("数字")), "missing Numbers: {:?}", titles);
        assert!(titles.iter().any(|t| t.contains("Letter forms") || t.contains("字母形式")), "missing Letter forms: {:?}", titles);
        assert!(titles.iter().any(|t| t.contains("Stylistic") || t.contains("样式集")), "missing Stylistic sets: {:?}", titles);
    }

    #[test]
    fn a_tag_the_font_lacks_produces_no_row_at_all() {
        let features = std::collections::BTreeMap::new();
        let supported = ["tnum"].iter().map(|s| s.to_string()).collect(); // 只有 tnum
        let rows = WindowState::type_feature_groups(1, &features, &supported);
        let (_, toggles, _) = group_summary(&rows);
        assert_eq!(toggles, 1, "liga/frac/kern 等字体没有的 feature 根本不生成行（不是置灰）");
        // 每个生成出来的行都必须 supported=true（它来自扫描结果）。
        for s in &rows {
            if let Setting::Toggle { supported, .. } = s {
                assert!(*supported, "数据驱动生成的行不可能 unsupported");
            }
        }
    }

    #[test]
    fn scanned_tag_outside_the_catalog_is_grouped_as_other() {
        let features = std::collections::BTreeMap::new();
        let supported = ["aalt", "liga"].iter().map(|s| s.to_string()).collect(); // aalt 不在本地表
        let rows = WindowState::type_feature_groups(1, &features, &supported);
        let (titles, toggles, _) = group_summary(&rows);
        assert_eq!(toggles, 2);
        assert!(titles.iter().any(|t| t.contains("Other") || t.contains("其它") || t.contains("其他")), "表外 tag 归 Other features: {:?}", titles);
    }

    #[test]
    fn set_features_writes_one_tag_incrementally() {
        let features = std::collections::BTreeMap::new();
        let supported = ["liga", "tnum"].iter().map(|s| s.to_string()).collect();
        let rows = WindowState::type_feature_groups(1, &features, &supported);
        // 按命令里的 tag 定位 liga（组序里 Numbers 在 Letter forms 前，不能用索引）。
        let liga = rows
            .iter()
            .find_map(|s| match s {
                Setting::Toggle { on_cmd, .. } if on_cmd.params["features"].get("liga").is_some() => Some(s),
                _ => None,
            })
            .expect("liga toggle must exist (it was scanned)");
        let Setting::Toggle { on, on_cmd, off_cmd, .. } = liga else { unreachable!() };
        assert_eq!(on_cmd.cmd, "text.setFeatures");
        assert_eq!(off_cmd.cmd, "text.setFeatures");
        assert_eq!(on_cmd.params["features"].as_object().unwrap().len(), 1, "每次只增量写一个 tag");
        assert!(*on.borrow(), "liga 默认开");
    }

    #[test]
    fn document_override_wins_over_the_catalog_default() {
        let mut features = std::collections::BTreeMap::new();
        features.insert("liga".to_string(), false); // 文档显式关掉（表里 liga 默认开）
        let supported = ["liga"].iter().map(|s| s.to_string()).collect();
        let rows = WindowState::type_feature_groups(1, &features, &supported);
        let Setting::Toggle { on, .. } = &rows[1] else { panic!("expected toggle after title") };
        assert!(!*on.borrow(), "文档覆盖应盖过默认开");
    }

    /// 效果设置小窗的数值行按这些键写回。键名必须与 `serde_json::to_value(Effect)` 一致，
    /// 否则 `effect.set` 会被引擎 `refuse_ignored_effect_keys` 判成 unknown properties 拒收。
    #[test]
    fn effect_setting_keys_match_the_serialized_effect() {
        let cases: &[(&str, figcraft_doc::Effect, &[&str])] = &[
            ("dropShadow", figcraft_doc::Effect::drop_shadow(), &["type", "offset", "radius", "spread", "show_behind", "visible"]),
            ("layerBlur", figcraft_doc::Effect::layer_blur(), &["type", "radius", "blur_type", "start_radius"]),
            ("noise", figcraft_doc::Effect::noise(), &["type", "mode", "size", "density", "opacity"]),
            ("texture", figcraft_doc::Effect::texture(), &["type", "size", "radius", "clip_to_shape"]),
            (
                "glass",
                figcraft_doc::Effect::glass(),
                &["type", "light_angle", "light_intensity", "refraction", "depth", "dispersion", "frost", "splay"],
            ),
        ];
        for (name, e, keys) in cases {
            let v = serde_json::to_value(e).unwrap();
            for k in *keys {
                assert!(v.get(k).is_some(), "{name} JSON missing `{k}`: {v}");
            }
        }
    }

    /// 类型切换必须生成干净对象：旧实现 clone 旧 effect JSON 只改 `type`，
    /// 残留的 offset / spread / color 会让整条 `effect.set` 静默失效。
    #[test]
    fn switching_effect_type_drops_the_old_types_fields() {
        let old = serde_json::to_value(figcraft_doc::Effect::drop_shadow()).unwrap();
        assert!(old.get("offset").is_some() && old.get("spread").is_some());
        let clean = serde_json::to_value(figcraft_doc::Effect::of_type("glass").unwrap()).unwrap();
        assert_eq!(clean.get("type").and_then(Value::as_str), Some("glass"));
        for k in ["offset", "spread", "color"] {
            assert!(clean.get(k).is_none(), "clean glass must not carry shadow field `{k}`: {clean}");
        }
        // 小窗里的类型切换用的正是 `Effect::of_type`，逐类型都不残留别类的键。
        for (_, ty) in figcraft_doc::Effect::TYPES {
            let v = serde_json::to_value(figcraft_doc::Effect::of_type(ty).unwrap()).unwrap();
            assert_eq!(v.get("type").and_then(Value::as_str), Some(ty), "type tag for {ty}");
            assert!(v.as_object().unwrap().len() < 16, "{ty} carries too many fields: {v}");
        }
    }
}
