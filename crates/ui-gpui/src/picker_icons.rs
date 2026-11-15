//! 取色器 / 调色面板专用的 24 pt 图标（照 Figma 网页编辑器 DOM 里的原始 SVG 1:1 落地）。
//!
//! 为什么不塞进冻结的 [`crate::widgets::icon::Icon`]：那张表的判别值、`asset_name`、`ALL`
//! 与单测断言全部锁死（2026-11-05 用户硬性要求），而这组图标只服务取色器的四条
//! 工具栏（填充类型 / 混合模式 / 吸管 / 色板下拉箭头）。项目既有约定（见 `typ_icons`）：
//! 冻结文件只加 `pub mod` / `pub use`，不改内容 —— 于是走 `canvas/svg_text.rs` 里
//! `AppAssets` 的 `picker:` 前缀，把 Figma 的 `<path d>` 原样内联，用与 `icon::paint`
//! 完全相同的 `svg().path().text_color()` 管线渲染（SVG 只收黑色，着色由 `text_color` 注入）。
//!
//! 来源：`Figma – Figma.html` 快照里取色面板的 `svg[data-fpl-icon-size="24"]` 节点，
//! 路径数据未做任何改写（只把 `fill="var(--fpl-icon-color…)"` 换成 `fill="#000"`）。

use gpui::{Hsla, IntoElement, SharedString, prelude::*, svg};

/// 取色器图标。每个对应一个 `assets/picker/*.svg`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerIcon {
    /// 纯色填充：外框 + 实心小方块（`paint_type_group_tabs--SOLID`）。
    Solid,
    /// 渐变填充：外框 + 平滑色阶点阵（`paint_type_group_tabs--GRADIENT_LINEAR`）。
    Gradient,
    /// 混合模式：水滴（`aria-label="混合模式"`）。
    Blend,
    /// 吸管（`aria-label="Sample color"`）。
    Dropper,
    /// 对比度设置：竖排推子（`aria-label="对比度设置"`）。
    Sliders,
    /// 下拉箭头（`_15y6gsq7` 里的 trailing chevron）。
    Chevron,
}

struct Entry {
    icon: PickerIcon,
    name: &'static str,
    svg: &'static str,
}

/// 唯一资源表。`include_str!` 编译期内联，运行时零 I/O。
static ENTRIES: &[Entry] = &[
    Entry { icon: PickerIcon::Solid, name: "picker-solid", svg: include_str!("../assets/picker/picker-solid.svg") },
    Entry { icon: PickerIcon::Gradient, name: "picker-gradient", svg: include_str!("../assets/picker/picker-gradient.svg") },
    Entry { icon: PickerIcon::Blend, name: "picker-blend", svg: include_str!("../assets/picker/picker-blend.svg") },
    Entry { icon: PickerIcon::Dropper, name: "picker-dropper", svg: include_str!("../assets/picker/picker-dropper.svg") },
    Entry { icon: PickerIcon::Sliders, name: "picker-sliders", svg: include_str!("../assets/picker/picker-sliders.svg") },
    Entry { icon: PickerIcon::Chevron, name: "picker-chevron", svg: include_str!("../assets/picker/picker-chevron.svg") },
];

/// 资源前缀。完整格式：`picker:<name>.svg`。
pub const ASSET_PREFIX: &str = "picker:";

impl PickerIcon {
    fn entry(self) -> &'static Entry {
        &ENTRIES[self as usize]
    }

    pub fn asset_path(self) -> SharedString {
        let e = self.entry();
        debug_assert_eq!(e.icon, self, "PickerIcon 判别值与资源表顺序不一致");
        SharedString::from(format!("{ASSET_PREFIX}{}.svg", e.name))
    }

    /// `AssetSource::load` 的查表入口。
    pub fn load(name: &str) -> Option<&'static str> {
        ENTRIES.iter().find(|e| e.name == name).map(|e| e.svg)
    }

    pub fn list() -> impl Iterator<Item = SharedString> {
        ENTRIES.iter().map(|e| SharedString::from(format!("{ASSET_PREFIX}{}.svg", e.name)))
    }
}

/// 与 `icon::paint` 同一契约：`svg()` → `text_color()` 着色 → 给定尺寸（24 网格原生尺寸）。
pub fn paint(icon: PickerIcon, color: impl Into<gpui::Rgba>, size: f32) -> impl IntoElement {
    let rgba: gpui::Rgba = color.into();
    svg().path(icon.asset_path()).w(gpui::px(size)).h(gpui::px(size)).flex_none().text_color(Hsla::from(rgba))
}

#[cfg(test)]
mod tests {
    use super::{ASSET_PREFIX, ENTRIES, PickerIcon};

    #[test]
    fn every_icon_has_a_24_grid_svg() {
        assert_eq!(ENTRIES.len(), PickerIcon::Chevron as usize + 1);
        for e in ENTRIES {
            assert!(e.svg.contains(r#"viewBox="0 0 24 24""#), "{} 不是 24 网格", e.name);
            assert!(e.svg.contains(r##"fill="#000""##), "{} 必须是单色（靠 text_color 注入）", e.name);
        }
    }

    #[test]
    fn paths_carry_the_prefix() {
        assert_eq!(PickerIcon::Blend.asset_path(), format!("{ASSET_PREFIX}picker-blend.svg"));
        assert!(PickerIcon::load("picker-blend").is_some());
        assert!(PickerIcon::load("picker-nope").is_none());
    }
}
