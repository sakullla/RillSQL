//! ENUM/SET 候选值、生成列与分区方案的编辑面板。
//!
//! 面板只改草稿：语句仍由 ddl::plan 与方言生成，执行仍走既有的确认执行链。
//! 这里只给适用的操作提供输入；不适用的操作由 table_ddl::plan_partition 在预览时阻断，
//! 不会因为界面上有个按钮就假装可用。

use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use super::{check_box, StructurePane};
use crate::ddl::model::{EnumSetDraft, EnumSetKind, GeneratedColumnDraft, GeneratedStorage, PartitionDef, PartitionMethod, PartitionOp};
use crate::i18n::{self, Locale};
use crate::ui::kit::{self, Palette, Space, TypeScale};

/// 一个字段行的候选值输入框最多渲染多少个：编号按这个步长分段。
const ENUM_VALUE_STRIDE: usize = 64;
/// 字段行下标的上限：候选值编号 = 字段行 × 步长 + 值序号，必须留在同一个 10000 的组里。
const ENUM_COLUMN_LIMIT: usize = 10_000 / ENUM_VALUE_STRIDE;

/// 面板里的固定宽度与缩进都按 Space 令牌的倍数算，不写字面像素值。
const PANEL_INDENT: f32 = Space::XXL + Space::XS;
const VALUE_INDEX_W: f32 = Space::XXL - Space::XXS;
const NAME_FIELD_W: f32 = Space::XXL * 8.;
const FIELD_WIDE_W: f32 = Space::XXL * 17.;
const MENU_WIDE_W: f32 = Space::XXL * 9.;
const MENU_MAX_W: f32 = Space::XXL * 10.;

impl StructurePane {
    // ---- ENUM / SET ----

    /// 把这一列改成 ENUM 或 SET：已有候选值原样保留，没有时给一个空值占位。
    pub(super) fn set_enum_set_kind(&mut self, index: usize, kind: EnumSetKind) {
        let changed = self.edit_column(index, |draft| {
            let values = match draft.enum_set.take() {
                Some(state) if !state.values.is_empty() => state.values,
                _ => vec![String::new()],
            };
            draft.enum_set = Some(EnumSetDraft { kind, values });
            true
        });
        if changed {
            self.session.cancel();
        }
    }

    /// 去掉 ENUM/SET 定义，回到按类型原文写出的普通列。
    pub(super) fn drop_enum_set(&mut self, index: usize) {
        let changed = self.edit_column(index, |draft| draft.enum_set.take().is_some());
        if changed {
            self.session.cancel();
        }
    }

    /// 在末尾追加一个空候选值并聚焦它。顺序就是定义顺序，只能这样往后加。
    pub(super) fn add_enum_value(&mut self, index: usize) {
        let mut focus = None;
        let changed = self.edit_column(index, |draft| match draft.enum_set.as_mut() {
            Some(state) if state.values.len() < ENUM_VALUE_STRIDE => {
                state.values.push(String::new());
                focus = Some(index * ENUM_VALUE_STRIDE + state.values.len() - 1);
                true
            }
            _ => false,
        });
        if changed {
            self.session.cancel();
            self.pending_focus_draft = focus.map(|slot| 90_000 + slot as u32);
        }
    }

    pub(super) fn remove_enum_value(&mut self, index: usize, position: usize) {
        let changed = self.edit_column(index, |draft| match draft.enum_set.as_mut() {
            // 至少留一个候选值：空列表本来就会被服务器拒绝，界面不制造这种草稿。
            Some(state) if state.values.len() > 1 && position < state.values.len() => {
                state.values.remove(position);
                true
            }
            _ => false,
        });
        if changed {
            self.session.cancel();
        }
    }

    pub(super) fn move_enum_value(&mut self, index: usize, position: usize, delta: isize) {
        let changed = self.edit_column(index, |draft| {
            let Some(state) = draft.enum_set.as_mut() else {
                return false;
            };
            let next = position as isize + delta;
            if next < 0 || next as usize >= state.values.len() {
                return false;
            }
            state.values.swap(position, next as usize);
            true
        });
        if changed {
            self.session.cancel();
        }
    }

    // ---- 生成列 ----

    /// 打开或关闭这一列的生成属性。打开时清掉自增与默认值：生成列不能同时有它们。
    pub(super) fn toggle_generated(&mut self, index: usize) {
        let changed = self.edit_column(index, |draft| {
            if draft.generated.is_some() {
                draft.generated = None;
            } else {
                draft.generated = Some(GeneratedColumnDraft { expression: String::new(), storage: GeneratedStorage::Virtual });
                draft.default = crate::ddl::DefaultSpec::None;
                draft.auto = false;
            }
            true
        });
        if changed {
            self.session.cancel();
            self.pending_focus_draft = Some(100_000 + index as u32);
        }
    }

    pub(super) fn set_generated_storage(&mut self, index: usize, storage: GeneratedStorage) {
        let changed = self.edit_column(index, |draft| match draft.generated.as_mut() {
            Some(generated) if generated.storage != storage => {
                generated.storage = storage;
                true
            }
            _ => false,
        });
        if changed {
            self.session.cancel();
        }
    }

    // ---- 分区 ----

    /// 切换分区操作。维护类操作只能按现有分区方式执行，所以方式跟着读回的方案走。
    pub(super) fn set_partition_op(&mut self, op: PartitionOp) {
        if self.session.partition.op == op {
            return;
        }
        self.session.partition.op = op;
        self.session.partition.targets.clear();
        if op != PartitionOp::Replace {
            if let Some(current) = self.snapshot.as_ref().and_then(|snapshot| snapshot.partitions.as_ref()) {
                self.session.partition.method = current.method;
            }
        }
        self.session.cancel();
    }

    pub(super) fn set_partition_method(&mut self, method: PartitionMethod) {
        if self.session.partition.method == method {
            return;
        }
        self.session.partition.method = method;
        // 方式换了，上一份分区定义与 KEY 列清单都不再适用，清掉避免生成串味的语句。
        self.session.partition.definitions.clear();
        self.session.partition.columns.clear();
        if method.named() {
            self.session.partition.count.clear();
        }
        self.session.cancel();
    }

    pub(super) fn add_partition_def(&mut self) {
        let len = self.session.partition.definitions.len();
        if len >= 64 {
            return;
        }
        self.session.partition.definitions.push(PartitionDef { name: format!("p{len}"), description: String::new() });
        self.session.cancel();
        self.pending_focus_draft = Some(110_000 + 10 + (len as u32) * 2);
    }

    pub(super) fn remove_partition_def(&mut self, position: usize) {
        if position >= self.session.partition.definitions.len() {
            return;
        }
        self.session.partition.definitions.remove(position);
        self.session.cancel();
    }

    /// 选中或取消一个现有分区（删除与重组的对象）。
    pub(super) fn toggle_partition_target(&mut self, name: &str) {
        let targets = &mut self.session.partition.targets;
        match targets.iter().position(|item| item == name) {
            Some(at) => {
                targets.remove(at);
            }
            None => targets.push(name.to_owned()),
        }
        self.session.cancel();
    }
}
impl StructurePane {
    /// 编辑中那一行下面的专有形态面板。没有 ENUM/SET 也没有生成属性时不给面板。
    pub(super) fn column_special_editor(
        &mut self,
        index: usize,
        column: &crate::ddl::ColumnDraft,
        engine: &str,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if column.enum_set.is_none() && column.generated.is_none() {
            return None;
        }
        let mut panel = div()
            .id(SharedString::from(format!("structure-special-{index}")))
            .test_support()
            .flex()
            .flex_col()
            .gap(px(Space::SM))
            .pl(px(PANEL_INDENT))
            .pr(px(Space::LG))
            .py(px(Space::SM))
            .border_b_1()
            .border_color(p.line_soft)
            .bg(p.sunken);
        if column.enum_set.is_some() {
            panel = panel.child(self.enum_set_section(index, column, p, window, cx));
        }
        if let Some(generated) = column.generated.as_ref() {
            panel = panel.child(self.generated_section(index, generated.clone(), engine, p, window, cx));
        }
        Some(panel.into_any_element())
    }

    fn enum_set_section(
        &mut self,
        index: usize,
        column: &crate::ddl::ColumnDraft,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let Some(state) = column.enum_set.clone() else {
            return div().into_any_element();
        };
        let entity = cx.entity();
        let metrics = kit::Metrics::of(cx);
        let encodable = index < ENUM_COLUMN_LIMIT;
        let kind_items = EnumSetKind::ALL
            .into_iter()
            .map(|kind| {
                let owner = entity.clone();
                (
                    SharedString::from(format!("structure-enum-kind-{index}-{}", kind.keyword())),
                    SharedString::from(kind.keyword().to_uppercase()),
                    state.kind == kind,
                    kit::click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_enum_set_kind(index, kind);
                            cx.notify();
                        });
                    }),
                )
            })
            .collect();
        let mut head = div()
            .flex()
            .items_center()
            .gap(px(Space::SM))
            .flex_wrap()
            .child(kit::segmented_sized(kind_items, p, metrics.control_h));
        let drop_owner = entity.clone();
        head = head
            .child(kit::eyebrow(t("structure.enumSetValues"), p))
            .child(div().flex_1())
            .child(
                kit::icon_button(
                    SharedString::from(format!("structure-enum-drop-{index}")),
                    IconName::X,
                    t("structure.enumSetRemove"),
                    move |_, _, cx| {
                        drop_owner.update(cx, |pane, cx| {
                            pane.drop_enum_set(index);
                            cx.notify();
                        });
                    },
                )
                .xsmall(),
            );

        let mut list = div().flex().flex_col().gap(px(Space::XS));
        let count = state.values.len();
        for (position, value) in state.values.iter().enumerate() {
            let input = if encodable {
                self.draft_input(90_000 + (index * ENUM_VALUE_STRIDE + position) as u32, value, "", window, cx)
            } else {
                div().text_size(px(TypeScale::SM)).child(SharedString::from(value.clone())).into_any_element()
            };
            let up_owner = entity.clone();
            let down_owner = entity.clone();
            let remove_owner = entity.clone();
            let row = div()
                .flex()
                .items_center()
                .gap(px(Space::XS))
                .child(
                    div()
                        .w(px(VALUE_INDEX_W))
                        .flex_none()
                        .text_size(px(TypeScale::XS))
                        .text_color(p.text_faint)
                        .child(SharedString::from((position + 1).to_string())),
                )
                .child(div().flex_1().min_w_0().child(input))
                .child(
                    kit::icon_button(
                        SharedString::from(format!("structure-enum-up-{index}-{position}")),
                        IconName::ChevronUp,
                        t("structure.field.moveUp"),
                        move |_, _, cx| {
                            up_owner.update(cx, |pane, cx| {
                                pane.move_enum_value(index, position, -1);
                                cx.notify();
                            });
                        },
                    )
                    .xsmall()
                    .disabled(position == 0),
                )
                .child(
                    kit::icon_button(
                        SharedString::from(format!("structure-enum-down-{index}-{position}")),
                        IconName::ChevronDown,
                        t("structure.field.moveDown"),
                        move |_, _, cx| {
                            down_owner.update(cx, |pane, cx| {
                                pane.move_enum_value(index, position, 1);
                                cx.notify();
                            });
                        },
                    )
                    .xsmall()
                    .disabled(position + 1 >= count),
                )
                .child(
                    kit::icon_button(
                        SharedString::from(format!("structure-enum-remove-{index}-{position}")),
                        IconName::X,
                        t("structure.enumValueRemove"),
                        move |_, _, cx| {
                            remove_owner.update(cx, |pane, cx| {
                                pane.remove_enum_value(index, position);
                                cx.notify();
                            });
                        },
                    )
                    .xsmall()
                    .disabled(count <= 1),
                );
            list = list.child(row);
        }
        let add_owner = entity.clone();
        let add = kit::tool_button(
            SharedString::from(format!("structure-enum-add-{index}")),
            Some(IconName::Plus),
            t("structure.enumValueAdd"),
            move |_, _, cx| {
                add_owner.update(cx, |pane, cx| {
                    pane.add_enum_value(index);
                    cx.notify();
                });
            },
        );
        div().flex().flex_col().gap(px(Space::SM)).child(head).child(kit::muted(t("structure.enumSetOrderHint"), p)).child(list).child(add).into_any_element()
    }

    fn generated_section(
        &mut self,
        index: usize,
        generated: GeneratedColumnDraft,
        _engine: &str,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let hint = i18n::text(locale, "structure.generatedExpressionHint");
        let expression = self.draft_input(100_000 + index as u32, &generated.expression, &hint, window, cx);
        let label = storage_label(generated.storage);
        let current = generated.storage;
        let storage_entity = entity.clone();
        let storage = kit::inline_menu_button(SharedString::from(format!("structure-generated-storage-{index}")))
            .label(SharedString::from(label))
            .dropdown_menu(move |menu, _, _| {
                let menu_owner = storage_entity.clone();
                let mut menu = menu.min_w(px(MENU_WIDE_W));
                for choice in [GeneratedStorage::Virtual, GeneratedStorage::Stored] {
                    let owner = menu_owner.clone();
                    let text = storage_label(choice);
                    menu = menu.item(PopupMenuItem::new(text).checked(choice == current).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_generated_storage(index, choice);
                            cx.notify();
                        });
                    }));
                }
                menu
            });
        let remove_owner = entity.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(Space::SM))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(Space::SM))
                    .child(kit::eyebrow(t("structure.generatedColumn"), p))
                    .child(kit::muted(t("structure.generatedReadOnlyHint"), p))
                    .child(div().flex_1())
                    .child(
                        kit::icon_button(
                            SharedString::from(format!("structure-generated-drop-{index}")),
                            IconName::X,
                            t("structure.generatedRemove"),
                            move |_, _, cx| {
                                remove_owner.update(cx, |pane, cx| {
                                    pane.toggle_generated(index);
                                    cx.notify();
                                });
                            },
                        )
                        .xsmall(),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(Space::MD))
                    .child(div().flex_1().min_w_0().child(kit::form_row(t("structure.generatedExpression"), expression, p)))
                    .child(kit::form_row(t("structure.generatedStorage"), storage, p)),
            )
            .child(kit::muted(t("structure.generatedStorageHint"), p))
            .into_any_element()
    }
}

/// 存储方式的显示名。用哪种关键字写进语句是方言的事（MariaDB 写 PERSISTENT，
/// 与 STORED 同义），界面只说清语义，见 structure.generatedStorageHint。
fn storage_label(storage: GeneratedStorage) -> String {
    match storage {
        GeneratedStorage::Virtual => "VIRTUAL".to_owned(),
        GeneratedStorage::Stored => "STORED".to_owned(),
    }
}
impl StructurePane {
    /// 分区方案表单：方式、操作、表达式或列、数量与分区定义分开输入。
    pub(super) fn partition_form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = Palette::of(cx);
        let metrics = kit::Metrics::of(cx);
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let entity = cx.entity();
        let op = self.session.partition.op;
        let method = self.session.partition.method;
        let current = self.snapshot.as_ref().and_then(|snapshot| snapshot.partitions.clone());
        let partition_error = self.snapshot.as_ref().and_then(|snapshot| snapshot.partition_error.clone());

        let mut form = kit::card(&p)
            .id("structure-partition-form")
            .test_support()
            .p(px(metrics.card_pad()))
            .flex()
            .flex_col()
            .gap(px(Space::MD + Space::XXS));

        // 现状：读回的分区方案（读失败时照实说读不到，不当作未分区）。
        let current_text = match (&current, &partition_error) {
            (Some(state), _) => {
                let expression = if state.expression.trim().is_empty() { method_columns_hint(state.method) } else { state.expression.clone() };
                i18n::fill(
                    &i18n::text(locale, "structure.partitionSummary"),
                    &[("method", state.method.keyword()), ("expression", &expression), ("count", &state.definitions.len().to_string())],
                )
            }
            (None, Some(error)) => i18n::fill(&i18n::text(locale, "structure.partitionUnreadable"), &[("error", error)]),
            (None, None) => i18n::text(locale, "structure.partitionNone"),
        };
        form = form.child(
            kit::sunken(&p)
                .px(px(Space::MD))
                .py(px(Space::SM))
                .flex()
                .items_center()
                .flex_wrap()
                .gap(px(Space::SM))
                .child(kit::eyebrow(t("structure.partitionCurrent"), &p))
                .child(div().text_size(px(TypeScale::SM)).text_color(p.text).child(SharedString::from(current_text)))
                .when(partition_error.is_some(), |this| this.child(kit::pill(t("structure.partitionReadFailed"), p.danger, p.danger_soft))),
        );

        // 操作：哪些操作有意义由预览判定，这里只列出这一轮支持的操作。
        let op_owner = entity.clone();
        let op_label = partition_op_label(op, locale);
        let op_menu = kit::menu_button("structure-partition-op")
            .label(SharedString::from(op_label.clone()))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(MENU_MAX_W));
                for choice in [PartitionOp::Replace, PartitionOp::Add, PartitionOp::Drop, PartitionOp::Reorganize, PartitionOp::Remove] {
                    let owner = op_owner.clone();
                    let label = partition_op_label(choice, locale);
                    menu = menu.item(PopupMenuItem::new(label).checked(choice == op).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_partition_op(choice);
                            cx.notify();
                        });
                    }));
                }
                menu
            });

        // 分区方式：重建方案时可选；维护类操作只能按现有方案，界面锁住并说明原因。
        let method_locked = op != PartitionOp::Replace;
        let method_owner = entity.clone();
        let method_text = method.keyword().to_owned();
        let locked_hint = t("structure.partitionMethodLocked");
        let method_button: AnyElement = if method_locked {
            kit::menu_button("structure-partition-method")
                .label(SharedString::from(method_text))
                .disabled(true)
                .tooltip(locked_hint)
                .into_any_element()
        } else {
            kit::menu_button("structure-partition-method")
                .label(SharedString::from(method_text))
                .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(NAME_FIELD_W));
                for choice in PartitionMethod::ALL {
                    let owner = method_owner.clone();
                    let label = choice.keyword().to_owned();
                    menu = menu.item(PopupMenuItem::new(label).checked(choice == method).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.set_partition_method(choice);
                            cx.notify();
                        });
                    }));
                }
                menu
                })
                .into_any_element()
        };
        form = form.child(
            div()
                .flex()
                .items_end()
                .flex_wrap()
                .gap(px(Space::LG))
                .child(kit::form_row(t("structure.partitionOperation"), op_menu, &p))
                .child(kit::form_row(t("structure.partitionMethod"), method_button, &p)),
        );

        if method_locked {
            form = form.child(kit::muted(t("structure.partitionMethodLockedHint"), &p));
        }

        // 表达式：重建 RANGE/LIST/HASH 与 KEY 的列清单分开输入。
        if op == PartitionOp::Replace {
            if method == PartitionMethod::Key {
                let hint = i18n::text(locale, "structure.partitionColumnsHint");
                let input = self.draft_input(110_002, &self.session.partition_columns.clone(), &hint, window, cx);
                form = form.child(div().max_w(px(FIELD_WIDE_W)).child(kit::form_row(t("structure.partitionColumns"), input, &p)));
            } else {
                let hint = i18n::text(locale, "structure.partitionExpressionHint");
                let input = self.draft_input(110_000, &self.session.partition.expression.clone(), &hint, window, cx);
                form = form.child(div().max_w(px(FIELD_WIDE_W)).child(kit::form_row(t("structure.partitionExpression"), input, &p)));
            }
        }

        // HASH/KEY 按数量分：只有这三种操作用到数量。
        if !method.named() && matches!(op, PartitionOp::Replace | PartitionOp::Add | PartitionOp::Drop) {
            let count_hint = i18n::text(locale, "structure.partitionCountHint");
            let input = self.draft_input(110_001, &self.session.partition.count.clone(), &count_hint, window, cx);
            form = form.child(div().max_w(px(MENU_WIDE_W)).child(kit::form_row(t("structure.partitionCount"), input, &p)));
        }

        // 命名分区：重建、新增与重组都要写名字与边界。
        if method.named() && matches!(op, PartitionOp::Replace | PartitionOp::Add | PartitionOp::Reorganize) {
            let mut rows = div().flex().flex_col().gap(px(Space::XS));
            let definitions = self.session.partition.definitions.clone();
            for (position, def) in definitions.iter().enumerate() {
                let name_hint = i18n::text(locale, "structure.partitionNameHint");
                let name = self.draft_input(110_010 + (position as u32) * 2, &def.name, &name_hint, window, cx);
                let boundary_hint = if method == PartitionMethod::Range {
                    i18n::text(locale, "structure.partitionBoundaryRangeHint")
                } else {
                    i18n::text(locale, "structure.partitionBoundaryListHint")
                };
                let boundary = self.draft_input(110_011 + (position as u32) * 2, &def.description, &boundary_hint, window, cx);
                let remove_owner = entity.clone();
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(Space::SM))
                        .child(div().w(px(NAME_FIELD_W)).flex_none().child(name))
                        .child(div().flex_1().min_w_0().child(boundary))
                        .child(
                            kit::icon_button(
                                SharedString::from(format!("structure-partition-def-remove-{position}")),
                                IconName::X,
                                t("structure.partitionDefRemove"),
                                move |_, _, cx| {
                                    remove_owner.update(cx, |pane, cx| {
                                        pane.remove_partition_def(position);
                                        cx.notify();
                                    });
                                },
                            )
                            .xsmall(),
                        ),
                );
            }
            let add_owner = entity.clone();
            form = form
                .child(kit::eyebrow(t("structure.partitionDefinitions"), &p))
                .child(rows)
                .child(kit::tool_button(
                    "structure-partition-def-add",
                    Some(IconName::Plus),
                    t("structure.partitionDefAdd"),
                    move |_, _, cx| {
                        add_owner.update(cx, |pane, cx| {
                            pane.add_partition_def();
                            cx.notify();
                        });
                    },
                ));
        }

        // 删除与重组要先选现有分区：没有分区可选的表在这里就看得出来。
        if matches!(op, PartitionOp::Drop | PartitionOp::Reorganize) {
            let mut targets = div().flex().flex_wrap().gap(px(Space::SM));
            match current.as_ref() {
                Some(state) if !state.definitions.is_empty() => {
                    for def in &state.definitions {
                        let selected = self.session.partition.targets.iter().any(|name| name == &def.name);
                        let owner = entity.clone();
                        let name = def.name.clone();
                        targets = targets.child(
                            check_box(
                                SharedString::from(format!("structure-partition-target-{}", def.name)),
                                selected,
                                true,
                                SharedString::from(if def.description.trim().is_empty() {
                                    def.name.clone()
                                } else {
                                    format!("{} · {}", def.name, def.description)
                                }),
                                &p,
                            )
                            .on_click(move |_, _, cx| {
                                owner.update(cx, |pane, cx| {
                                    pane.toggle_partition_target(&name);
                                    cx.notify();
                                });
                            }),
                        );
                    }
                }
                _ => {
                    targets = targets.child(kit::muted(t("structure.partitionNoTargets"), &p));
                }
            }
            form = form.child(kit::form_row(t("structure.partitionTargets"), targets, &p));
        }

        // 后果提醒先在表单里说清楚，确认层的风险文案来自同一份计划。
        let (level, note) = match op {
            PartitionOp::Replace => ("danger", "structure.partitionRiskReplace"),
            PartitionOp::Drop => ("danger", "structure.partitionRiskDrop"),
            PartitionOp::Reorganize => ("warn", "structure.partitionRiskReorganize"),
            PartitionOp::Remove => ("warn", "structure.partitionRiskRemove"),
            PartitionOp::Add => ("info", "structure.partitionRiskAdd"),
        };
        let color = match level {
            "danger" => p.danger,
            "warn" => p.warn,
            _ => p.text_muted,
        };
        form = form.child(
            div()
                .flex()
                .items_center()
                .gap(px(Space::SM))
                .child(kit::icon(IconName::TriangleAlert, 13., color))
                .child(div().text_size(px(TypeScale::SM)).text_color(color).child(t(note))),
        );
        form.into_any_element()
    }
}

/// 分区操作的中文/英文名（下拉与按钮共用）。
fn partition_op_label(op: PartitionOp, locale: Locale) -> String {
    let key = match op {
        PartitionOp::Replace => "structure.partitionOpReplace",
        PartitionOp::Add => "structure.partitionOpAdd",
        PartitionOp::Drop => "structure.partitionOpDrop",
        PartitionOp::Reorganize => "structure.partitionOpReorganize",
        PartitionOp::Remove => "structure.partitionOpRemove",
    };
    i18n::text(locale, key).to_owned()
}

fn method_columns_hint(method: PartitionMethod) -> String {
    if method == PartitionMethod::Key {
        "键列".to_owned()
    } else {
        String::new()
    }
}
