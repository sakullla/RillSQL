//! 新建表页（R2）与常用字段（R19）：默认值模式下拉、类型组合框、表级选项折叠区、字段拖拽与键盘排序、
//! 常用字段菜单与模板管理面板。方言差异（类型候选、表达式白名单、表级选项）只经适配器的类型规则与 DDL 方言取得。

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div, px, relative,
};

use crate::adapters::ddl::{ddl_named, types_named, TableOptionSpec};
use crate::adapters::registry;
use crate::ddl::templates::{is_builtin_id, ColumnTemplate, TemplateColumn, TemplateProblem, BUILTIN};
use crate::ddl::{DefaultMode, DefaultSpec};
use crate::i18n::{self, Locale};
use crate::ui::kit::{self, FieldSpec, Palette, Radius, TypeScale};
use crate::ui::kit::ScrollAreaExt as _;

use super::{check_box, default_column_type, on_draft, DraftIssue, IssueField, StructureMode, StructurePane};

/// 拖动字段行时携带的源行号。
#[derive(Clone, Debug)]
pub(super) struct ColumnDrag {
    pub index: usize,
    pub label: SharedString,
}

/// 拖动时跟随指针的字段名标签。
pub(super) struct ColumnDragGhost {
    pub label: SharedString,
}

impl Render for ColumnDragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::of(cx);
        div()
            .px(px(8.))
            .py(px(3.))
            .rounded(px(Radius::TAG))
            .bg(p.raised)
            .border_1()
            .border_color(p.accent)
            .text_size(px(TypeScale::SM))
            .text_color(p.text)
            .child(self.label.clone())
    }
}

/// 常用字段管理面板的状态：正在编辑的用户模板副本与上一次保存的校验结果。
#[derive(Clone, Debug, Default)]
pub(super) struct TemplateManager {
    pub draft: Option<ColumnTemplate>,
    pub problem: Option<TemplateProblem>,
    pub saved: bool,
}

/// 默认值模式在菜单与按钮上的文字。
fn mode_key(mode: DefaultMode, short: bool) -> &'static str {
    match (mode, short) {
        (DefaultMode::None, false) => "structure.default.none",
        (DefaultMode::None, true) => "structure.default.noneShort",
        (DefaultMode::Null, _) => "structure.default.null",
        (DefaultMode::Literal, false) => "structure.default.literal",
        (DefaultMode::Literal, true) => "structure.default.literalShort",
        (DefaultMode::Expression, false) => "structure.default.expression",
        (DefaultMode::Expression, true) => "structure.default.expressionShort",
    }
}

/// 默认值模式下拉：四种模式（NULL 只给可空字段）与表达式白名单。`pick` 收到新的默认值。
pub(super) fn default_menu(
    menu: PopupMenu,
    current: &DefaultSpec,
    nullable: bool,
    expressions: &'static [&'static str],
    locale: Locale,
    pick: impl Fn(DefaultSpec, &mut App) + Clone + 'static,
) -> PopupMenu {
    let mut menu = menu.min_w(px(200.));
    for mode in DefaultMode::ALL {
        let next = current.with_mode(mode);
        let pick = pick.clone();
        menu = menu.item(
            PopupMenuItem::new(i18n::text(locale, mode_key(mode, false)))
                .checked(current.mode() == mode)
                .disabled(mode == DefaultMode::Null && !nullable)
                .on_click(move |_, _, cx| pick(next.clone(), cx)),
        );
    }
    if !expressions.is_empty() {
        menu = menu.separator().label(i18n::text(locale, "structure.default.expressions"));
        for expression in expressions {
            let value = DefaultSpec::Expression((*expression).to_owned());
            let checked = *current == value;
            let pick = pick.clone();
            menu = menu.item(PopupMenuItem::new(*expression).checked(checked).on_click(move |_, _, cx| pick(value.clone(), cx)));
        }
    }
    menu
}

/// 类型组合框的下拉：按已输入文字筛选的常用类型，以及带参数的类型片段（选中写入示例参数）。
pub(super) fn type_menu(menu: PopupMenu, engine: &str, current: &str, locale: Locale, pick: impl Fn(String, &mut App) + Clone + 'static) -> PopupMenu {
    let types = types_named(engine);
    let needle = current.trim().to_lowercase();
    let exact = types.type_choices().iter().any(|item| item.eq_ignore_ascii_case(current.trim()))
        || types.param_types().iter().any(|item| item.insert.eq_ignore_ascii_case(current.trim()));
    let hit = |text: &str| exact || needle.is_empty() || text.to_lowercase().contains(&needle);
    let common: Vec<&str> = types.type_choices().iter().copied().filter(|item| hit(item)).collect();
    let params: Vec<_> = types.param_types().iter().filter(|item| hit(item.label) || hit(item.insert)).collect();
    let mut menu = menu.min_w(px(220.)).max_w(px(420.)).max_h(px(360.)).scrollable(true);
    if common.is_empty() && params.is_empty() {
        return menu
            .item(PopupMenuItem::label(i18n::text(locale, "structure.choice.noMatch")))
            .item(PopupMenuItem::label(i18n::text(locale, "structure.choice.noMatchHint")));
    }
    if !common.is_empty() {
        menu = menu.label(i18n::text(locale, "structure.type.common"));
        for item in common {
            let value = item.to_owned();
            let pick = pick.clone();
            menu = menu.item(PopupMenuItem::new(item).checked(item.eq_ignore_ascii_case(current.trim())).on_click(move |_, _, cx| pick(value.clone(), cx)));
        }
    }
    if !params.is_empty() {
        menu = menu.separator().label(i18n::text(locale, "structure.type.params"));
        for item in params {
            let value = item.insert.to_owned();
            let pick = pick.clone();
            menu = menu.item(PopupMenuItem::new(format!("{}  →  {}", item.label, item.insert)).on_click(move |_, _, cx| pick(value.clone(), cx)));
        }
    }
    menu
}

impl StructurePane {
    // ---- 字段：默认值、移动、键盘 ----

    pub(super) fn set_column_default(&mut self, index: usize, value: DefaultSpec) {
        let changed = self.edit_column(index, |column| {
            if column.default == value {
                return false;
            }
            column.default = value;
            true
        });
        if changed {
            if matches!(self.session.kept_columns.get(index).map(|column| column.default.mode()), Some(DefaultMode::Literal | DefaultMode::Expression)) {
                self.pending_focus_draft = Some(30_000 + index as u32);
            }
            self.session.cancel();
        }
    }

    /// 把一行字段移到另一行的位置（拖放）。只在新建表里可用。
    pub fn move_column_to(&mut self, from: usize, to: usize) {
        if self.session.mode != StructureMode::Create || from == to {
            return;
        }
        let len = self.session.kept_columns.len();
        if from >= len || to >= len {
            return;
        }
        let column = self.session.kept_columns.remove(from);
        self.session.kept_columns.insert(to, column);
        self.session.cancel();
    }

    /// 键盘焦点所在的字段行（名称、类型、默认值或注释框）。
    pub(super) fn focused_column(&self, window: &Window, cx: &App) -> Option<(usize, u32)> {
        (0..self.session.kept_columns.len()).find_map(|index| {
            [10_000u32, 20_000, 30_000, 40_000]
                .into_iter()
                .find(|group| self.draft_fields.is_focused(group + index as u32, window, cx))
                .map(|group| (index, group))
        })
    }

    /// 新建表页的键盘操作：Alt+↑/↓ 移动当前字段，Alt+P 主键，Alt+N 可空，Alt+A 自增。返回是否处理。
    pub(super) fn handle_create_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.session.mode != StructureMode::Create || self.session.preview_open() || self.template_manager.is_some() {
            return false;
        }
        let modifiers = &event.keystroke.modifiers;
        if !modifiers.alt || modifiers.control || modifiers.platform {
            return false;
        }
        let Some((index, group)) = self.focused_column(window, cx) else {
            return false;
        };
        match event.keystroke.key.as_str() {
            "up" | "down" => {
                let delta = if event.keystroke.key == "up" { -1 } else { 1 };
                let next = index as isize + delta;
                if next < 0 || next as usize >= self.session.kept_columns.len() {
                    return true;
                }
                self.move_column(index, delta);
                self.draft_fields.focus(group + next as u32, window, cx);
                true
            }
            "p" => {
                self.toggle_column(index, "primary");
                true
            }
            "n" => {
                if !self.session.kept_columns.get(index).is_some_and(|column| column.primary) {
                    self.toggle_column(index, "null");
                }
                true
            }
            "a" => {
                self.toggle_column(index, "auto");
                true
            }
            _ => false,
        }
    }

    /// 回车：新建表时表名 → 第一个字段名、字段名 → 下一行字段名（末行新增字段）；
    /// 已有表编辑态时确认当前行并推进到下一可见行，末行则退出编辑并保留行焦点。
    pub(super) fn draft_enter(&mut self, key: u32) {
        if self.session.mode == StructureMode::Existing {
            if matches!(key / 10_000, 1..=3) {
                let index = (key % 10_000) as usize;
                let visible = self.visible_column_indices();
                let next = visible
                    .iter()
                    .position(|&idx| idx == index)
                    .and_then(|pos| visible.get(pos + 1).copied());
                if let Some(next_idx) = next {
                    self.begin_edit_row(next_idx);
                    self.scroll_to_column = Some(next_idx);
                } else {
                    self.end_edit_row();
                    self.focused_row = Some(index);
                    self.pending_focus_pane = true;
                }
            }
            return;
        }
        if self.session.mode != StructureMode::Create {
            return;
        }
        if key == 4 {
            if !self.session.kept_columns.is_empty() {
                self.pending_focus_draft = Some(10_000);
            }
            return;
        }
        if key / 10_000 == 1 {
            let index = (key % 10_000) as usize;
            if index + 1 < self.session.kept_columns.len() {
                self.pending_focus_draft = Some(10_000 + index as u32 + 1);
            } else {
                self.add_column();
            }
        }
    }

    pub(super) fn draft_input_enter(&mut self, key: u32, value: &str, placeholder: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let state = self.draft_fields.bind_plain_enter(key, value, FieldSpec { placeholder, masked: false }, window, cx, on_draft, on_draft_enter);
        kit::text_input(SharedString::from(format!("structure-draft-{key}")), &state, placeholder.to_owned()).small().into_any_element()
    }

    /// 字段行的默认值单元：模式下拉 + 值输入（字面量/表达式才有输入框）。
    #[allow(clippy::too_many_arguments)]
    pub(super) fn default_cell(
        &mut self,
        index: usize,
        current: &DefaultSpec,
        nullable: bool,
        enabled: bool,
        engine: &str,
        entity: &Entity<Self>,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = self.session.locale;
        let owner = entity.clone();
        let expressions = types_named(engine).expression_choices();
        let current_menu = current.clone();
        let label = i18n::text(locale, mode_key(current.mode(), true));
        let mode_button = Button::new(SharedString::from(format!("structure-default-mode-{index}")))
            .ghost()
            .xsmall()
            .label(label)
            .icon(IconName::ChevronDown)
            .disabled(!enabled)
            .tooltip(i18n::text(locale, "structure.default.mode"))
            .dropdown_menu(move |menu, _, _| {
                let owner = owner.clone();
                default_menu(menu, &current_menu, nullable, expressions, locale, move |value, cx| {
                    owner.update(cx, |pane, cx| {
                        pane.set_column_default(index, value);
                        cx.notify();
                    });
                })
            });
        let value: AnyElement = match current.mode() {
            DefaultMode::Literal | DefaultMode::Expression => {
                let placeholder = i18n::text(locale, if current.mode() == DefaultMode::Literal { "structure.default.valueHint" } else { "structure.default.expressionHint" });
                self.draft_input_enter(30_000 + index as u32, current.text(), &placeholder, window, cx)
            }
            DefaultMode::Null => div().px(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child("NULL").into_any_element(),
            DefaultMode::None => div().px(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child("—").into_any_element(),
        };
        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .min_w_0()
            .child(div().flex_none().child(mode_button))
            .child(div().flex_1().min_w_0().child(value))
            .into_any_element()
    }

    /// 字段行下方就近显示的问题（该行没有问题时为 None）。
    pub(super) fn row_issue(&self, issues: &[DraftIssue], index: usize, p: &Palette) -> Option<AnyElement> {
        let issue = issues.iter().find(|issue| issue.row == Some(index))?;
        Some(issue_line(SharedString::from(format!("structure-row-issue-{index}")), &issue.text, p))
    }

    // ---- 表级选项 ----

    pub(super) fn set_table_option(&mut self, field: &str, value: String) {
        let options = &mut self.session.table_options;
        let slot = match field {
            "engine" => &mut options.engine,
            "charset" => &mut options.charset,
            "collation" => &mut options.collation,
            _ => return,
        };
        if *slot != value {
            *slot = value;
            // 换了字符集：不适用于新字符集的排序规则清回默认。
            if field == "charset" {
                let spec = ddl_named(&self.session.target.engine).map(|dialect| dialect.table_option_spec()).unwrap_or_default();
                if spec.collation_problem(&options.charset, &options.collation).is_some() {
                    options.collation.clear();
                }
            }
            self.session.cancel();
        }
    }

    pub(super) fn toggle_table_flag(&mut self, field: &str) {
        let options = &mut self.session.table_options;
        match field {
            "withoutRowid" => options.without_rowid = !options.without_rowid,
            "strict" => options.strict = !options.strict,
            _ => return,
        }
        self.session.cancel();
    }

    /// 表级选项折叠区：只列出该方言支持的选项（引擎、字符集、排序规则、表注释、WITHOUT ROWID、STRICT）。
    pub(super) fn table_options_panel(&mut self, entity: &Entity<Self>, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let engine = self.session.target.engine.clone();
        let spec: TableOptionSpec = ddl_named(&engine).map(|dialect| dialect.table_option_spec()).unwrap_or_default();
        if !spec.any() {
            return None;
        }
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let open = self.options_open;
        let toggle_owner = entity.clone();
        let options = self.session.table_options.clone();
        let mut summary = Vec::new();
        for value in [&options.engine, &options.charset, &options.collation] {
            if !value.trim().is_empty() {
                summary.push(value.trim().to_owned());
            }
        }
        for (on, label) in [(options.without_rowid, spec.without_rowid), (options.strict, spec.strict)] {
            if let (true, Some(label)) = (on, label) {
                summary.push(label.to_owned());
            }
        }
        let header = div()
            .id("structure-table-options-toggle")
            .test_support()
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .tab_index(0)
            .role(gpui_kit::Role::Button)
            .aria_label(t("structure.options.title"))
            .on_click(move |_, _, cx| {
                toggle_owner.update(cx, |pane, cx| {
                    pane.options_open = !pane.options_open;
                    cx.notify();
                })
            })
            .child(kit::icon(if open { IconName::ChevronDown } else { IconName::ChevronRight }, 13., p.text_muted))
            .child(kit::eyebrow(t("structure.options.title"), p))
            .when(!open && !summary.is_empty(), |this| this.child(kit::muted(SharedString::from(summary.join(" · ")), p)));
        let mut panel = div().id("structure-table-options").test_support().flex().flex_col().gap(px(8.)).child(header);
        if !open {
            return Some(panel.into_any_element());
        }
        let mut grid = div().flex().flex_wrap().items_end().gap(px(12.));
        for (field, key, choices, current) in [
            ("engine", "structure.options.engine", spec.engines, options.engine.clone()),
            ("charset", "structure.options.charset", spec.charsets, options.charset.clone()),
            ("collation", "structure.options.collation", spec.collations_for(&options.charset), options.collation.clone()),
        ] {
            if choices.is_empty() {
                continue;
            }
            let owner = entity.clone();
            let shown = if current.trim().is_empty() { i18n::text(locale, "structure.options.default") } else { current.clone() };
            let button = Button::new(SharedString::from(format!("structure-option-{field}")))
                .outline()
                .small()
                .label(shown)
                .icon(IconName::ChevronDown)
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.min_w(px(200.));
                    let clear_owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(i18n::text(locale, "structure.options.default")).checked(current.trim().is_empty()).on_click(move |_, _, cx| {
                        clear_owner.update(cx, |pane, cx| {
                            pane.set_table_option(field, String::new());
                            cx.notify();
                        })
                    }));
                    for choice in choices {
                        let value = (*choice).to_owned();
                        let owner = owner.clone();
                        menu = menu.item(PopupMenuItem::new(*choice).checked(current == *choice).on_click(move |_, _, cx| {
                            owner.update(cx, |pane, cx| {
                                pane.set_table_option(field, value.clone());
                                cx.notify();
                            })
                        }));
                    }
                    menu
                });
            grid = grid.child(div().min_w(px(160.)).child(kit::form_row(t(key), button, p)));
        }
        for (flag, on, label) in [("withoutRowid", options.without_rowid, spec.without_rowid), ("strict", options.strict, spec.strict)] {
            let Some(label) = label else {
                continue;
            };
            let label = SharedString::from(label);
            let owner = entity.clone();
            grid = grid.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(kit::field_h()))
                    .child(check_box(SharedString::from(format!("structure-option-{flag}")), on, true, label.clone(), p).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.toggle_table_flag(flag);
                            cx.notify();
                        })
                    }))
                    .child(div().text_size(px(TypeScale::SM)).font_family(cx.theme().mono_font_family.clone()).child(label)),
            );
        }
        panel = panel.child(grid);
        if spec.comment {
            let placeholder = i18n::text(locale, "structure.options.commentHint");
            let comment = self.draft_input(7, &options.comment, &placeholder, window, cx);
            panel = panel.child(div().max_w(px(560.)).child(kit::form_row(t("structure.options.comment"), comment, p)));
        }
        Some(panel.into_any_element())
    }

    /// 字段注释列是否显示（新建表且方言支持字段注释）。
    pub(super) fn shows_column_comments(&self) -> bool {
        self.session.mode == StructureMode::Create
            && ddl_named(&self.session.target.engine).is_some_and(|dialect| dialect.table_option_spec().column_comments)
    }

    // ---- 常用字段菜单 ----

    /// 常用字段按钮：新建表与修改表共用同一份模板列表；末项打开管理面板。
    pub(super) fn templates_button(&self, entity: &Entity<Self>) -> impl IntoElement + use<> {
        let locale = self.session.locale;
        let templates = self.available_templates();
        let owner = entity.clone();
        Button::new("structure-column-presets")
            .ghost()
            .small()
            .icon(IconName::Sparkles)
            .label(i18n::text(locale, "structure.field.presets"))
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(240.)).max_h(px(420.)).scrollable(true);
                for template in &templates {
                    let label = template_label(template, locale);
                    let id = template.id.clone();
                    let owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.apply_template(&id);
                            cx.notify();
                        })
                    }));
                }
                if templates.is_empty() {
                    menu = menu.item(PopupMenuItem::label(i18n::text(locale, "structure.templates.none")));
                }
                let manage_owner = owner.clone();
                menu.separator().item(PopupMenuItem::new(i18n::text(locale, "structure.templates.manage")).icon(IconName::Settings2).on_click(move |_, _, cx| {
                    manage_owner.update(cx, |pane, cx| {
                        pane.open_template_manager();
                        cx.notify();
                    })
                }))
            })
    }

    // ---- 模板管理 ----

    fn emit_templates(&self, cx: &mut Context<Self>) {
        if let Some(sink) = self.templates_sink.clone() {
            sink(self.templates.clone(), cx);
        }
    }

    pub fn open_template_manager(&mut self) {
        let draft = self.templates.custom.first().cloned();
        self.template_manager = Some(TemplateManager { draft, problem: None, saved: false });
        self.session.cancel();
    }

    pub fn close_template_manager(&mut self) {
        self.template_manager = None;
    }

    pub fn template_manager_open(&self) -> bool {
        self.template_manager.is_some()
    }

    fn manager_draft(&mut self) -> Option<&mut ColumnTemplate> {
        let manager = self.template_manager.as_mut()?;
        manager.saved = false;
        manager.problem = None;
        manager.draft.as_mut()
    }

    /// 新建一个用户模板（默认只适用于当前引擎），带一个空字段。
    pub fn new_template(&mut self) {
        let engine = registry::parse(&self.session.target.engine).map(|engine| crate::adapters::adapter(engine).meta().id().to_owned());
        let template = ColumnTemplate {
            id: format!("custom:{}", uuid::Uuid::new_v4()),
            name: String::new(),
            engines: engine.into_iter().collect(),
            columns: vec![TemplateColumn { type_name: default_column_type(&self.session.target.engine).to_owned(), nullable: true, ..TemplateColumn::default() }],
        };
        if self.template_manager.is_none() {
            self.open_template_manager();
        }
        if let Some(manager) = self.template_manager.as_mut() {
            manager.draft = Some(template);
            manager.problem = None;
            manager.saved = false;
        }
        self.pending_focus_draft = Some(8);
    }

    /// 用当前表草稿里的字段（不含主键与自增）新建模板。
    pub fn template_from_columns(&mut self) {
        self.new_template();
        let columns: Vec<TemplateColumn> = self
            .session
            .kept_columns
            .iter()
            .filter(|column| !column.dropped && !column.primary && !column.auto && !column.name.trim().is_empty())
            .map(|column| TemplateColumn {
                name: column.name.trim().to_owned(),
                type_name: column.type_name.trim().to_owned(),
                nullable: column.nullable,
                default: column.default.clone(),
                comment: column.comment.clone(),
            })
            .collect();
        if columns.is_empty() {
            return;
        }
        if let Some(draft) = self.manager_draft() {
            draft.columns = columns;
        }
    }

    pub fn select_template(&mut self, id: &str) {
        let draft = self.templates.find(id).cloned();
        if let Some(manager) = self.template_manager.as_mut() {
            manager.draft = draft;
            manager.problem = None;
            manager.saved = false;
        }
    }

    pub(super) fn write_template_name(&mut self, value: String) {
        if let Some(draft) = self.manager_draft() {
            draft.name = value;
        }
    }

    pub(super) fn write_template_cell(&mut self, group: u32, index: usize, value: String) {
        let Some(column) = self.manager_draft().and_then(|draft| draft.columns.get_mut(index)) else {
            return;
        };
        match group {
            5 => column.name = value,
            6 => column.type_name = value,
            7 => {
                column.default.set_text(value);
            }
            8 => column.comment = value,
            _ => {}
        }
    }

    pub fn template_column_default(&mut self, index: usize, value: DefaultSpec) {
        if let Some(column) = self.manager_draft().and_then(|draft| draft.columns.get_mut(index)) {
            if value == DefaultSpec::Null {
                column.nullable = true;
            }
            column.default = value;
        }
    }

    pub fn template_toggle_null(&mut self, index: usize) {
        if let Some(column) = self.manager_draft().and_then(|draft| draft.columns.get_mut(index)) {
            column.nullable = !column.nullable;
            if !column.nullable && column.default == DefaultSpec::Null {
                column.default = DefaultSpec::None;
            }
        }
    }

    pub fn template_add_column(&mut self) {
        let kind = default_column_type(&self.session.target.engine).to_owned();
        let Some(draft) = self.manager_draft() else {
            return;
        };
        draft.columns.push(TemplateColumn { type_name: kind, nullable: true, ..TemplateColumn::default() });
        let index = draft.columns.len() - 1;
        self.pending_focus_draft = Some(50_000 + index as u32);
    }

    pub fn template_remove_column(&mut self, index: usize) {
        if let Some(draft) = self.manager_draft() {
            if index < draft.columns.len() {
                draft.columns.remove(index);
            }
        }
    }

    pub fn template_move_column(&mut self, index: usize, delta: isize) {
        if let Some(draft) = self.manager_draft() {
            let next = index as isize + delta;
            if next >= 0 && (next as usize) < draft.columns.len() && index < draft.columns.len() {
                draft.columns.swap(index, next as usize);
            }
        }
    }

    /// 适用引擎：`None` 表示全部引擎（清空列表）；否则切换该引擎 id。
    pub fn template_toggle_engine(&mut self, engine: Option<&str>) {
        let Some(draft) = self.manager_draft() else {
            return;
        };
        match engine {
            None => draft.engines.clear(),
            Some(id) => {
                if let Some(at) = draft.engines.iter().position(|item| item == id) {
                    draft.engines.remove(at);
                } else {
                    draft.engines.push(id.to_owned());
                }
            }
        }
    }

    /// 保存正在编辑的模板。校验不通过时就近显示原因，不写入。
    pub fn save_template(&mut self, cx: &mut Context<Self>) -> Result<(), TemplateProblem> {
        let Some(draft) = self.template_manager.as_ref().and_then(|manager| manager.draft.clone()) else {
            return Ok(());
        };
        let result = self.templates.upsert(draft);
        if let Some(manager) = self.template_manager.as_mut() {
            manager.problem = result.err();
            manager.saved = result.is_ok();
        }
        if result.is_ok() {
            self.emit_templates(cx);
        }
        result
    }

    pub fn delete_template(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.templates.remove(id) {
            if let Some(manager) = self.template_manager.as_mut() {
                if manager.draft.as_ref().is_some_and(|draft| draft.id == id) {
                    manager.draft = self.templates.custom.first().cloned();
                    manager.problem = None;
                    manager.saved = false;
                }
            }
            self.emit_templates(cx);
        }
    }

    pub fn move_template(&mut self, id: &str, delta: isize, cx: &mut Context<Self>) {
        if self.templates.move_by(id, delta) {
            self.emit_templates(cx);
        }
    }

    pub fn toggle_builtin_hidden(&mut self, id: &str, cx: &mut Context<Self>) {
        let hidden = self.templates.is_hidden(id);
        self.templates.set_hidden(id, !hidden);
        self.emit_templates(cx);
    }

    /// 常用字段管理面板：左侧模板列表（内置可隐藏，用户模板可排序/删除），右侧编辑所选用户模板。
    #[inline(never)]
    pub(super) fn render_template_manager(&mut self, entity: &Entity<Self>, p: &Palette, metrics: kit::Metrics, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let manager = self.template_manager.clone()?;
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let engine = self.session.target.engine.clone();
        let viewport_w = f32::from(window.viewport_size().width);
        let stacked = (viewport_w > 0. && viewport_w <= 900.) || (self.content_width > 0. && self.content_width <= 900.);

        // 左栏（窄屏时为上栏）：内置模板与用户模板。
        let mut list = div()
            .id("structure-templates-list")
            .test_support()
            .flex()
            .flex_col()
            .gap(px(2.))
            .flex_none()
            .when(!stacked, |this| this.w(px(240.)))
            .when(stacked, |this| this.w_full().max_h(px(180.)))
            .scroll_area_y();
        list = list.child(div().pb(px(4.)).child(kit::eyebrow(t("structure.templates.builtin"), p)));
        for (id, key) in BUILTIN {
            let hidden = self.templates.is_hidden(id);
            let owner = entity.clone();
            let row_id = id.to_owned();
            list = list.child(
                div()
                    .id(SharedString::from(format!("structure-template-{id}")))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(4.))
                    .rounded(px(Radius::CONTROL))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(TypeScale::SM)).when(hidden, |this| this.text_color(p.text_faint).line_through()).child(t(key)))
                    .child(
                        kit::icon_button(
                            SharedString::from(format!("structure-template-hide-{id}")),
                            if hidden { IconName::EyeOff } else { IconName::Eye },
                            if hidden { t("structure.templates.show") } else { t("structure.templates.hide") },
                            move |_, _, cx| {
                                owner.update(cx, |pane, cx| {
                                    pane.toggle_builtin_hidden(&row_id, cx);
                                    cx.notify();
                                })
                            },
                        )
                        .xsmall(),
                    ),
            );
        }
        list = list.child(div().pt(px(10.)).pb(px(4.)).child(kit::eyebrow(t("structure.templates.custom"), p)));
        if self.templates.custom.is_empty() {
            list = list.child(div().px(px(8.)).child(kit::muted(t("structure.templates.emptyCustom"), p)));
        }
        let count = self.templates.custom.len();
        for (position, template) in self.templates.custom.iter().enumerate() {
            let selected = manager.draft.as_ref().is_some_and(|draft| draft.id == template.id);
            let (select_owner, up_owner, down_owner, delete_owner) = (entity.clone(), entity.clone(), entity.clone(), entity.clone());
            let (select_id, up_id, down_id, delete_id) = (template.id.clone(), template.id.clone(), template.id.clone(), template.id.clone());
            let applies = template.applies_to(&engine);
            list = list.child(
                div()
                    .id(SharedString::from(format!("structure-template-row-{position}")))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(Radius::CONTROL))
                    .cursor_pointer()
                    .when(selected, |this| this.bg(p.accent_soft))
                    .hover(|style| style.bg(p.hover))
                    .on_click(move |_, _, cx| {
                        select_owner.update(cx, |pane, cx| {
                            pane.select_template(&select_id);
                            cx.notify();
                        })
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().text_size(px(TypeScale::SM)).font_weight(FontWeight::MEDIUM).child(SharedString::from(template.name.clone())))
                            .child(div().truncate().text_size(px(TypeScale::XS)).text_color(if applies { p.text_faint } else { p.warn }).child(SharedString::from(engines_label(&template.engines, locale)))),
                    )
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-template-up-{position}")), IconName::ChevronUp, t("structure.field.moveUp"), move |_, _, cx| {
                            up_owner.update(cx, |pane, cx| {
                                pane.move_template(&up_id, -1, cx);
                                cx.notify();
                            })
                        })
                        .xsmall()
                        .disabled(position == 0),
                    )
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-template-down-{position}")), IconName::ChevronDown, t("structure.field.moveDown"), move |_, _, cx| {
                            down_owner.update(cx, |pane, cx| {
                                pane.move_template(&down_id, 1, cx);
                                cx.notify();
                            })
                        })
                        .xsmall()
                        .disabled(position + 1 >= count),
                    )
                    .child(
                        kit::icon_button(SharedString::from(format!("structure-template-delete-{position}")), IconName::Trash, t("structure.templates.delete"), move |_, _, cx| {
                            delete_owner.update(cx, |pane, cx| {
                                pane.delete_template(&delete_id, cx);
                                cx.notify();
                            })
                        })
                        .xsmall(),
                    ),
            );
        }
        let new_owner = entity.clone();
        let from_owner = entity.clone();
        list = list.child(
            div()
                .pt(px(8.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(kit::secondary_button("structure-template-new", Some(IconName::Plus), t("structure.templates.new"), move |_, _, cx| {
                    new_owner.update(cx, |pane, cx| {
                        pane.new_template();
                        cx.notify();
                    })
                }))
                .when(self.session.kept_columns.iter().any(|column| !column.primary && !column.auto && !column.dropped), |this| {
                    this.child(kit::tool_button("structure-template-from-columns", Some(IconName::ListPlus), t("structure.templates.fromColumns"), move |_, _, cx| {
                        from_owner.update(cx, |pane, cx| {
                            pane.template_from_columns();
                            cx.notify();
                        })
                    }))
                }),
        );

        // 右栏：编辑所选用户模板。
        let editor: AnyElement = match manager.draft.clone() {
            None => div().flex_1().flex().items_center().justify_center().child(kit::muted(t("structure.templates.pick"), p)).into_any_element(),
            Some(draft) => self.template_editor(&draft, &manager, entity, p, window, cx),
        };

        let close_owner = entity.clone();
        let scrim_owner = entity.clone();
        let done_owner = entity.clone();
        Some(
            div()
                .id("structure-templates-scrim")
                .absolute()
                .inset_0()
                .bg(gpui_kit::hsla(0., 0., 0., if p.dark { 0.45 } else { 0.25 }))
                .flex()
                .items_center()
                .justify_center()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    scrim_owner.update(cx, |pane, cx| {
                        pane.close_template_manager();
                        cx.notify();
                    })
                })
                .child(
                    div()
                        .id("structure-templates-dialog")
                        .test_support()
                        .role(gpui_kit::Role::Dialog)
                        .aria_label(t("structure.templates.title"))
                        .occlude()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .w(px(920.))
                        .max_w(relative(0.94))
                        .h(px(560.))
                        .max_h(relative(0.92))
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .bg(p.raised)
                        .border_1()
                        .border_color(p.line)
                        .rounded(px(Radius::PANEL))
                        .shadow_lg()
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .px(px(metrics.card_pad()))
                                .pt(px(metrics.card_pad()))
                                .pb(px(10.))
                                .border_b_1()
                                .border_color(p.line_soft)
                                .child(kit::icon(IconName::Sparkles, 16., p.accent))
                                .child(kit::title(t("structure.templates.title"), TypeScale::LG, p))
                                .child(div().flex_1())
                                .child(kit::icon_button("structure-templates-close", IconName::X, t("common.close"), move |_, _, cx| {
                                    close_owner.update(cx, |pane, cx| {
                                        pane.close_template_manager();
                                        cx.notify();
                                    })
                                })
                                .xsmall()),
                        )
                        .child(
                            div()
                                .id("structure-templates-body")
                                .test_support()
                                .flex_1()
                                .min_h_0()
                                .flex()
                                .when(stacked, |this| this.flex_col().gap(px(12.)))
                                .when(!stacked, |this| this.gap(px(16.)))
                                .px(px(metrics.card_pad()))
                                .py(px(10.))
                                .child(list)
                                .child(
                                    div()
                                        .flex_none()
                                        .bg(p.line_soft)
                                        .when(stacked, |this| this.h(px(1.)).w_full())
                                        .when(!stacked, |this| this.w(px(1.))),
                                )
                                .child(editor),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .px(px(metrics.card_pad()))
                                .py(px(10.))
                                .border_t_1()
                                .border_color(p.line_soft)
                                .child(kit::muted(t("structure.templates.persistHint"), p))
                                .child(div().flex_1())
                                .child(kit::secondary_button("structure-templates-done", None, t("common.close"), move |_, _, cx| {
                                    done_owner.update(cx, |pane, cx| {
                                        pane.close_template_manager();
                                        cx.notify();
                                    })
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    #[inline(never)]
    fn template_editor(&mut self, draft: &ColumnTemplate, manager: &TemplateManager, entity: &Entity<Self>, p: &Palette, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let locale = self.session.locale;
        let t = |key: &str| SharedString::from(i18n::text(locale, key));
        let engine = self.session.target.engine.clone();
        let name_hint = i18n::text(locale, "structure.templates.nameHint");
        let name = self.draft_input(8, &draft.name, &name_hint, window, cx);

        // 适用引擎：全部 / 逐个引擎（只列有结构编辑的引擎）。
        let mut engines = div().flex().flex_wrap().items_center().gap(px(4.));
        let all_owner = entity.clone();
        engines = engines.child(
            kit::tool_button("structure-template-engine-all", None, t("structure.templates.allEngines"), move |_, _, cx| {
                all_owner.update(cx, |pane, cx| {
                    pane.template_toggle_engine(None);
                    cx.notify();
                })
            })
            .selected(draft.engines.is_empty()),
        );
        for id in registry::ENGINES.into_iter().filter(|engine| crate::adapters::ddl::types_of(*engine).is_some()).map(|engine| crate::adapters::adapter(engine).meta().id()) {
            let owner = entity.clone();
            let on = draft.engines.iter().any(|item| item == id);
            let label = crate::adapters::label_named(id);
            engines = engines.child(
                kit::tool_button(SharedString::from(format!("structure-template-engine-{id}")), None, label, move |_, _, cx| {
                    owner.update(cx, |pane, cx| {
                        pane.template_toggle_engine(Some(id));
                        cx.notify();
                    })
                })
                .selected(on),
            );
        }

        // 字段表。
        let head = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .h(px(26.))
            .text_size(px(TypeScale::XS))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(p.text_faint)
            .child(div().w(px(130.)).flex_none().child(t("structure.field.column")))
            .child(div().w(px(130.)).flex_none().child(t("structure.field.type")))
            .child(div().w(px(44.)).flex_none().flex().justify_center().child(t("ui.structure.nullable")))
            .child(div().w(px(170.)).flex_none().child(t("ui.structure.default")))
            .child(div().flex_1().min_w(px(80.)).child(t("structure.field.comment")))
            .child(div().w(px(70.)).flex_none());
        let mut rows = div().flex().flex_col();
        let count = draft.columns.len();
        let expressions = types_named(&engine).expression_choices();
        for (index, column) in draft.columns.iter().enumerate() {
            let key = |group: u32| group * 10_000 + index as u32;
            let column_name = self.draft_input(key(5), &column.name, "name", window, cx);
            let kind_placeholder = default_column_type(&engine).to_owned();
            let kind = self.draft_input(key(6), &column.type_name, &kind_placeholder, window, cx);
            let comment = self.draft_input(key(8), &column.comment, "", window, cx);
            let type_owner = entity.clone();
            let current_type = column.type_name.clone();
            let menu_engine = engine.clone();
            let type_button = Button::new(SharedString::from(format!("structure-template-types-{index}")))
                .ghost()
                .xsmall()
                .icon(IconName::ChevronDown)
                .dropdown_menu(move |menu, _, _| {
                    let owner = type_owner.clone();
                    type_menu(menu, &menu_engine, &current_type, locale, move |choice, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.write_template_cell(6, index, choice);
                            cx.notify();
                        })
                    })
                });
            let default_owner = entity.clone();
            let current_default = column.default.clone();
            let nullable = column.nullable;
            let mode_button = Button::new(SharedString::from(format!("structure-template-default-{index}")))
                .ghost()
                .xsmall()
                .label(i18n::text(locale, mode_key(column.default.mode(), true)))
                .icon(IconName::ChevronDown)
                .dropdown_menu(move |menu, _, _| {
                    let owner = default_owner.clone();
                    default_menu(menu, &current_default, nullable, expressions, locale, move |value, cx| {
                        owner.update(cx, |pane, cx| {
                            pane.template_column_default(index, value);
                            cx.notify();
                        })
                    })
                });
            let default_value: AnyElement = match column.default.mode() {
                DefaultMode::Literal | DefaultMode::Expression => self.draft_input(key(7), column.default.text(), "", window, cx),
                DefaultMode::Null => div().px(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_muted).child("NULL").into_any_element(),
                DefaultMode::None => div().px(px(4.)).text_size(px(TypeScale::SM)).text_color(p.text_faint).child("—").into_any_element(),
            };
            let (null_owner, up_owner, down_owner, remove_owner) = (entity.clone(), entity.clone(), entity.clone(), entity.clone());
            rows = rows.child(
                div()
                    .id(SharedString::from(format!("structure-template-col-{index}")))
                    .test_support()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(8.))
                    .py(px(2.))
                    .border_b_1()
                    .border_color(p.line_soft)
                    .child(div().w(px(130.)).flex_none().child(column_name))
                    .child(div().w(px(130.)).flex_none().flex().items_center().child(div().flex_1().min_w_0().child(kind)).child(type_button))
                    .child(
                        div().w(px(44.)).flex_none().flex().justify_center().child(
                            check_box(SharedString::from(format!("structure-template-null-{index}")), column.nullable, true, t("ui.structure.nullable"), p).on_click(move |_, _, cx| {
                                null_owner.update(cx, |pane, cx| {
                                    pane.template_toggle_null(index);
                                    cx.notify();
                                })
                            }),
                        ),
                    )
                    .child(div().w(px(170.)).flex_none().flex().items_center().gap(px(2.)).child(mode_button).child(div().flex_1().min_w_0().child(default_value)))
                    .child(div().flex_1().min_w(px(80.)).child(comment))
                    .child(
                        div()
                            .w(px(70.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_end()
                            .child(
                                kit::icon_button(SharedString::from(format!("structure-template-col-up-{index}")), IconName::ChevronUp, t("structure.field.moveUp"), move |_, _, cx| {
                                    up_owner.update(cx, |pane, cx| {
                                        pane.template_move_column(index, -1);
                                        cx.notify();
                                    })
                                })
                                .xsmall()
                                .disabled(index == 0),
                            )
                            .child(
                                kit::icon_button(SharedString::from(format!("structure-template-col-down-{index}")), IconName::ChevronDown, t("structure.field.moveDown"), move |_, _, cx| {
                                    down_owner.update(cx, |pane, cx| {
                                        pane.template_move_column(index, 1);
                                        cx.notify();
                                    })
                                })
                                .xsmall()
                                .disabled(index + 1 >= count),
                            )
                            .child(
                                kit::icon_button(SharedString::from(format!("structure-template-col-remove-{index}")), IconName::X, t("structure.field.removeDraft"), move |_, _, cx| {
                                    remove_owner.update(cx, |pane, cx| {
                                        pane.template_remove_column(index);
                                        cx.notify();
                                    })
                                })
                                .xsmall(),
                            ),
                    ),
            );
        }
        let add_owner = entity.clone();
        let save_owner = entity.clone();
        let builtin = is_builtin_id(&draft.id);
        div()
            .id("structure-template-editor")
            .test_support()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .scroll_area_y()
            .child(div().max_w(px(360.)).child(kit::form_row(t("structure.templates.name"), name, p)))
            .child(kit::form_row(t("structure.templates.engines"), engines, p))
            .child(
                kit::card(p)
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("structure-template-columns-hscroll")
                            .test_support()
                            .w_full()
                            .scroll_area_x()
                            .child(div().min_w(px(670.)).w_full().flex().flex_col().child(head).child(rows)),
                    )
                    .child(
                        div().px(px(6.)).py(px(4.)).child(kit::tool_button("structure-template-add-column", Some(IconName::Plus), t("structure.field.addColumn"), move |_, _, cx| {
                            add_owner.update(cx, |pane, cx| {
                                pane.template_add_column();
                                cx.notify();
                            })
                        })),
                    ),
            )
            .when_some(manager.problem, |this, problem| this.child(issue_line("structure-template-problem", &i18n::text(locale, problem.key()), p)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(manager.saved, |this| this.child(kit::icon(IconName::Check, 13., p.ok)).child(div().text_size(px(TypeScale::SM)).text_color(p.ok).child(t("structure.templates.saved"))))
                    .child(div().flex_1())
                    .child(
                        kit::primary_button("structure-template-save", Some(IconName::Check), t("structure.templates.save"), move |_, _, cx| {
                            save_owner.update(cx, |pane, cx| {
                                let _ = pane.save_template(cx);
                                cx.notify();
                            })
                        })
                        .disabled(builtin),
                    ),
            )
            .into_any_element()
    }
}

/// 回车回调：交给新建表页的焦点流转。
fn on_draft_enter(this: &mut StructurePane, key: u32, _: &mut Context<StructurePane>) {
    this.draft_enter(key);
}

/// 就近的问题提示行。
pub(super) fn issue_line(id: impl Into<gpui_kit::ElementId>, text: &str, p: &Palette) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .py(px(3.))
        .text_size(px(TypeScale::XS))
        .text_color(p.danger)
        .child(kit::icon(IconName::TriangleAlert, 12., p.danger))
        .child(div().min_w_0().child(SharedString::from(text.to_owned())))
        .into_any_element()
}

/// 菜单里的模板名：内置模板按语言翻译，用户模板显示名称与字段数。
pub(super) fn template_label(template: &ColumnTemplate, locale: Locale) -> String {
    if template.is_builtin() {
        return i18n::text(locale, &template.name);
    }
    let names = template.columns.iter().map(|column| column.name.as_str()).collect::<Vec<_>>().join(" / ");
    format!("{} · {names}", template.name)
}

/// 模板适用引擎的说明。
fn engines_label(engines: &[String], locale: Locale) -> String {
    if engines.is_empty() {
        return i18n::text(locale, "structure.templates.allEngines");
    }
    engines.iter().map(|id| crate::adapters::label_named(id)).collect::<Vec<_>>().join(", ")
}

/// 表名框下方的问题（表名为空或无效）。
pub(super) fn table_issue(issues: &[DraftIssue]) -> Option<&DraftIssue> {
    issues.iter().find(|issue| issue.field == IssueField::TableName)
}
