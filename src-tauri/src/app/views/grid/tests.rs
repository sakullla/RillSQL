//! 第三轮表格编辑回归（R1、R2）：编辑器内按键、复制不带表头、整行/越界粘贴成新增行、
//! 新增行的插入位置、焦点、横向跟随、整行标识与撤销、双击进编辑、放大编辑器。
//! 夹具只用通用名称（people / wide_orders）。

use super::session::{self, GridTarget};
use super::*;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, Bounds, Point, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, size};

fn target(table: &str) -> GridTarget {
    GridTarget {
        connection_id: "demo".to_owned(),
        connection_name: "demo-uat".to_owned(),
        database: "shop".to_owned(),
        schema: String::new(),
        table: table.to_owned(),
        engine: "sqlite".to_owned(),
        environment: "dev".to_owned(),
    }
}

fn open(cx: &mut TestAppContext, width: f32, height: f32) -> (AnyWindowHandle, Entity<GridPane>) {
    let dir = std::env::temp_dir().join(format!("rillsql-grid-round3-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    cx.update(gpui_kit::init);
    cx.update(|cx| {
        let bounds = Bounds { origin: Point::default(), size: size(px(width), px(height)) };
        gpui_kit::open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |window, cx| cx.new(|cx| GridPane::new(services, window, cx)),
        )
        .expect("grid pane")
    })
}

/// people(id 自增主键, name, note)，三行。
fn plant_people(pane: &Entity<GridPane>, cx: &mut App) {
    pane.update(cx, |pane, cx| {
        pane.plant_writable(
            target("people"),
            vec![("id", "integer"), ("name", "text"), ("note", "text")],
            vec![vec![Some("1"), Some("甲"), Some("一")], vec![Some("2"), Some("乙"), Some("二")], vec![Some("3"), Some("丙"), Some("三")]],
            "id",
            cx,
        );
    });
}

/// people(id 自增主键, code 非空, note 可空)：新增行里 code 必填。
fn plant_required(pane: &Entity<GridPane>, cx: &mut App) {
    pane.update(cx, |pane, cx| {
        let columns = vec![
            session::column_of("id", "integer", true),
            crate::grid::Column { name: "code".into(), type_name: "varchar(8)".into(), nullable: false, ..crate::grid::Column::default() },
            crate::grid::Column { name: "note".into(), type_name: "text".into(), nullable: true, ..crate::grid::Column::default() },
        ];
        let schema = session::schema_from_parts("sqlite", "", "people", "table", columns, vec!["id".to_owned()]).unwrap();
        let rows = vec![session::GridRow {
            id: "1".into(),
            values: vec![Some("1".into()), Some("A1".into()), None],
            located: true,
            locator: vec![("id".into(), Some("1".into()))],
            inserted: false,
        }];
        pane.session.mode = GridMode::Table;
        pane.session.switch_target(target("people"));
        pane.session.load_local(columns_from_schema(&schema, true), rows, Some(1), Some(schema));
        pane.sync_table(cx);
    });
}

/// wide_orders：id 自增主键加 24 个文本列，一共 25 列。
fn plant_wide(pane: &Entity<GridPane>, cx: &mut App) {
    let names: Vec<String> = (1..=24).map(|index| format!("field_{index:02}")).collect();
    pane.update(cx, |pane, cx| {
        let mut columns = vec![("id", "integer")];
        columns.extend(names.iter().map(|name| (name.as_str(), "text")));
        let row: Vec<Option<&str>> = std::iter::once(Some("1")).chain(names.iter().map(|_| Some("value"))).collect();
        pane.plant_writable(target("wide_orders"), columns, vec![row], "id", cx);
    });
}

/// 选中一格进入编辑，并把输入框里的文字换成 `text`（光标在末尾）。
fn start_edit(window: AnyWindowHandle, pane: &Entity<GridPane>, row: usize, column: usize, text: &str, cx: &mut TestAppContext) {
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.session.select(row, column, false);
            let _ = pane.command("edit-cell", cx);
        });
        window.render_frame(cx);
        let input = pane.read(cx).edit_input.clone();
        input.update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

/// 新增行里用户显式设为 NULL 的格子：再进编辑、什么都不输就离开，NULL 暂存保留，不被当成“先不填”撤掉。
#[gpui_kit::test]
fn leaving_an_explicit_null_on_a_new_row_keeps_it(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_required(&pane, cx);
        pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-insert", cx);
            pane.cancel_edit();
            pane.session.select(0, 2, false);
            let _ = pane.command("grid-set-null", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    let row_id = cx.update(|cx| pane.read(cx).session.row_id(0).map(str::to_owned).expect("新增行"));
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert!(pane.session.can_revert_cell(&row_id, 2), "note 显式暂存为 NULL");
        assert_eq!(pane.session.raw_value_of(&row_id, 2), Some(None));
    });
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::DoubleClickedCell(0, 3)));
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| assert_eq!(pane.read(cx).edit_target.as_ref().map(|target| target.column), Some(2)));
    cx.update_window(window, |_, window, cx| window.press("enter", cx)).unwrap();
    settle(cx);
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert!(!pane.editing(), "离开后编辑结束：{}", pane.notice);
        assert!(pane.session.can_revert_cell(&row_id, 2), "显式 NULL 的暂存仍在");
        assert_eq!(pane.session.raw_value_of(&row_id, 2), Some(None));
    });
}

fn settle(cx: &mut TestAppContext) {
    cx.run_until_parked();
}

fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

// ---------------------------------------------------------------- R1 编辑器内按键

/// 编辑中 ⌘C 只复制输入框里选中的文字；←/→ 在文字两端也不会跳到别的单元格。
#[gpui_kit::test]
fn editing_copy_takes_only_the_selected_text_and_arrows_stay_in_the_cell(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update(|cx| plant_people(&pane, cx));
    start_edit(window, &pane, 0, 2, "长文本内容", cx);
    cx.update(|cx| assert!(pane.read(cx).editing(), "进入编辑"));
    // 光标在末尾时按 →：输入框交还按键，但编辑格接住，不会移到下一格。
    cx.update_window(window, |_, window, cx| window.press("right", cx)).unwrap();
    settle(cx);
    cx.update(|cx| {
        assert!(pane.read(cx).editing(), "→ 在文字末尾不结束编辑");
        assert_eq!(pane.read(cx).session.focus_cell(), Some((0, 2)), "→ 不切换单元格");
    });
    // 选中最后两个字（Shift+←×2）后 ⌘C：剪贴板只有这两个字。
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("旧内容".to_owned())));
    cx.update_window(window, |_, window, cx| {
        window.press("shift-left", cx);
        window.press("shift-left", cx);
        window.press("secondary-c", cx);
    })
    .unwrap();
    settle(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("内容"), "编辑中 ⌘C 只复制选中的文字");
    cx.update(|cx| assert!(pane.read(cx).editing(), "复制不结束编辑"));
    // 回到开头再按 ←：同样停在原格。Home/End 由输入框处理。
    cx.update_window(window, |_, window, cx| {
        window.press("home", cx);
        window.press("left", cx);
        window.press("left", cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        assert!(pane.read(cx).editing(), "← 在文字开头不结束编辑");
        assert_eq!(pane.read(cx).session.focus_cell(), Some((0, 2)), "← 不切换单元格");
        assert_eq!(pane.read(cx).edit_value(cx), "长文本内容", "方向键没有改动文字");
    });
    // ⌘A 全选、⌘X 剪切归输入框。
    cx.update_window(window, |_, window, cx| {
        window.press("secondary-a", cx);
        window.press("secondary-x", cx);
    })
    .unwrap();
    settle(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("长文本内容"));
    cx.update(|cx| {
        assert_eq!(pane.read(cx).edit_value(cx), "", "⌘X 剪走输入框里的文字");
        assert_eq!(pane.read(cx).session.selection_count(), 1, "⌘A 没有选中整张表");
    });
    // Tab 仍是“保存并到下一格”。
    cx.update_window(window, |_, window, cx| {
        window.input("新备注", cx);
        window.press("tab", cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        assert_eq!(pane.read(cx).cell(0, 2), "新备注");
        assert_eq!(pane.read(cx).session.focus_cell(), Some((1, 0)), "末列 Tab 换到下一行首列");
    });
}

/// 编辑时不浮出快捷键提示面板，提示在状态区；NULL / 放大收在格子里的图标组。
#[gpui_kit::test]
fn editing_a_plain_cell_keeps_the_rows_below_uncovered(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        pane.update(cx, |pane, cx| {
            pane.session.select(0, 1, false);
            let _ = pane.command("edit-cell", cx);
        });
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find("grid-edit-panel").is_none(), "普通文本列不浮出面板");
        assert!(window.try_find("grid-edit-hint").is_some(), "快捷键提示在状态区");
        assert!(window.try_find("grid-edit-null").is_some() && window.try_find("grid-edit-expand").is_some());
    })
    .unwrap();
    let mut visual = VisualTestContext::from_window(window, cx);
    let editing = visual.debug_bounds("grid-cell-0-2").expect("编辑中的格子");
    let below = visual.debug_bounds("grid-cell-1-2").expect("下一行同列");
    let actions = visual.debug_bounds("grid-edit-actions").expect("格内图标组");
    assert!(actions.bottom() <= editing.bottom() + px(0.5) && actions.top() >= editing.top() - px(0.5), "图标组在格子里面：{actions:?} {editing:?}");
    assert!(actions.bottom() <= below.top() + px(0.5), "下一行没有被遮挡");
    // 结束编辑后提示消失。
    visual.update(|window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("grid-edit-hint").is_none(), "编辑结束提示消失");
    });
}

/// 放大编辑器：可以换行、⌘F 打开编辑器自己的查找（不是网格的查找栏）、⌘A ⌘C 复制全部文字。
#[gpui_kit::test]
fn expanded_editor_supports_newlines_search_and_select_all(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update(|cx| plant_people(&pane, cx));
    start_edit(window, &pane, 0, 2, "第一行", cx);
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, cx| pane.expand_edit_to_viewer(window, cx));
        window.render_frame(cx);
        let editor = pane.read(cx).expand.as_ref().map(|expand| expand.viewer.read(cx).editor().clone()).expect("放大编辑器");
        editor.update(cx, |state, cx| state.focus(window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update_window(window, |_, window, cx| {
        window.press("end", cx);
        window.press("enter", cx);
        window.input("第二行", cx);
    })
    .unwrap();
    settle(cx);
    let editor = cx.update(|cx| pane.read(cx).expand.as_ref().map(|expand| expand.viewer.read(cx).editor().clone()).expect("放大编辑器"));
    cx.update(|cx| assert_eq!(editor.read(cx).value().to_string(), "第一行\n第二行", "Enter 在放大编辑器里换行"));
    cx.update_window(window, |_, window, cx| {
        window.press("secondary-a", cx);
        window.press("secondary-c", cx);
    })
    .unwrap();
    settle(cx);
    assert_eq!(clipboard(cx).as_deref(), Some("第一行\n第二行"), "全选复制得到全部文字");
    cx.update_window(window, |_, window, cx| window.press("secondary-f", cx)).unwrap();
    settle(cx);
    cx.update(|cx| {
        assert!(editor.read(cx).search_session().open, "⌘F 打开放大编辑器里的查找");
        assert!(!pane.read(cx).find_open, "网格的查找栏不抢 ⌘F");
        assert!(pane.read(cx).expand.is_some(), "放大编辑器仍开着");
    });
}

// ---------------------------------------------------------------- R1 复制与粘贴

/// ⌘C 多格不带表头，⇧⌘C 带表头；单格都只复制值。
#[gpui_kit::test]
fn copy_has_no_header_by_default_and_shift_adds_it(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        window.render_frame(cx);
    })
    .unwrap();
    assert_eq!(grid_shortcut("c", true, false), Some("copy-region"));
    assert_eq!(grid_shortcut("c", true, true), Some("grid-copy-with-header"));
    assert_eq!(grid_shortcut_with("c", true, true, true), Some("copy-sql-value"));
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.session.select(0, 1, false);
            pane.session.select(1, 2, true);
            let _ = pane.command("copy-region", cx);
        })
    });
    assert_eq!(clipboard(cx).as_deref(), Some("甲\t一\n乙\t二"));
    cx.update(|cx| pane.update(cx, |pane, cx| pane.command("grid-copy-with-header", cx)));
    assert_eq!(clipboard(cx).as_deref(), Some("name\tnote\n甲\t一\n乙\t二"));
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.session.select(2, 1, false);
            let _ = pane.command("grid-copy-with-header", cx);
        })
    });
    assert_eq!(clipboard(cx).as_deref(), Some("丙"), "单格只复制值本身");
}

/// 点行号选中两行 ⌘C 得到两行 TSV；⌘V 粘贴后出现两条新增行（插在选中行下方），一次撤销全部回退。
#[gpui_kit::test]
fn pasting_whole_rows_stages_new_rows_below_the_selection(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        window.render_frame(cx);
    })
    .unwrap();
    // 行头选中第 1、2 行。
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.apply_table_event(&TableEvent::SelectRow(0));
            pane.session.select(1, 2, true);
            let _ = pane.command("copy-region", cx);
        })
    });
    assert_eq!(clipboard(cx).as_deref(), Some("1\t甲\t一\n2\t乙\t二"), "两行 TSV，不带表头");
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            assert_eq!(pane.session.whole_rows_selected(), Some((0, 1)));
            let _ = pane.command("grid-paste", cx);
        })
    });
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.loaded_len(), 5, "{}", pane.notice);
        assert!(pane.session.row_inserted(2) && pane.session.row_inserted(3), "新行紧跟在选中的第 2 行下方");
        assert_eq!(pane.session.staged_summary().inserts, 2, "两条新增行，标记为待提交");
        assert_eq!(pane.cell(2, 1), "甲");
        assert_eq!(pane.cell(3, 2), "二");
        assert_eq!(pane.cell(2, 0), i18n::text(Locale::ZhCn, "grid.value.autoLabel"), "自增主键不跟着复制过来，交给数据库");
        assert_eq!(pane.session.row_number(2), i18n::text(Locale::ZhCn, "grid.row.new"), "行号区显示“新”");
        assert_eq!(pane.notice, i18n::fill(&i18n::text(Locale::ZhCn, "grid.write.pastedRows"), &[("count", "2")]));
        assert_eq!(pane.session.whole_rows_selected(), Some((2, 3)), "选中刚粘贴的新行");
    });
    // 一次撤销把两条新行和它们的值一起去掉。
    cx.update(|cx| pane.update(cx, |pane, cx| pane.command("grid-undo", cx)));
    cx.update(|cx| {
        assert_eq!(pane.read(cx).loaded_len(), 3);
        assert!(!pane.read(cx).session.uncommitted());
    });
}

/// 整行粘贴：第一行和列名完全一致时当作表头跳过；列数不符时说出具体是哪一列；只读结果说明原因。
#[gpui_kit::test]
fn whole_row_paste_skips_the_header_and_names_the_mismatched_column(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        window.render_frame(cx);
    })
    .unwrap();
    let paste = |cx: &mut TestAppContext, text: &str| {
        cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned())));
        cx.update(|cx| {
            pane.update(cx, |pane, cx| {
                pane.apply_table_event(&TableEvent::SelectRow(2));
                let _ = pane.command("grid-paste", cx);
            })
        });
    };
    paste(cx, "id\tname\tnote\n7\t丁\t四");
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.loaded_len(), 4, "表头行被跳过，只多一行：{}", pane.notice);
        assert_eq!(pane.cell(3, 1), "丁");
    });
    cx.update(|cx| pane.update(cx, |pane, cx| pane.command("grid-undo", cx)));
    // 只有两列：缺少第 3 列 note。
    paste(cx, "8\t戊");
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.loaded_len(), 3, "列数不符整次拒绝");
        let expected = BatchReject::MissingColumn { position: 3, column: "note".into(), pasted: 2 }.message(Locale::ZhCn);
        assert_eq!(pane.notice, expected);
        assert!(pane.notice.contains("note") && pane.notice.contains('3'));
    });
    // 四列：第 4 列没有对应的列。
    paste(cx, "8\t戊\t五\t多余");
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.loaded_len(), 3);
        assert_eq!(pane.notice, BatchReject::ExtraColumn { position: 4, last: "note".into() }.message(Locale::ZhCn));
    });
    // 只读查询结果：可以复制，粘贴被拒并给出原因。
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            let sheet = QuerySheet { sql: "select count(*) from people".into(), columns: vec!["n".into()], rows: vec![vec![Some("3".into())]], truncated: false, full_count: Some(1) };
            pane.show_query(target("people"), sheet, cx);
            pane.session.select(0, 0, false);
            let _ = pane.command("copy-region", cx);
        })
    });
    assert_eq!(clipboard(cx).as_deref(), Some("3"), "只读结果可以复制");
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-paste", cx);
            let reason = pane.session.gate_text(Gate::Readonly);
            assert!(!reason.is_empty());
            assert_eq!(pane.notice, BatchReject::Readonly(reason).message(Locale::ZhCn), "粘贴被拒并说明只读原因");
            assert!(!pane.session.uncommitted());
        })
    });
}

/// 从焦点格粘贴的行超出已加载行：多出来的行接在末尾成为新增行；列越界说出是第几列。
#[gpui_kit::test]
fn pasting_past_the_last_row_adds_new_rows(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("改\t改一\n新甲\t新一\n新乙\t新二".to_owned())));
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.session.select(2, 1, false);
            let _ = pane.command("grid-paste", cx);
        })
    });
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.loaded_len(), 5, "{}", pane.notice);
        assert_eq!(pane.cell(2, 1), "改", "已有行按单元格覆盖");
        assert!(pane.session.row_inserted(3) && pane.session.row_inserted(4));
        assert_eq!((pane.cell(3, 1), pane.cell(4, 2)), ("新甲".to_owned(), "新二".to_owned()));
        let summary = pane.session.staged_summary();
        assert_eq!((summary.inserts, summary.updates), (2, 2));
    });
}

// ---------------------------------------------------------------- R2 新增行

/// 新增行插在选中行下方（没有选中时在顶部），焦点落在第一个可编辑列并直接进入编辑。
#[gpui_kit::test]
fn insert_goes_below_the_selected_row_and_starts_editing(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        pane.update(cx, |pane, cx| {
            pane.session.select(1, 2, false);
            let _ = pane.command("grid-insert", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert!(pane.session.row_inserted(2), "插在选中的第 2 行下方");
        assert_eq!(pane.session.focus_cell(), Some((2, 1)), "自增 id 跳过，焦点在第一个可编辑列 name");
        assert!(pane.editing(), "直接进入编辑");
        assert_eq!(pane.edit_target.as_ref().map(|target| target.column), Some(1));
    });
    cx.update_window(window, |_, window, cx| {
        window.input("新人", cx);
        window.press("enter", cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| assert_eq!(pane.read(cx).cell(2, 1), "新人"));
    // 没有选择时插在顶部。
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.session.clear_selection();
            let _ = pane.command("grid-insert", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert!(pane.session.row_inserted(0), "没有选中行时插在顶部");
        assert_eq!(pane.session.focus_cell(), Some((0, 1)));
        assert!(pane.editing());
    });
}

/// 新增行整行标识：行号区写“新”、整行浅色底、左侧色条只在行号格；一键撤销这一行。
#[gpui_kit::test]
fn new_rows_are_marked_and_can_be_discarded_one_by_one(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_people(&pane, cx);
        pane.update(cx, |pane, cx| {
            pane.session.select(0, 1, false);
            let _ = pane.command("grid-insert", cx);
            pane.cancel_edit();
            let _ = pane.command("grid-insert", cx);
            pane.cancel_edit();
        });
        window.render_frame(cx);
    })
    .unwrap();
    {
        let mut visual = VisualTestContext::from_window(window, cx);
        let mark = visual.debug_bounds("grid-row-new-1").expect("行号区显示“新”");
        let number = visual.debug_bounds("grid-cell-1-0").expect("行号格");
        assert!(number.contains(&mark.center()), "“新”在行号格里");
        assert!(visual.debug_bounds("grid-row-new-2").is_some());
        assert!(visual.debug_bounds("grid-row-new-0").is_none(), "已有行不显示“新”");
    }
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.session.row_number(1), "新");
        assert!(pane.session.row_inserted(1) && pane.session.row_inserted(2));
    });
    // 只撤销其中一行，另一行留着。
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, cx| pane.discard_inserted_row(1, cx));
        window.render_frame(cx);
    })
    .unwrap();
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.loaded_len(), 4);
        assert_eq!(pane.session.staged_summary().inserts, 1, "只撤销了一条新增行");
    });
}

/// 新增行里双击任一格都能编辑：哪怕上一格是没填的必填列（直接离开不算输入 NULL，不会卡住编辑框）。
#[gpui_kit::test]
fn double_click_edits_any_cell_of_a_new_row(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 1100., 720.);
    cx.update_window(window, |_, window, cx| {
        plant_required(&pane, cx);
        pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-insert", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        assert!(pane.read(cx).editing(), "新增后直接编辑 code");
        assert_eq!(pane.read(cx).session.focus_cell(), Some((0, 1)));
    });
    // 什么都没填就双击同一行的 note：编辑移过去，不因 code 必填卡在原格。
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::DoubleClickedCell(0, 3)));
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        let pane = pane.read(cx);
        assert!(pane.editing(), "双击新增行的另一格进入编辑：{}", pane.notice);
        assert_eq!(pane.edit_target.as_ref().map(|target| target.column), Some(2));
        assert!(pane.session.staged_summary().updates == 0 && pane.session.row_inserted(0));
    });
    cx.update_window(window, |_, window, cx| {
        window.input("备注", cx);
        window.press("enter", cx);
    })
    .unwrap();
    settle(cx);
    // 再双击已填的格子：照样进入编辑，原值在输入框里。
    cx.update_window(window, |_, window, cx| {
        pane.update(cx, |pane, _| pane.apply_table_event(&TableEvent::DoubleClickedCell(0, 3)));
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    cx.update(|cx| {
        assert!(pane.read(cx).editing());
        assert_eq!(pane.read(cx).edit_value(cx), "备注");
    });
    // 真实鼠标双击另一格（必填 code）。
    let mut visual = VisualTestContext::from_window(window, cx);
    let code = visual.debug_bounds("grid-cell-0-2").expect("code 格");
    for click_count in [1, 2] {
        visual.simulate_event(MouseDownEvent { position: code.center(), button: MouseButton::Left, click_count, ..Default::default() });
        visual.simulate_event(gpui_kit::MouseUpEvent { position: code.center(), button: MouseButton::Left, click_count, ..Default::default() });
    }
    visual.run_until_parked();
    visual.update(|window, cx| window.render_frame(cx));
    visual.update(|_, cx| {
        let pane = pane.read(cx);
        assert!(pane.editing(), "真实双击新增行进入编辑：{}", pane.notice);
        assert_eq!(pane.edit_target.as_ref().map(|target| target.column), Some(1));
    });
}

/// 25 列宽表：新增行里连续 Tab 填到最后一列，每一步当前单元格都完整可见，行号列固定在左侧。
#[gpui_kit::test]
fn tab_through_a_wide_new_row_keeps_the_cell_fully_visible(cx: &mut TestAppContext) {
    let (window, pane) = open(cx, 900., 600.);
    cx.update_window(window, |_, window, cx| {
        plant_wide(&pane, cx);
        pane.update(cx, |pane, cx| {
            let _ = pane.command("grid-insert", cx);
        });
        window.render_frame(cx);
    })
    .unwrap();
    settle(cx);
    let row = cx.update(|cx| pane.read(cx).session.focus_cell().map(|(row, _)| row)).expect("新增行");
    let mut visual = VisualTestContext::from_window(window, cx);
    visual.update(|window, cx| window.render_frame(cx));
    for step in 1..=24usize {
        let (focus_row, column) = visual.update(|_, cx| pane.read(cx).session.focus_cell()).expect("焦点");
        assert_eq!((focus_row, column), (row, step), "第 {step} 步");
        visual.update(|_, cx| assert!(pane.read(cx).editing(), "第 {step} 列在编辑"));
        visual.update(|window, cx| window.render_frame(cx));
        let body = visual.debug_bounds("grid-body").expect("网格主体");
        let number_selector: &'static str = format!("grid-cell-{row}-0").leak();
        let number = visual.debug_bounds(number_selector).expect("行号格");
        let selector: &'static str = format!("grid-cell-{row}-{}", step + 1).leak();
        let cell = visual.debug_bounds(selector).unwrap_or_else(|| panic!("第 {step} 列的格子画出来了"));
        let bar = gpui_kit::component::scroll::Scrollbar::width();
        assert!(cell.left() >= number.right() - px(0.5), "第 {step} 列没有钻到固定的行号列下面：{cell:?} {number:?}");
        assert!(cell.right() <= body.right() - bar + px(0.5), "第 {step} 列右缘完整可见：{cell:?} {body:?}");
        assert!(number.right() <= body.left() + px(72.), "行号列固定在左侧：{number:?} {body:?} {cell:?}");
        visual.update(|window, cx| {
            window.input(&format!("v{step}"), cx);
            window.press("tab", cx);
        });
        visual.run_until_parked();
    }
    visual.update(|_, cx| {
        let pane = pane.read(cx);
        assert_eq!(pane.cell(row, 24), "v24", "最后一列也填上了");
        assert_eq!(pane.cell(row, 1), "v1");
    });
}
