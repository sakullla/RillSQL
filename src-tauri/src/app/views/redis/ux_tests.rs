//! 2026-11-20 体验问题（R10–R14、R18、R19）：类型筛选立即生效、选键不闪、左表右值、诊断表、双击与高亮。
//! 只注入模型状态，不连真实 Redis；渲染走 GPUI 无头窗口。

use super::tables::ENTRY_TABLE;
use super::*;
use crate::adapters::redis::keys::{KeyValuePair, ValueProbe};
use crate::ui::read_table::ReadTableEvent;
use gpui_kit::test::TestWindowExt;

fn services(name: &str) -> (Arc<AppServices>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-ux-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = Arc::new(AppServices::new(Arc::new(crate::connections::Service::new_for_test(dir.join("connections.sqlite")))));
    (services, dir)
}

fn key(name: &str, key_type: &str) -> RedisKeyInfo {
    RedisKeyInfo { name: name.into(), key_type: key_type.into(), ttl_ms: Some(-1) }
}

fn string_detail(text: &str) -> RedisKeyDetail {
    RedisKeyDetail { key_type: "string".into(), ttl_ms: Some(-1), value: RedisValue::String { text: text.into() }, total: Some(text.len() as u64), next: None }
}

fn hash_detail(pairs: &[(&str, &str)]) -> RedisKeyDetail {
    RedisKeyDetail {
        key_type: "hash".into(),
        ttl_ms: Some(-1),
        value: RedisValue::Hash { entries: pairs.iter().map(|(f, v)| KeyValuePair { field: (*f).into(), value: (*v).into() }).collect() },
        total: Some(pairs.len() as u64 + 300),
        next: Some("7".into()),
    }
}

fn open(cx: &mut gpui_kit::TestAppContext, width: f32, setup: impl FnOnce(&mut RedisPane) + 'static) -> (gpui_kit::AnyWindowHandle, Entity<RedisPane>, std::path::PathBuf) {
    let (services, dir) = services("render");
    cx.update(gpui_kit::init);
    let (window, view) = cx.update(|cx| {
        let bounds = gpui_kit::Bounds { origin: gpui_kit::Point::default(), size: gpui_kit::size(px(width), px(900.)) };
        gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = RedisPane::new(services);
                    pane.connection_id = "cache".into();
                    pane.connection_name = "订单缓存".into();
                    setup(&mut pane);
                    pane
                })
            },
        )
        .expect("window")
    });
    (window, view, dir)
}

fn mixed_keys(pane: &mut RedisPane) {
    pane.set_keys(vec![key("s:1", "string"), key("h:1", "hash"), key("s:2", "string"), key("l:1", "list"), key("h:2", "hash")]);
    pane.done = true;
}

fn listed(pane: &mut RedisPane) -> Vec<String> {
    pane.key_rows()
        .iter()
        .filter_map(|row| match row {
            KeyRow::Key { index, .. } => pane.keys.get(*index).map(|key| key.name.clone()),
            _ => None,
        })
        .collect()
}

/// R11：先完成首次渲染（缓存已建立），再切到 String、Hash、全部：列表每次立即对应筛选，与计数一致。
#[gpui_kit::test]
fn type_filter_applies_immediately_after_the_first_render(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., mixed_keys);
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    assert_eq!(cx.update(|cx| view.update(cx, |pane, _| listed(pane).len())), 5, "首次渲染后缓存里是全部键");
    for (chip, expected) in [("redis-type-string", vec!["s:1", "s:2"]), ("redis-type-hash", vec!["h:1", "h:2"]), ("redis-type-all", vec!["s:1", "h:1", "s:2", "l:1", "h:2"])] {
        cx.update_window(window, |_, window, cx| {
            window.click(chip, cx);
            window.render_frame(cx);
        })
        .unwrap();
        let (names, footer) = cx
            .update_window(window, |_, window, cx| (view.update(cx, |pane, _| listed(pane)), window.find("redis-scan-progress").label().map(str::to_owned)))
            .unwrap();
        assert_eq!(names, expected, "{chip} 点下去列表立刻变化，不等下一次选键");
        let footer = footer.unwrap_or_default();
        if chip != "redis-type-all" {
            assert!(footer.contains(&format!("{} / 5", expected.len())) || footer.contains(&format!("{} ", expected.len())), "页脚计数与列表一致：{footer}");
        }
    }
    // 文件夹展开 / 折叠同样立即生效。
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.set_keys(vec![key("app:a", "string"), key("app:b", "string"), key("solo", "string")]);
            pane.command("redis-view-tree", cx);
            let before = pane.key_rows().len();
            pane.command("redis-folder:app", cx);
            let after = pane.key_rows().len();
            assert_ne!(before, after, "折叠文件夹后行数立即变化");
        })
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// R11：选键不清空详情；读取中可以再选别的键（写入 busy 也拦不住选键）；只有最后一次选键的结果落地。
#[gpui_kit::test]
fn switching_keys_keeps_old_content_and_only_the_last_read_lands(cx: &mut gpui_kit::TestAppContext) {
    // 选键会在后台运行时上发起读取（这里没有真实连接，结果是报错并被代数过滤）。
    cx.executor().allow_parking();
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.set_keys(vec![key("a", "string"), key("b", "string"), key("c", "string")]);
        pane.done = true;
        pane.selected = "a".into();
        pane.detail_key = "a".into();
        pane.detail = Some(Arc::new(string_detail("alpha")));
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    let first_viewer = cx.update(|cx| view.read(cx).string_viewer.as_ref().map(|viewer| viewer.entity_id())).expect("字符串查看器");

    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            // 有写入在跑（busy），也照样可以选键。
            pane.busy = true;
            pane.command("redis-select:1", cx);
            assert_eq!(pane.selected(), "b");
            assert!(pane.error().is_empty(), "选键不被 busy 拒绝：{}", pane.error());
            assert!(pane.reading(), "正在读取 b");
            assert!(pane.detail.is_some(), "旧内容还在，不清空");
            assert_eq!(pane.detail_key, "a");
            pane.busy = false;
        })
    });
    let (skeleton, inline, progress) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("skeleton-state").is_some(), window.try_find("inline-loading").is_some(), window.try_find("redis-detail-loading").is_some())
        })
        .unwrap();
    assert!(!skeleton && !inline, "读取中不出骨架、不往列表插加载行");
    assert!(progress, "只在详情头部画一条细进度线");

    // 连续切换：b、c 两次读取，b 的结果晚到也不覆盖 c。
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.selected = "b".into();
            let b = pane.begin_read();
            pane.selected = "c".into();
            let c = pane.begin_read();
            assert!(pane.deliver_read(c, "c".into(), Ok(string_detail("gamma")), cx));
            assert!(!pane.deliver_read(b, "b".into(), Ok(string_detail("beta")), cx), "旧响应被丢弃");
            assert_eq!(pane.detail_key, "c");
            assert!(pane.detail_text().contains("gamma"), "{}", pane.detail_text());
            assert!(!pane.reading());
        })
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    let (viewer, text) = cx.update(|cx| {
        let pane = view.read(cx);
        let viewer = pane.string_viewer.clone().expect("查看器");
        (viewer.entity_id(), viewer.read(cx).source().to_owned())
    });
    assert_eq!(viewer, first_viewer, "换键复用同一个 FormatViewer 实体");
    assert_eq!(text, "gamma");
    let _ = std::fs::remove_dir_all(dir);
}

/// R11：读取时发现键已不存在：详情说明它不存在，列表里保留这一行并画删除线。
#[gpui_kit::test]
fn a_key_found_missing_is_explained_and_struck_through(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.set_keys(vec![key("gone:1", "string"), key("kept", "string")]);
        pane.done = true;
    });
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.selected = "gone:1".into();
            let generation = pane.begin_read();
            let missing = RedisKeyDetail { key_type: "none".into(), ttl_ms: Some(-2), value: RedisValue::None {}, total: None, next: None };
            pane.deliver_read(generation, "gone:1".into(), Ok(missing), cx);
            assert!(pane.key_gone("gone:1"));
            assert_eq!(listed(pane), vec!["gone:1", "kept"], "键留在列表里，只是标记为已不存在");
        })
    });
    let gone = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("redis-key-gone").is_some()
        })
        .unwrap();
    assert!(gone, "详情区说明键已不存在");
    // 重新扫描后标记清掉。
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.set_keys(vec![key("kept", "string")]);
            assert!(!pane.key_gone("gone:1"));
        })
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// R10/R18：键行是固定宽度的类型 chip + 界面字体键名（实测截断、保留尾部、悬停全名）；工具区两行不折行。
#[gpui_kit::test]
fn key_rows_use_kit_chips_and_tail_truncation(cx: &mut gpui_kit::TestAppContext) {
    let long = "session:tenant-alpha:region-east:user:0000000000000000000000000000000123456789";
    let (window, view, dir) = open(cx, 1280., move |pane| {
        pane.set_keys(vec![key(long, "hash"), key("short", "zset")]);
        pane.done = true;
    });
    let (search, types, row_label) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.find("redis-tools-search").bounds(), window.find("redis-tools-types").bounds(), window.find("redis-select:0").label().map(str::to_owned))
        })
        .unwrap();
    assert!(search.origin.y < types.origin.y, "搜索与图标按钮一行，类型分段与视图切换一行");
    assert!(f32::from(types.size.height) < 40., "类型分段不折行：{:?}", types.size);
    assert!(row_label.unwrap_or_default().contains(long), "无障碍标签与悬停给完整键名");
    let fitted: Vec<String> = cx.update(|cx| view.read(cx).fit_cache.values().cloned().collect());
    assert!(fitted.iter().any(|text| text.starts_with('…') && long.ends_with(&text['…'.len_utf8()..])), "长键名截断时保留尾部：{fitted:?}");
    assert_eq!(super::view::type_code("hash"), "HSH");
    assert_eq!(super::view::type_code("stream"), "STM");
    let _ = std::fs::remove_dir_all(dir);
}

/// R19：Hash 是左表右值的分栏；表格显示「已加载 / 总数」、默认选中第一行；没有「结构化值查看器」标题；
/// 右栏标题就是字段名；双击值进入编辑；⌘C 复制选区；可以搜索、排序。
#[gpui_kit::test]
fn collections_split_into_a_read_table_and_a_titled_viewer(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1360., |pane| {
        pane.set_keys(vec![key("h", "hash")]);
        pane.done = true;
        pane.selected = "h".into();
        pane.detail_key = "h".into();
        pane.entry_focus = Some(0);
        pane.detail = Some(Arc::new(hash_detail(&[("name", "ada"), ("profile", "{\"age\":36}")])));
    });
    let (split, entries, count, inspector) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            (
                window.try_find("redis-value-split").is_some(),
                window.try_find("redis-entries").is_some(),
                window.try_find("redis-entry-count").is_some(),
                window.try_find("redis-entry-inspector").is_some(),
            )
        })
        .unwrap();
    assert!(split && entries && count && inspector, "左表右值");
    let (title, focus, rows) = cx.update(|cx| {
        let pane = view.read(cx);
        let viewer = pane.entry_viewer.clone().expect("右栏查看器");
        (viewer.read(cx).title().map(|t| t.to_string()), pane.entry_focus, pane.entry_rows.clone())
    });
    assert_eq!(title.as_deref(), Some("name"), "右栏标题是当前字段名");
    assert_eq!(focus, Some(0), "默认选中第一行");
    assert_eq!(rows, vec![0, 1]);
    let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/views/redis/view.rs")).unwrap();
    assert!(!source.contains("redis.inspector.title") && !source.contains("ENTRY_ROWS_MAX"), "没有单独的查看器标题，也没有非虚拟的条目表");
    assert!(i18n::fill(&i18n::text(Locale::ZhCn, "redis.count.loaded"), &[("n", "2"), ("total", "302")]).contains("2 / 共 302"));

    // 表格里选第二行：右栏换成 profile，同一个查看器实体。
    let before = cx.update(|cx| view.read(cx).entry_viewer.as_ref().map(|viewer| viewer.entity_id()));
    cx.update(|cx| view.update(cx, |pane, cx| pane.select_table_row(ENTRY_TABLE, 1, cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    let (after, title, focus) = cx.update(|cx| {
        let pane = view.read(cx);
        let viewer = pane.entry_viewer.clone().unwrap();
        (Some(viewer.entity_id()), viewer.read(cx).title().map(|t| t.to_string()), pane.entry_focus)
    });
    assert_eq!(focus, Some(1));
    assert_eq!(title.as_deref(), Some("profile"));
    assert_eq!(before, after, "换条目复用同一个查看器");

    // ⌘C：表格把选中的行复制出来。
    let copied = cx.update(|cx| {
        let table = view.read(cx).tables.get(ENTRY_TABLE).map(|slot| slot.table.clone()).unwrap();
        table.update(cx, |table, cx| table.copy_selection(cx))
    });
    assert!(copied.unwrap_or_default().contains("profile"), "选中行可以复制");

    // 双击（表格的 Activate）Hash 行：进入编辑表单，带上字段与值。
    cx.update(|cx| {
        let table = view.read(cx).tables.get(ENTRY_TABLE).map(|slot| slot.table.clone()).unwrap();
        table.update(cx, |_, cx| cx.emit(ReadTableEvent::Activate(0)));
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let pane = view.read(cx);
        assert!(pane.entry_form_open, "双击 Hash 值进入编辑");
        assert_eq!(pane.entry_field, "name");
        assert_eq!(pane.entry_value, "ada");
    });

    // 搜索与排序：筛选后的行映射到原条目下标；降序时 profile 在前。
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.entry_form_open = false;
            pane.command("redis-entry-sort", cx);
            pane.command("redis-entry-sort", cx);
            let detail = pane.detail.clone().unwrap();
            let (_, lines, _) = pane.entry_lines(&detail);
            assert_eq!(lines.iter().map(|line| line.index).collect::<Vec<_>>(), vec![1, 0], "降序");
            pane.hash_query = "ada".into();
            let (_, lines, _) = pane.entry_lines(&detail);
            assert_eq!(lines.iter().map(|line| line.index).collect::<Vec<_>>(), vec![0], "搜索按字段和值过滤");
        })
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// R19：分隔条位置保存在 Redis 偏好里，并夹在两栏的最窄宽度之间；窄详情区改为上下分栏。
#[gpui_kit::test]
fn value_split_width_comes_from_prefs_and_stacks_when_narrow(cx: &mut gpui_kit::TestAppContext) {
    let (services_handle, dir) = services("split");
    let mut pane = RedisPane::new(services_handle);
    assert!((pane.value_left_w(1000.) - 420.).abs() < 0.5, "默认按比例");
    pane.prefs.value_split = 360;
    assert_eq!(pane.value_left_w(1000.), 360.);
    pane.prefs.value_split = 5000;
    assert!(pane.value_left_w(1000.) <= 1000. - 280., "右栏至少留出查看器宽度");
    let prefs = keyspace::KeyspacePrefs { value_split: 380, ..Default::default() };
    let restored = keyspace::KeyspacePrefs::from_json(Some(&prefs.to_json()));
    assert_eq!(restored.value_split, 380, "分隔条位置随偏好保存");
    assert_eq!(keyspace::KeyspacePrefs::from_json(Some("{\"separators\":[\":\"]}")).value_split, 0, "旧偏好没有这个字段时用默认比例");
    let _ = std::fs::remove_dir_all(dir);

    // 900 宽：侧栏占去 304，详情区不到 640，条目表与查看器上下排。
    let (window, _view, dir) = open(cx, 900., |pane| {
        pane.set_keys(vec![key("h", "hash")]);
        pane.done = true;
        pane.selected = "h".into();
        pane.detail_key = "h".into();
        pane.detail = Some(Arc::new(hash_detail(&[("a", "1")])));
    });
    let splitter = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("redis-value-split").is_some(), window.try_find("redis-value-splitter").is_some())
        })
        .unwrap();
    assert_eq!(splitter, (true, false), "窄窗口上下分栏，不画左右分隔条");
    let _ = std::fs::remove_dir_all(dir);
}

/// R19：String 超过 1 MiB 只读前缀预览，查看器进入预览态并给「加载全部」；预览态不能应用修改。
#[gpui_kit::test]
fn large_strings_open_as_a_preview_with_load_full(cx: &mut gpui_kit::TestAppContext) {
    let preview = "x".repeat(keys::STRING_PREVIEW_BYTES as usize);
    let (window, view, dir) = open(cx, 1280., move |pane| {
        pane.set_keys(vec![key("big", "string")]);
        pane.selected = "big".into();
        pane.detail_key = "big".into();
        pane.detail = Some(Arc::new(RedisKeyDetail {
            key_type: "string".into(),
            ttl_ms: Some(-1),
            value: RedisValue::String { text: preview },
            total: Some(3 * keys::STRING_PREVIEW_BYTES),
            next: None,
        }));
    });
    let load = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("format-load-full").is_some()
        })
        .unwrap();
    assert!(load, "大值先预览，并给「加载全部」");
    let locked = cx.update(|cx| view.read(cx).string_viewer.as_ref().map(|viewer| !viewer.read(cx).edits_enabled()));
    assert_eq!(locked, Some(true), "预览态不能编辑");
    assert_eq!(keys::string_read_command("big", Some(3 * keys::STRING_PREVIEW_BYTES)), vec!["GETRANGE", "big", "0", "1048575"]);
    assert_eq!(keys::string_read_command("small", Some(10)), vec!["GET", "small"]);
    let _ = std::fs::remove_dir_all(dir);
}

/// R19：应用修改前比较值的指纹：别人改过就提示刷新、不进确认；没改过才进确认框（仍走 Confirm::Mutation）。
#[test]
fn applying_an_edit_checks_the_value_was_not_changed_by_someone_else() {
    let (services_handle, dir) = services("guard");
    let mut pane = RedisPane::new(services_handle);
    pane.connection_id = "cache".into();
    pane.selected = "h".into();
    pane.detail_key = "h".into();
    let mutation = RedisMutation::HashSet { key: "h".into(), field: "f".into(), value: "new".into() };
    let expected = keys::value_fingerprint(Some("old"));
    pane.finish_guard(mutation.clone(), expected, Ok(Some("changed-by-someone".into())));
    assert!(!pane.confirm_open(), "值已被修改：不进确认");
    assert!(pane.stale_value && pane.error().contains("修改"), "{}", pane.error());
    pane.error.clear();
    pane.finish_guard(mutation.clone(), expected, Ok(None));
    assert!(!pane.confirm_open(), "字段被删了也算被修改");
    pane.finish_guard(mutation, expected, Ok(Some("old".into())));
    assert!(pane.confirm_open(), "没被修改：进入确认框");
    assert!(pane.confirm_preview().contains("HSET"), "{}", pane.confirm_preview());
    assert_eq!(keys::probe_command(&ValueProbe::HashField { key: "h".into(), field: "f".into() }), vec!["HGET", "h", "f"]);
    assert_eq!(keys::probe_command(&ValueProbe::ListIndex { key: "l".into(), index: 3 }), vec!["LINDEX", "l", "3"]);
    assert_eq!(keys::probe_command(&ValueProbe::String { key: "s".into() }), vec!["GET", "s"]);
    assert_ne!(keys::value_fingerprint(Some("")), keys::value_fingerprint(None), "空字符串与不存在不同");
    let _ = std::fs::remove_dir_all(dir);
}

/// R13/R14：诊断页都是可选择复制的只读表；双击慢日志把命令填进命令栏，双击大键打开这个键。
#[gpui_kit::test]
fn diagnostic_tables_select_copy_and_activate(cx: &mut gpui_kit::TestAppContext) {
    // 选键会在后台运行时上发起读取（这里没有真实连接，结果是报错并被代数过滤）。
    cx.executor().allow_parking();
    use crate::adapters::redis::diag::{BigKey, SlowEntry};
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.page = Page::Slowlog;
        pane.diag.slow_loaded = true;
        pane.diag.slowlog = vec![SlowEntry { id: 9, at: 100, micros: 2_000, command: "HGETALL user:1".into(), client: "c".into(), client_name: String::new(), node: String::new() }];
        pane.diag.big = vec![BigKey { key: "orders".into(), key_type: "list".into(), size: 9000, memory: None, node: String::new() }];
    });
    let slow = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.within("redis-slowlog-rows").try_find("read-table").is_some()
        })
        .unwrap();
    assert!(slow, "慢日志是只读表");
    cx.update(|cx| {
        let table = view.read(cx).tables.get("slowlog").map(|slot| slot.table.clone()).unwrap();
        table.update(cx, |table, cx| {
            table.select_row(0, false, cx);
            assert!(table.copy_selection(cx).unwrap_or_default().contains("HGETALL user:1"), "可以选中复制");
            cx.emit(ReadTableEvent::Activate(0));
        });
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let pane = view.read(cx);
        assert_eq!(pane.command_input(), "HGETALL user:1", "双击慢日志把命令填进命令栏");
        assert!(pane.console_open);
    });
    for page in ["clients", "info", "bigkeys"] {
        cx.update(|cx| view.update(cx, |pane, cx| pane.command(&format!("redis-page:{page}"), cx)));
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    }
    let big = cx.update_window(window, |_, window, _| window.within("redis-big-rows").try_find("read-table").is_some()).unwrap();
    assert!(big, "大键是只读表");
    cx.update(|cx| {
        let table = view.read(cx).tables.get("bigkeys").map(|slot| slot.table.clone()).unwrap();
        table.update(cx, |_, cx| cx.emit(ReadTableEvent::Activate(0)));
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let pane = view.read(cx);
        assert_eq!(pane.page(), Page::Keys, "双击大键回到键页");
        assert_eq!(pane.selected(), "orders", "并打开这个键");
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// R14：键列表里双击或回车，在新标签中固定打开这个键（交给外壳的请求带上键名）；带键的规格会直接读这个键。
#[gpui_kit::test]
fn double_click_or_enter_requests_a_pinned_tab(cx: &mut gpui_kit::TestAppContext) {
    // 选键会在后台运行时上发起读取（这里没有真实连接，结果是报错并被代数过滤）。
    cx.executor().allow_parking();
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.set_keys(vec![key("user:1", "hash"), key("user:2", "hash")]);
        pane.done = true;
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.command("redis-open-tab:1", cx);
            assert_eq!(pane.take_open_requests(), vec!["cache|订单缓存|0|user:2".to_owned()]);
            pane.selected = "user:1".into();
            pane.key_list_key(&gpui_kit::KeyDownEvent { keystroke: gpui_kit::Keystroke::parse("enter").unwrap(), is_held: false, prefer_character_input: false }, cx);
            assert_eq!(pane.take_open_requests(), vec!["cache|订单缓存|0|user:1".to_owned()], "回车与双击相同");
        })
    });
    assert_eq!(spec_key("cache|订单缓存|0|a|b").as_deref(), Some("a|b"), "键名里可以有 |");
    assert_eq!(spec_key("cache|订单缓存|0"), None);
    assert_eq!(parse_spec("cache|订单缓存|3|k").map(|(_, _, db)| db).as_deref(), Some("3"));
    let _ = std::fs::remove_dir_all(dir);
}

/// R12：命令栏按 Redis 词法叠加高亮层；输入 HGE 能补全出 HGET 和 HGETALL。
#[gpui_kit::test]
fn command_bar_highlights_and_completes_redis_commands(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| pane.set_command("HGETALL user:1"));
    let overlay = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("redis-command-highlight").is_some()
        })
        .unwrap();
    assert!(overlay, "命令栏叠加 Redis 高亮");
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.set_command("HGE");
            pane.complete_open = true;
            let (_, _, items) = pane.current_completions();
            let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
            assert!(labels.contains(&"HGET") && labels.contains(&"HGETALL"), "{labels:?}");
        })
    });
    let tokens = crate::ui::redis_highlight::lex("HGETALL user:1");
    assert_ne!(tokens[0].kind.highlight(), tokens[1].kind.highlight(), "关键字与键颜色不同");
    let _ = std::fs::remove_dir_all(dir);
}

/// R18：redis 视图目录的裸 px 计数低于 T1 记录的上限（与 kit 棘轮同一计数口径）。
#[test]
fn redis_view_bare_px_stays_below_the_t1_ceiling() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/views/redis");
    let mut total = 0usize;
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.extension().is_none_or(|ext| ext != "rs") || name == "tests.rs" || name.ends_with("_tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut code = String::new();
        for (ix, line) in lines.iter().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") && lines.get(ix + 1).is_some_and(|next| next.trim_start().starts_with("mod ")) {
                break;
            }
            if !trimmed.starts_with("//") {
                code.push_str(line);
                code.push('\n');
            }
        }
        total += code
            .match_indices("px(")
            .filter(|(at, _)| {
                let before = code[..*at].chars().next_back();
                if before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                    return false;
                }
                let rest = code[at + 3..].trim_start();
                let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
                rest.chars().next().is_some_and(|c| c.is_ascii_digit())
            })
            .count();
    }
    const T1_CEILING: usize = 267;
    assert!(total < T1_CEILING, "redis 视图裸 px {total} 处，应低于 T1 上限 {T1_CEILING}");
}
