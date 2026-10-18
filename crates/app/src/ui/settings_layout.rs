//! 设置页纯布局：行规格与卡片游标（无数据、无绘制）。
//!
//! 行高与绘制共用同一份 [`RowSpec`] 列表：卡片高 = Σ(行主体 + 行内错误) +
//! 行间 1px 分隔线；绘制时用 [`take_row`] 推进同一游标。两者不一致会在设置页
//! `draw_card` 的 debug_assert 暴露——防内联错误行导致卡片底/分隔线漂移。

use eframe::egui;

use crate::ui::settings_tokens::DIVIDER_H;

/// 一行卡片的布局规格（高度与绘制共用的唯一行模型）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RowSpec {
    /// 行主体高（字号/通用 44、目录头 52、目录行 40）。
    pub(crate) height: f32,
    /// 行下方内联错误行的额外高（0 = 无）。
    pub(crate) extra: f32,
}

impl RowSpec {
    /// 无额外行。
    pub(crate) const fn new(height: f32) -> Self {
        Self { height, extra: 0.0 }
    }

    /// 行内错误/辅助行（如字号保存失败、自启未打包）追加的高度。
    pub(crate) fn with_extra(mut self, extra: f32) -> Self {
        debug_assert!(extra >= 0.0, "行内额外高不得为负:{extra}");
        self.extra = extra;
        self
    }

    /// 本行占用的总高（主体 + 内联错误行）。
    pub(crate) fn total(self) -> f32 {
        self.height + self.extra
    }
}

/// 卡片总高：Σ 行总高 + 行间分隔线（末行后无分隔线）。
pub(crate) fn card_height(rows: &[RowSpec]) -> f32 {
    rows.iter().map(|row| row.total()).sum::<f32>()
        + DIVIDER_H * rows.len().saturating_sub(1) as f32
}

/// 从卡片游标取出一行：可选前置分隔线 + 行主体 + 行内额外行。
#[derive(Debug, Clone, Copy)]
pub(crate) struct RowRects {
    /// 行前分隔线（首行无）。
    pub(crate) divider: Option<egui::Rect>,
    /// 行主体（标签与控件）。
    pub(crate) body: egui::Rect,
    /// 行内错误行（高度可为 0）。
    pub(crate) extra: egui::Rect,
}

/// 按 [`RowSpec`] 推进游标；`divider_before` = 非首行（行间 1px）。
pub(crate) fn take_row(
    card: egui::Rect,
    y: &mut f32,
    spec: RowSpec,
    divider_before: bool,
) -> RowRects {
    let divider = divider_before.then(|| {
        let rect = egui::Rect::from_min_size(
            egui::pos2(card.left(), *y),
            egui::vec2(card.width(), DIVIDER_H),
        );
        *y += DIVIDER_H;
        rect
    });
    let body = egui::Rect::from_min_size(
        egui::pos2(card.left(), *y),
        egui::vec2(card.width(), spec.height),
    );
    *y += spec.height;
    let extra = egui::Rect::from_min_size(
        egui::pos2(card.left(), *y),
        egui::vec2(card.width(), spec.extra),
    );
    *y += spec.extra;
    RowRects {
        divider,
        body,
        extra,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_for(rows: &[RowSpec]) -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(24.0, 100.0),
            egui::vec2(572.0, card_height(rows)),
        )
    }

    /// 行模型契约：卡片总高 = 行主体 + 内联错误行 + 分隔线；游标走到卡片底。
    #[test]
    fn card_height_matches_row_cursor_with_extra_lines() {
        let rows = [
            RowSpec::new(44.0),
            RowSpec::new(44.0).with_extra(17.0),
            RowSpec::new(40.0),
        ];
        let expected = 44.0 + 44.0 + 17.0 + 40.0 + 2.0 * DIVIDER_H;
        assert!((card_height(&rows) - expected).abs() < 1e-4);

        let card = card_for(&rows);
        let mut y = card.top();
        let mut dividers = 0;
        for (index, row) in rows.iter().enumerate() {
            let rects = take_row(card, &mut y, *row, index > 0);
            if rects.divider.is_some() {
                dividers += 1;
            }
        }
        assert_eq!(dividers, rows.len() - 1);
        assert!(
            (y - card.bottom()).abs() < 1e-4,
            "游标({y})必须回到卡片底({})",
            card.bottom()
        );
    }

    /// 首行无分隔线；空行列表 = 空卡片（不出现负 divider）。
    #[test]
    fn divider_only_between_rows() {
        let card = card_for(&[RowSpec::new(44.0)]);
        let mut y = card.top();
        let rects = take_row(card, &mut y, RowSpec::new(44.0), false);
        assert!(rects.divider.is_none());
        assert_eq!(rects.body.height(), 44.0);
        assert_eq!(rects.extra.height(), 0.0);

        assert_eq!(card_height(&[]), 0.0);
    }
}
