//! 主键条（R13）：字段表上方显示 `PRIMARY KEY (a, b)`，主键列可拖动调整顺序、移出主键或从菜单加入；
//! 自增列不在主键里时给出一键“设为主键”。新建表与修改已有表共用，主键顺序存在草稿的 `pk_order` 里，
//! 生成的建表语句与 DROP/ADD PRIMARY KEY 都按这里的顺序。

use gpui_kit::assets::IconName;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div, px,
};

use super::{create, StructureMode, StructurePane};
use crate::ddl::ColumnDraft;
use crate::i18n;
use crate::ui::kit::{self, ControlSize, Palette, Radius, Space, Tip as _, TypeScale};

/// 拖动主键条里的一列时携带的源位置（主键内的序号）。
#[derive(Clone, Debug)]
pub(super) struct PkDrag {
    pub position: usize,
    pub label: SharedString,
}

/// 把主键列的 `pk_order` 规整成 0..n：先按已有序号，没有序号的按字段顺序排在后面。不在主键里（或已删除）的列清空序号。
pub(super) fn normalize_pk_order(columns: &mut [ColumnDraft]) {
    let mut order: Vec<usize> = (0..columns.len()).filter(|&index| columns[index].primary && !columns[index].dropped).collect();
    order.sort_by_key(|&index| columns[index].pk_order.unwrap_or(u16::MAX));
    for column in columns.iter_mut() {
        column.pk_order = None;
    }
    for (position, index) in order.into_iter().enumerate() {
        columns[index].pk_order = u16::try_from(position).ok();
    }
}

impl StructurePane {
    /// 主键列在 `kept_columns` 里的下标，按主键顺序。
    pub fn primary_key_order(&self) -> Vec<usize> {
        let columns = &self.session.kept_columns;
        let mut order: Vec<usize> = (0..columns.len()).filter(|&index| columns[index].primary && !columns[index].dropped).collect();
        order.sort_by_key(|&index| columns[index].pk_order.unwrap_or(u16::MAX));
        order
    }

    /// 某列在主键里的序号（从 1 起）。不在主键里时为 None。
    pub fn primary_key_position(&self, index: usize) -> Option<usize> {
        self.primary_key_order().iter().position(|item| *item == index).map(|position| position + 1)
    }

    /// 把一列加入或移出主键。加入时排在主键末尾并自动设为 NOT NULL（主键列不能为 NULL）。
    pub fn set_primary(&mut self, index: usize, on: bool) {
        // 先把已有主键列编好号（例如默认的 id 列还没有序号），新加入的列才能排在它们后面。
        normalize_pk_order(&mut self.session.kept_columns);
        let next = self.primary_key_order().len();
        let Some(column) = self.session.kept_columns.get_mut(index) else {
            return;
        };
        if column.primary == on {
            return;
        }
        column.primary = on;
        if on {
            column.pk_order = u16::try_from(next).ok();
            column.nullable = false;
            if column.default == crate::ddl::DefaultSpec::Null {
                column.default = crate::ddl::DefaultSpec::None;
            }
        } else {
            column.pk_order = None;
        }
        normalize_pk_order(&mut self.session.kept_columns);
        self.session.cancel();
    }

    /// 在主键内调整顺序：把第 `from` 个主键列移到第 `to` 个位置（都从 0 起）。
    pub fn move_primary_key(&mut self, from: usize, to: usize) {
        let mut order = self.primary_key_order();
        if from == to || from >= order.len() || to >= order.len() {
            return;
        }
        let moved = order.remove(from);
        order.insert(to, moved);
        for (position, index) in order.into_iter().enumerate() {
            self.session.kept_columns[index].pk_order = u16::try_from(position).ok();
        }
        self.session.cancel();
    }

    /// 自增但不在主键里、又会被拦下的列（下标）。主键条给出一键“设为主键”。
    pub fn auto_outside_primary_key(&self) -> Vec<usize> {
        let create = self.session.mode == StructureMode::Create;
        self.session
            .kept_columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !column.dropped && column.auto && !column.primary)
            .filter(|(_, column)| !self.snapshot.as_ref().is_some_and(|snapshot| crate::ddl::plan::auto_keyed_elsewhere(snapshot, column, create)))
            .map(|(index, _)| index)
            .collect()
    }

    /// 已有表的主键组成或顺序是否被草稿改动。
    pub fn primary_key_changed(&self) -> bool {
        self.session.mode == StructureMode::Existing
            && self.snapshot.as_ref().is_some_and(|snapshot| crate::ddl::plan::primary_key_change(snapshot, &self.session.kept_columns).is_some())
    }

    /// 字段表上方的主键条。视图与没有字段的草稿不显示。
    pub(super) fn primary_key_strip(&self, entity: &Entity<Self>, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let mono = cx.theme().mono_font_family.clone();
        let metrics = kit::Metrics::of(cx);
        let order = self.primary_key_order();
        let candidates: Vec<(usize, String)> = self
            .session
            .kept_columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !column.primary && !column.dropped && !column.name.trim().is_empty() && column.generated.is_none())
            .map(|(index, column)| (index, column.name.trim().to_owned()))
            .collect();
        let mut chips = div().flex().flex_wrap().items_center().gap(px(Space::XS)).font_family(mono.clone()).text_size(px(TypeScale::SM));
        chips = chips.child(div().text_color(p.text_faint).child("PRIMARY KEY ("));
        if order.is_empty() {
            chips = chips.child(div().text_color(p.text_faint).child(t("structure.pk.none")));
        }
        let last = order.len().saturating_sub(1);
        for (position, &index) in order.iter().enumerate() {
            let name = self.session.kept_columns[index].name.trim().to_owned();
            let label = SharedString::from(name.clone());
            let remove_owner = entity.clone();
            let accent = p.accent;
            chips = chips.child(
                div()
                    .id(SharedString::from(format!("structure-pk-chip-{position}")))
                    .test_support()
                    .h(px(metrics.control_h - 4.))
                    .pl(px(Space::SM))
                    .pr(px(Space::XXS))
                    .flex()
                    .items_center()
                    .gap(px(Space::XS))
                    .rounded(px(Radius::CONTROL))
                    .bg(p.warn_soft)
                    .border_1()
                    .border_color(p.line_soft)
                    .cursor_grab()
                    .tooltip_text(t("structure.pk.dragHint"))
                    .on_drag(PkDrag { position, label: label.clone() }, |drag, _, _, cx| cx.new(|_| create::ColumnDragGhost { label: drag.label.clone() }))
                    .drag_over::<PkDrag>(move |style, _, _, _| style.border_color(accent))
                    .on_drop(cx.listener(move |pane, drag: &PkDrag, _, cx| {
                        pane.move_primary_key(drag.position, position);
                        cx.notify();
                    }))
                    .child(kit::icon(IconName::GripVertical, 11., p.text_faint))
                    .child(div().text_size(px(TypeScale::XS)).text_color(p.warn).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(format!("{}", position + 1))))
                    .child(div().text_color(p.text).child(label))
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-pk-remove-{position}")), IconName::X, t("structure.pk.remove"), move |_, _, cx| {
                            remove_owner.update(cx, |pane, cx| {
                                pane.set_primary(index, false);
                                cx.notify();
                            });
                        })
                        .xsmall(),
                    ),
            );
            if position < last {
                chips = chips.child(div().text_color(p.text_faint).child(","));
            }
        }
        chips = chips.child(div().text_color(p.text_faint).child(")"));
        let add_owner = entity.clone();
        let add_menu = (!candidates.is_empty()).then(|| {
            kit::menu_button("structure-pk-add").icon(IconName::Plus).label(i18n::text(locale, "structure.pk.add")).dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(ControlSize::FORM_MIN_W)).max_h(px(ControlSize::PICKER_MAX_W)).scrollable(true);
                for (index, name) in &candidates {
                    let owner = add_owner.clone();
                    let index = *index;
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_primary(index, true);
                            cx.notify();
                        });
                    }));
                }
                menu
            })
        });
        let mut strip = div()
            .id("structure-pk-strip")
            .test_support()
            .flex()
            .flex_col()
            .gap(px(Space::XS))
            .px(px(Space::MD))
            .py(px(Space::SM))
            .rounded(px(Radius::CONTROL))
            .border_1()
            .border_color(if self.primary_key_changed() { p.warn } else { p.line_soft })
            .bg(p.surface)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .child(kit::icon(IconName::Key, 13., p.warn))
                    .child(div().flex_none().text_size(px(TypeScale::SM)).font_weight(FontWeight::SEMIBOLD).child(t("structure.field.primary")))
                    .child(div().flex_1().min_w_0().child(chips))
                    .children(add_menu),
            );
        if !order.is_empty() {
            strip = strip.child(kit::muted(t("structure.pk.notNullNote"), p));
        }
        if self.primary_key_changed() {
            strip = strip.child(
                div()
                    .id("structure-pk-changed")
                    .test_support()
                    .text_size(px(TypeScale::SM))
                    .text_color(p.warn)
                    .child(t("structure.pk.changed")),
            );
        }
        for index in self.auto_outside_primary_key() {
            let name = self.session.kept_columns[index].name.trim().to_owned();
            let owner = entity.clone();
            strip = strip.child(
                div()
                    .id(SharedString::from(format!("structure-pk-auto-{index}")))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .child(kit::icon(IconName::TriangleAlert, 12., p.danger))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(TypeScale::SM))
                            .text_color(p.danger)
                            .child(SharedString::from(i18n::fill(&i18n::text(locale, "structure.diag.autoNotPk"), &[("name", &name)]))),
                    )
                    .child(kit::secondary_button(SharedString::from(format!("structure-pk-make-{index}")), Some(IconName::Key), t("structure.pk.makePrimary"), move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_primary(index, true);
                            cx.notify();
                        });
                    })),
            );
        }
        strip.into_any_element()
    }
}
