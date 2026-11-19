use super::*;
use crate::catalog::StatRow;

fn object(name: &str, rows: Option<u64>, size: Option<u64>, comment: &str) -> StatRow {
    StatRow { rows, size, comment: comment.to_owned(), ..StatRow::named(name, Some(ObjectKind::Table)) }
}

fn mysql_target(database: &str) -> OverviewTarget {
    OverviewTarget::new("c1", "本地 MySQL", database, "", Engine::Mysql, "dev")
}

#[test]
fn target_spec_round_trips_and_keeps_the_work_item_layout() {
    let target = OverviewTarget::new("pg", "Atlas", "shop", "sales", Engine::Postgres, "prod");
    let spec = target.spec();
    assert_eq!(OverviewTarget::parse(&spec), Some(target.clone()));
    let parts: Vec<&str> = spec.split('|').collect();
    // 工作项按 0 连接、1 名称、2 库、5 引擎取目标；概览规格与表规格同一布局。
    assert_eq!((parts[0], parts[1], parts[2], parts[5]), ("pg", "Atlas", "shop", Engine::Postgres.as_str()));
    assert_eq!(target.title(), "sales");
    assert_eq!(mysql_target("").title(), "本地 MySQL");
    assert_eq!(mysql_target("").level(), crate::catalog::StatsLevel::Databases);
    assert_eq!(mysql_target("shop").level(), crate::catalog::StatsLevel::Objects);
    assert!(OverviewTarget::parse("|x||||mysql|").is_none(), "没有连接不是有效目标");
}

#[test]
fn child_rows_drill_down_one_level_and_objects_build_table_specs() {
    let connection = OverviewTarget::new("pg", "Atlas", "", "", Engine::Postgres, "");
    let database = connection.child(&StatRow::named("shop", None)).unwrap();
    assert_eq!((database.database.as_str(), database.schema.as_str()), ("shop", ""));
    assert_eq!(database.level(), crate::catalog::StatsLevel::Schemas);
    let schema = database.child(&StatRow::named("public", None)).unwrap();
    assert_eq!(schema.level(), crate::catalog::StatsLevel::Objects);
    assert!(schema.child(&object("orders", None, None, "")).is_none());
    let spec = schema.object_spec("orders");
    let target = crate::app::model::ObjectTarget::parse(&spec).unwrap();
    assert_eq!((target.database.as_str(), target.schema.as_str(), target.name.as_str()), ("shop", "public", "orders"));
}

#[test]
fn filter_matches_name_or_comment_and_sort_keeps_unknowns_last() {
    let mut state = OverviewState::new(mysql_target("shop"));
    state.rows = vec![
        object("t10", Some(5), Some(4096), ""),
        object("t2", Some(900), None, "订单明细"),
        object("Orders", None, Some(1_048_576), "订单"),
        object("audit", Some(40), Some(16_384), "日志"),
    ];
    let names = |state: &OverviewState| state.visible().iter().map(|row| row.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&state), ["audit", "Orders", "t2", "t10"], "名称自然排序，不分大小写");
    state.toggle_sort(OverviewColumn::Size);
    assert!(state.descending, "数字列首次点击从大到小");
    assert_eq!(names(&state), ["Orders", "audit", "t10", "t2"], "读不到大小的排最后");
    state.toggle_sort(OverviewColumn::Size);
    assert_eq!(names(&state), ["t10", "audit", "Orders", "t2"], "升序时未知值仍在最后");
    state.toggle_sort(OverviewColumn::Rows);
    assert_eq!(names(&state), ["t2", "audit", "t10", "Orders"]);
    state.filter = "订单".to_owned();
    assert_eq!(names(&state), ["t2", "Orders"], "关键字匹配注释");
    state.filter = "ORD".to_owned();
    assert_eq!(names(&state), ["Orders"], "关键字不分大小写");
}

#[test]
fn late_results_after_a_reload_are_dropped_and_selection_survives_when_still_present() {
    let mut state = OverviewState::new(mysql_target("shop"));
    let first = state.begin_load();
    state.reload();
    let body = |names: &[&str]| crate::catalog::StatsBody {
        level: crate::catalog::StatsLevel::Objects,
        rows: names.iter().map(|name| object(name, None, None, "")).collect(),
        missing: None,
    };
    assert!(!state.finish_load(first, Ok(body(&["old"]))), "刷新前的读取迟到后丢弃");
    assert_eq!(state.load, OverviewLoad::Pending);
    let second = state.begin_load();
    assert!(state.finish_load(second, Ok(body(&["orders", "users"]))));
    assert_eq!(state.load, OverviewLoad::Ready);
    state.step_selection(1);
    assert_eq!(state.selected.as_deref(), Some("orders"));
    state.step_selection(1);
    state.step_selection(1);
    assert_eq!(state.selected.as_deref(), Some("users"), "停在最后一行");
    state.reload();
    let third = state.begin_load();
    assert!(state.finish_load(third, Ok(body(&["orders"]))));
    assert_eq!(state.selected, None, "选中的行已不存在就清掉");
    state.reload();
    let fourth = state.begin_load();
    assert!(state.finish_load(fourth, Err("timeout".to_owned())));
    assert_eq!(state.load, OverviewLoad::Failed("timeout".to_owned()));
    assert!(state.rows.is_empty());
}

#[test]
fn sizes_and_counts_format_for_humans_and_unknowns_show_a_dash() {
    assert_eq!(format_size(None), MISSING);
    assert_eq!(format_size(Some(512)), "512 B");
    assert_eq!(format_size(Some(16_384)), "16.0 KB");
    assert_eq!(format_size(Some(1_572_864)), "1.5 MB");
    assert_eq!(format_count(Some(1_234_567)), "1,234,567");
    assert_eq!(format_count(Some(12)), "12");
    assert_eq!(format_count(None), MISSING);
    assert_eq!(columns(crate::catalog::StatsLevel::Databases), &[OverviewColumn::Name, OverviewColumn::Tables, OverviewColumn::Size]);
    assert!(columns(crate::catalog::StatsLevel::Objects).contains(&OverviewColumn::Updated));
}
