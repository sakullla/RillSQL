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
    RedisKeyDetail { key_type: "string".into(), ttl_ms: Some(-1), value: RedisValue::String { text: text.into() }, total: Some(text.len() as u64), next: None, memory: None, encoding: None }
}

fn hash_detail(pairs: &[(&str, &str)]) -> RedisKeyDetail {
    RedisKeyDetail {
        key_type: "hash".into(),
        ttl_ms: Some(-1),
        value: RedisValue::Hash { entries: pairs.iter().map(|(f, v)| KeyValuePair { field: (*f).into(), value: (*v).into() }).collect() },
        total: Some(pairs.len() as u64 + 300),
        next: Some("7".into()),
        memory: None,
        encoding: None,
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
            // 类型筛选是一个下拉：先展开，再点类型；选中即生效并收起。
            window.click("redis-type-toggle", cx);
            window.render_frame(cx);
            window.click(chip, cx);
            window.render_frame(cx);
            assert!(window.try_find("redis-type-picker").is_none(), "选中后下拉收起");
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
            let missing = RedisKeyDetail { key_type: "none".into(), ttl_ms: Some(-2), value: RedisValue::None {}, total: None, next: None, memory: None, encoding: None };
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

/// R10/R18 → R12：键行是固定宽度的类型标签 + 界面字体键名（前缀弱化、从左侧省略，末段完整；悬停给全名、类型与 TTL）；
/// 搜索一行在上，类型筛选在下，类型多时折行而不是被横向裁掉。
#[gpui_kit::test]
fn key_rows_use_type_tags_and_keep_the_last_segment(cx: &mut gpui_kit::TestAppContext) {
    let long = "session:tenant-alpha-production:region-east-1:user:profile:cache:v2:shard-0042:123456789";
    let (window, view, dir) = open(cx, 1280., move |pane| {
        pane.set_keys(vec![key(long, "hash"), key("short", "zset")]);
        pane.done = true;
        pane.view_mode = ViewMode::Flat;
    });
    let (search, types, row_label, tag_w, short_w) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find("redis-tools-search").bounds(),
                window.find("redis-tools-types").bounds(),
                window.find("redis-select:0").label().map(str::to_owned),
                window.within("redis-select:0").find("redis-type-tag").bounds().size.width,
                window.within("redis-select:1").find("redis-type-tag").bounds().size.width,
            )
        })
        .unwrap();
    assert!(search.origin.y < types.origin.y, "搜索与图标按钮一行，类型筛选与视图切换在下面");
    assert!(row_label.unwrap_or_default().contains(long), "无障碍标签给完整键名");
    assert_eq!(tag_w, short_w, "类型标签固定宽度，上下对齐");
    let fitted: Vec<tables::FittedKey> = cx.update(|cx| view.read(cx).fit_cache.values().cloned().collect());
    assert!(
        fitted.iter().any(|fit| fit.last == "123456789" && fit.prefix.strip_prefix('…').is_some_and(|kept| !kept.is_empty() && long.ends_with(&format!("{kept}123456789")))),
        "末段完整保留，前缀从左侧省略：{fitted:?}"
    );
    // 前缀与末段分开着色：前缀弱化，末段正常色。
    let (prefix, last) = cx
        .update_window(window, |_, window, _| (window.try_find("redis-key-prefix:0").is_some(), window.try_find("redis-key-last:0").is_some()))
        .unwrap();
    assert!(prefix && last, "键名拆成前缀与末段两段");
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
            memory: None,
            encoding: None,
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
    let probe = ValueProbe::HashField { key: "h".into(), field: "f".into() };
    pane.finish_guard(mutation.clone(), probe.clone(), expected, Ok(Some("changed-by-someone".into())));
    assert!(!pane.confirm_open(), "值已被修改：不进确认");
    assert!(pane.stale_value && pane.error().contains("修改"), "{}", pane.error());
    pane.error.clear();
    pane.finish_guard(mutation.clone(), probe.clone(), expected, Ok(None));
    assert!(!pane.confirm_open(), "字段被删了也算被修改");
    pane.finish_guard(mutation, probe.clone(), expected, Ok(Some("old".into())));
    assert!(pane.confirm_open(), "没被修改：进入确认框");
    assert!(
        matches!(&pane.confirm, Some(Confirm::Mutation { guard: Some((guard_probe, guard_expected)), .. }) if *guard_probe == probe && *guard_expected == expected),
        "确认框带着核对：确认之后、写入之前还要再核对一次"
    );
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
        assert_eq!(pane.page(), Page::Console, "双击慢日志切到控制台");
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

/// R14：键列表里双击或回车，在当前视图选中这个键，并把固定标签请求（带键名）留给外壳；
/// 外壳取走请求后新建固定标签（见 shell 测试 `redis_open_requests_open_a_pinned_key_tab`）。带键的规格会直接读这个键。
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
            assert_eq!(pane.selected(), "user:2", "双击在当前视图打开这个键");
            assert!(pane.reading(), "并开始读取它");
            assert_eq!(pane.take_open_requests(), vec!["cache|订单缓存|0|user:2".to_owned()]);
            pane.selected = "user:1".into();
            pane.key_list_key(&gpui_kit::KeyDownEvent { keystroke: gpui_kit::Keystroke::parse("enter").unwrap(), is_held: false, prefer_character_input: false }, cx);
            assert_eq!(pane.take_open_requests(), vec!["cache|订单缓存|0|user:1".to_owned()], "回车与双击相同");
            assert_eq!(pane.selected(), "user:1");
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
    // set_command 切到控制台页：命令栏在那里。
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

/// R11：删除 / 改名等待回执期间可以选别的键；回执只作用于发起时的那个键，不按完成时的选中去猜。
#[gpui_kit::test]
fn delete_or_rename_completion_only_touches_the_key_it_was_issued_for(cx: &mut gpui_kit::TestAppContext) {
    cx.executor().allow_parking();
    let (_window, view, dir) = open(cx, 1280., |pane| {
        pane.set_keys(vec![key("a", "string"), key("b", "hash"), key("c", "list")]);
        pane.done = true;
        pane.selected = "a".into();
        pane.detail_key = "a".into();
        pane.detail = Some(Arc::new(string_detail("alpha")));
    });
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            // 确认删除 a，回执还没到时选了 b。
            pane.command("redis-delete", cx);
            assert!(pane.confirm_open());
            pane.command("redis-confirm", cx);
            assert!(pane.running(), "删除在等回执");
            pane.command("redis-select:1", cx);
            assert_eq!(pane.selected(), "b", "等待删除回执时仍可选别的键");
            // b 的详情先到。
            let generation = pane.read_generation;
            assert!(pane.deliver_read(generation, "b".into(), Ok(hash_detail(&[("f", "v")])), cx));
            // 删除 a 的回执到达（真实请求的回执被代数作废，这里直接投递）。
            pane.generation = pane.generation.wrapping_add(1);
            pane.busy = false;
            pane.finish_delete(Ok(("a".into(), crate::application::redis::DeleteOutcome { deleted: 1 })), cx);
            let names: Vec<&str> = pane.keys().iter().map(|key| key.name.as_str()).collect();
            assert_eq!(names, vec!["b", "c"], "删掉的是 a，不是当前选中的 b");
            assert_eq!(pane.selected(), "b", "选中不动");
            assert_eq!(pane.detail_key, "b", "b 的详情还在");
            assert!(pane.detail.is_some());

            // 改名 c → d，等待期间选中的是 b：只把 c 换成 d，选中与详情不动。
            pane.finish_rename(Ok(("c".into(), "d".into())), cx);
            let names: Vec<&str> = pane.keys().iter().map(|key| key.name.as_str()).collect();
            assert_eq!(names, vec!["b", "d"], "c 改名为 d，b 还在");
            assert_eq!(pane.keys().iter().find(|key| key.name == "d").map(|key| key.key_type.as_str()), Some("list"), "新名字沿用原类型");
            assert_eq!(pane.selected(), "b");
            assert_eq!(pane.detail_key, "b");

            // 改名的正是当前选中的键：选中跟到新名字。
            pane.finish_rename(Ok(("b".into(), "b2".into())), cx);
            assert_eq!(pane.selected(), "b2");
            assert!(!pane.keys().iter().any(|key| key.name == "b"));
        })
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// R19：确认之后、写入之前再核对一次：发现值已被改过就不写，提示刷新。
#[gpui_kit::test]
fn a_write_rejected_by_the_second_check_reports_stale(cx: &mut gpui_kit::TestAppContext) {
    let (_window, view, dir) = open(cx, 1280., |pane| {
        pane.selected = "h".into();
        pane.detail_key = "h".into();
    });
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.finish_mutate(Ok(None), cx);
            assert!(pane.stale_value, "标记为过期，给出刷新入口");
            assert!(pane.error().contains("没有写入"), "{}", pane.error());
            assert!(pane.note().is_empty() || !pane.note().contains("已执行"), "{}", pane.note());
        })
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 在两个字段数相同的 Hash 之间切换：行映射一样，表里的选中也要回到当前条目（默认第一行），与右栏一致。
#[gpui_kit::test]
fn entry_table_selection_follows_the_detail_across_same_shaped_keys(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1360., |pane| {
        pane.set_keys(vec![key("h1", "hash"), key("h2", "hash")]);
        pane.done = true;
        pane.selected = "h1".into();
        pane.detail_key = "h1".into();
        pane.entry_focus = Some(0);
        pane.detail = Some(Arc::new(hash_detail(&[("a", "1"), ("b", "2")])));
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| view.update(cx, |pane, cx| pane.select_table_row(ENTRY_TABLE, 1, cx)));
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    // 换到 h2（同样两个字段）：读取落地后右栏回到第一行。
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.selected = "h2".into();
            let generation = pane.begin_read();
            assert!(pane.deliver_read(generation, "h2".into(), Ok(hash_detail(&[("x", "9"), ("y", "8")])), cx));
            assert_eq!(pane.entry_focus, Some(0));
        })
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    let (focus, selected_row) = cx.update(|cx| {
        let pane = view.read(cx);
        let table = pane.tables.get(ENTRY_TABLE).map(|slot| slot.table.clone()).unwrap();
        (pane.entry_focus, table.read(cx).selection(cx).map(|selection| selection.focus.0))
    });
    assert_eq!(focus, Some(0));
    assert_eq!(selected_row, Some(0), "表里高亮的行与右栏显示的条目一致");
    let _ = std::fs::remove_dir_all(dir);
}

/// 回看 A → 字符串键 B → 再回 A（期间 A 多了条目）：这是换键后的重读，不是「加载更多」的追加页，
/// 表里的选中必须回到右栏的当前条目（第一行），不能沿用上次在 A 上的第 7 行。
#[gpui_kit::test]
fn returning_to_a_grown_key_realigns_the_entry_selection(cx: &mut gpui_kit::TestAppContext) {
    let fields: Vec<(String, String)> = (0..10).map(|i| (format!("f{i}"), i.to_string())).collect();
    let grown: Vec<(String, String)> = (0..12).map(|i| (format!("f{i}"), i.to_string())).collect();
    let first = fields;
    let (window, view, dir) = open(cx, 1360., move |pane| {
        pane.set_keys(vec![key("a", "hash"), key("b", "string")]);
        pane.done = true;
        pane.selected = "a".into();
        pane.detail_key = "a".into();
        pane.entry_focus = Some(0);
        let refs: Vec<(&str, &str)> = first.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        pane.detail = Some(Arc::new(hash_detail(&refs)));
    });
    let render = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap()
    };
    render(cx);
    cx.update(|cx| view.update(cx, |pane, cx| pane.select_table_row(ENTRY_TABLE, 7, cx)));
    render(cx);
    assert_eq!(cx.update(|cx| view.read(cx).entry_focus), Some(7));
    // 换到字符串键 B。
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.selected = "b".into();
            let generation = pane.begin_read();
            assert!(pane.deliver_read(generation, "b".into(), Ok(string_detail("hello")), cx));
        })
    });
    render(cx);
    // 回到 A，A 期间多了两个字段。
    let second = grown;
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.selected = "a".into();
            let generation = pane.begin_read();
            let refs: Vec<(&str, &str)> = second.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            assert!(pane.deliver_read(generation, "a".into(), Ok(hash_detail(&refs)), cx));
            assert_eq!(pane.entry_focus, Some(0), "换键回来默认第一条");
        })
    });
    render(cx);
    let (focus, rows, selected_row) = cx.update(|cx| {
        let pane = view.read(cx);
        let table = pane.tables.get(ENTRY_TABLE).map(|slot| slot.table.clone()).unwrap();
        let position = pane.entry_focus.and_then(|index| pane.entry_rows.iter().position(|row| *row == index));
        (position, table.read(cx).rows_len(cx), table.read(cx).selection(cx).map(|selection| selection.focus.0))
    });
    assert_eq!(rows, 12);
    assert_eq!(focus, Some(0));
    assert_eq!(selected_row, focus, "表里高亮的行就是右栏当前条目所在的行");
    let _ = std::fs::remove_dir_all(dir);
}

/// 用户反馈：集合类键右侧的值面板可以关掉（工具栏按钮或面板右上角的关闭），关掉后条目表占满，状态随偏好保存。
#[gpui_kit::test]
fn value_panel_can_be_closed_and_reopened(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1360., |pane| {
        // 不连库：偏好只改内存，不在后台写连接库。
        pane.connection_id.clear();
        pane.set_keys(vec![key("h", "hash")]);
        pane.done = true;
        pane.selected = "h".into();
        pane.detail_key = "h".into();
        pane.entry_focus = Some(0);
        pane.detail = Some(Arc::new(hash_detail(&[("name", "ada")])));
    });
    let probe = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
            (window.try_find("redis-entry-inspector").is_some(), window.try_find("redis-inspector-toggle").is_some())
        })
        .unwrap()
    };
    assert_eq!(probe(cx), (true, true), "默认显示值面板，工具栏有开关");
    // 面板自己的关闭按钮发出 Close。
    cx.update(|cx| {
        let viewer = view.read(cx).entry_viewer.clone().expect("右栏查看器");
        viewer.update(cx, |_, cx| cx.emit(FormatViewerEvent::Close));
    });
    cx.run_until_parked();
    assert!(cx.update(|cx| view.read(cx).prefs.inspector_hidden), "关闭后记进偏好");
    assert_eq!(probe(cx), (false, true), "值面板收起，条目表占满，开关还在");
    cx.update(|cx| view.update(cx, |pane, cx| pane.command("redis-inspector-toggle", cx)));
    assert_eq!(probe(cx), (true, true), "再点开关恢复");
    let prefs = keyspace::KeyspacePrefs { inspector_hidden: true, ..Default::default() };
    assert!(keyspace::KeyspacePrefs::from_json(Some(&prefs.to_json())).inspector_hidden);
    assert!(!keyspace::KeyspacePrefs::from_json(Some("{}")).inspector_hidden, "旧偏好默认显示");
    let _ = std::fs::remove_dir_all(dir);
}

/// 用户反馈：信息页只有一张虚拟化的表（分组 / 指标 / 值），不再是一堆嵌在滚动区里的小表；可以按文字过滤。
#[gpui_kit::test]
fn info_page_is_one_filterable_table(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.page = Page::Info;
        pane.diag.info = vec![
            crate::adapters::redis::diag::InfoSection { name: "Server".into(), items: vec![("redis_version".into(), "7.2.4".into()), ("tcp_port".into(), "6379".into())] },
            crate::adapters::redis::diag::InfoSection { name: "Memory".into(), items: vec![("used_memory".into(), "1048576".into())] },
        ];
    });
    let rows = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| {
            let pane = view.read(cx);
            assert_eq!(pane.tables.keys().filter(|key| key.starts_with("info")).count(), 1, "只有一张信息表");
            let table = pane.tables.get("info").map(|slot| slot.table.clone()).expect("信息表");
            table.read(cx).rows_len(cx)
        })
    };
    assert_eq!(rows(cx), 3, "所有分组的指标都在同一张表里");
    cx.update(|cx| view.update(cx, |pane, _| pane.info_query = "memory".into()));
    assert_eq!(rows(cx), 1, "按分组 / 指标 / 值过滤");
    // 点击分组筛选芯片，只看 Server 分组。
    cx.update(|cx| view.update(cx, |pane, cx| {
        pane.info_query.clear();
        pane.command("redis-info-section:Server", cx);
    }));
    assert_eq!(rows(cx), 2, "按分组芯片过滤只保留 Server 分组");
    let _ = std::fs::remove_dir_all(dir);
}

/// 过期值冲突提示条给出「重试读取」按钮；Escape 能逐层关闭类型下拉、键空间设置与批量 TTL 条。
#[gpui_kit::test]
fn stale_value_banner_shows_refresh_action_and_escape_dismisses_all_popovers(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.plant_key("user:1");
        pane.stale_value = true;
        pane.error_source = ErrorSource::Command;
        pane.error = pane.text("redis.value.stale");
    });
    let has_refresh = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("redis-stale-refresh").is_some()
        })
        .unwrap();
    assert!(has_refresh, "值已被修改的错误条必须带「重试读取」入口");

    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            let esc = gpui_kit::KeyDownEvent {
                keystroke: gpui_kit::Keystroke::parse("escape").unwrap(),
                is_held: false,
                prefer_character_input: false,
            };
            pane.type_picker_open = true;
            pane.key_down(&esc, cx);
            assert!(!pane.type_picker_open, "Escape 应关闭类型下拉");

            pane.prefs_open = true;
            pane.key_down(&esc, cx);
            assert!(!pane.prefs_open, "Escape 应关闭键空间设置");

            pane.batch_ttl_for = Some("order:*".into());
            pane.key_down(&esc, cx);
            assert!(pane.batch_ttl_for.is_none(), "Escape 应关闭批量 TTL 条");
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 模板展开超过抽样上限时显示截断提示行；树模式支持全部展开与全部折叠。
#[gpui_kit::test]
fn template_truncation_row_and_tree_expand_collapse_all(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        let mut keys: Vec<_> = (0..120).map(|i| key(&format!("order:{i}"), "hash")).collect();
        keys.push(key("app:prod:cache:v1", "string"));
        pane.set_keys(keys);
        pane.done = true;
        pane.view_mode = ViewMode::Template;
        pane.open_templates.insert("order:*".into());
    });
    let has_more = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            view.update(cx, |pane, _| pane.key_rows().iter().any(|row| matches!(row, KeyRow::TemplateMore { shown: 100, total: 120, .. })))
        })
        .unwrap();
    assert!(has_more, "超过 100 条成员时应给出截断提示行，引导服务端按模板扫描");

    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.command("redis-view-tree", cx);
            pane.command("redis-tree-expand-all", cx);
            assert!(pane.expanded.contains("app:prod:cache"), "全部展开应展开多层命名空间");
            pane.command("redis-tree-collapse-all", cx);
            assert!(pane.expanded.is_empty(), "全部折叠应清空展开集合");
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 控制台空态提供快捷只读命令；历史条目可一键回填到输入框。
#[gpui_kit::test]
fn console_empty_quick_commands_and_rerun_fill_input(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.page = Page::Console;
    });
    let has_quick = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("redis-console-quick:PING").is_some()
        })
        .unwrap();
    assert!(has_quick, "控制台空态应展示常用只读快捷命令");

    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.console_log.push_back(ConsoleEntry {
                db: "0".into(),
                line: "HGETALL user:42".into(),
                output: "1) name\n2) ada".into(),
                ok: true,
                elapsed: None,
            });
            pane.command("redis-console-rerun:0", cx);
            assert_eq!(pane.command_input(), "HGETALL user:42", "回填按钮应把历史命令填入输入框");
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}

/// 客户端页按关键词过滤后仍精准命中真实客户端 ID；Pub/Sub 消息支持点击频道回填与复制消息体。
#[gpui_kit::test]
fn clients_filter_maps_kill_index_and_pubsub_supports_channel_reply(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.page = Page::Clients;
        pane.diag.clients_loaded = true;
        pane.diag.clients = vec![
            crate::adapters::redis::diag::ClientInfo {
                id: "10".into(),
                addr: "10.0.0.1:5000".into(),
                name: "worker-a".into(),
                age: "120".into(),
                idle: "5".into(),
                db: "0".into(),
                cmd: "get".into(),
                flags: "N".into(),
                node: String::new(),
            },
            crate::adapters::redis::diag::ClientInfo {
                id: "20".into(),
                addr: "10.0.0.2:6000".into(),
                name: "billing-svc".into(),
                age: "3600".into(),
                idle: "10".into(),
                db: "1".into(),
                cmd: "hgetall".into(),
                flags: "N".into(),
                node: String::new(),
            },
        ];
        pane.diag.clients_query = "billing".into();
    });
    cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update(|cx| view.update(cx, |pane, cx| pane.select_table_row("clients", 0, cx)));
    let kill_id = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.try_find("redis-client-kill:1").is_some()
        })
        .unwrap();
    assert!(kill_id, "过滤后选中第 0 行应映射到真实客户端索引 1（id=20）");

    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.page = Page::PubSub;
            pane.diag.messages.push_front(ReceivedMessage {
                channel: "orders.created".into(),
                payload: "{\"id\":99}".into(),
                at: "14:00:00".into(),
            });
            pane.command("redis-pubsub-fill-channel:0", cx);
            assert_eq!(pane.diag.pub_channel, "orders.created", "点击消息频道应回填发布频道");
            pane.command("redis-pubsub-copy:0", cx);
            assert_eq!(pane.copied(), "{\"id\":99}", "复制按钮应复制完整消息体");
        });
    });
    let _ = std::fs::remove_dir_all(dir);
}


fn hash_entries(range: std::ops::Range<usize>) -> Vec<KeyValuePair> {
    range.map(|i| KeyValuePair { field: format!("field:{i:04}"), value: format!("{{\"n\":{i}}}") }).collect()
}

/// R13：加载更多是同一个键的追加页：条目数增加，表里选中的行与滚动位置都不变（不回弹到顶部或选中行）；
/// 读完后页脚说明“已全部加载 N 项”；失败在页脚给原因与重试，已读到的条目不被错误态替换。
#[gpui_kit::test]
fn loading_more_entries_keeps_the_view_and_reports_the_end_or_the_failure(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1360., |pane| {
        pane.set_keys(vec![key("big", "hash")]);
        pane.done = true;
        pane.selected = "big".into();
        pane.detail_key = "big".into();
        pane.entry_focus = Some(0);
        pane.detail = Some(Arc::new(RedisKeyDetail {
            key_type: "hash".into(),
            ttl_ms: Some(-1),
            value: RedisValue::Hash { entries: hash_entries(0..200) },
            total: Some(450),
            next: Some("200".into()),
            memory: Some(40_960),
            encoding: Some("hashtable".into()),
        }));
    });
    let render = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap()
    };
    render(cx);
    let table = cx.update(|cx| view.read(cx).tables.get(ENTRY_TABLE).map(|slot| slot.table.clone()).expect("条目表"));
    // 选中第 3 行，再滚到表尾去点「加载更多」：选中行已不在视口里。
    cx.update(|cx| view.update(cx, |pane, cx| pane.select_table_row(ENTRY_TABLE, 3, cx)));
    render(cx);
    let scrolled = gpui_kit::Point { x: px(0.), y: px(-2400.) };
    cx.update(|cx| table.update(cx, |table, cx| table.set_vertical_offset(scrolled, cx)));
    render(cx);
    let before = cx.update(|cx| table.read(cx).scroll_offsets(cx).0);
    assert!(before.y < px(-1000.), "已滚到表的下部：{before:?}");
    let more = cx.update_window(window, |_, window, _| window.find("redis-value-more").label().map(str::to_owned)).unwrap();
    assert_eq!(more.as_deref(), Some("加载更多"));

    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.value_loading = true;
            pane.finish_value_page(Ok(("big".into(), ValuePage { value: RedisValue::Hash { entries: hash_entries(200..400) }, next: Some("400".into()) })));
        })
    });
    render(cx);
    let (rows, selected, after, focus) = cx.update(|cx| {
        let table = table.read(cx);
        (table.rows_len(cx), table.selection(cx).map(|selection| selection.focus.0), table.scroll_offsets(cx).0, view.read(cx).entry_focus)
    });
    assert_eq!(rows, 400, "条目数增加");
    assert_eq!(selected, Some(3), "选中行不变，不回到第一行");
    assert_eq!(focus, Some(3), "右栏仍是同一个条目");
    assert_eq!(after, before, "滚动位置不回弹");

    // 读取失败：页脚给原因与重试，条目仍在，值区不换成错误态。
    cx.update(|cx| view.update(cx, |pane, _| pane.finish_value_page(Err("连接已断开".into()))));
    render(cx);
    let (error, retry, state, rows) = cx
        .update_window(window, |_, window, cx| {
            (
                window.try_find("redis-value-more-error").is_some(),
                window.find("redis-value-more").label().map(str::to_owned),
                window.within("redis-value").try_find("error-state").is_some(),
                table.read(cx).rows_len(cx),
            )
        })
        .unwrap();
    assert!(error, "页脚说明加载失败的原因");
    assert_eq!(retry.as_deref(), Some("重试"), "失败后按钮变成重试");
    assert!(!state, "已读到的条目不被错误态替换");
    assert_eq!(rows, 400);

    // 最后一页：没有下一页时说明已全部加载，不再有加载按钮。
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.value_more_error = None;
            pane.finish_value_page(Ok(("big".into(), ValuePage { value: RedisValue::Hash { entries: hash_entries(400..450) }, next: None })));
        })
    });
    render(cx);
    let (all, more, rows, selected) = cx
        .update_window(window, |_, window, cx| {
            (
                window.try_find("redis-value-all-loaded").and_then(|node| node.label().map(str::to_owned)),
                window.try_find("redis-value-more").is_some(),
                table.read(cx).rows_len(cx),
                table.read(cx).selection(cx).map(|selection| selection.focus.0),
            )
        })
        .unwrap();
    assert_eq!(rows, 450);
    assert_eq!(selected, Some(3));
    assert!(!more, "读完后没有加载按钮");
    assert_eq!(all.as_deref(), Some("已全部加载 450 项"), "读完后页脚说明已全部加载");
    let _ = std::fs::remove_dir_all(dir);
}

/// R13：有筛选时新一页一条都不匹配（可见行不变）、有排序时新行插到选中行之前，
/// 都仍是追加页：滚动位置不动，选中跟着同一条目，不回弹到选中行。
#[gpui_kit::test]
fn loading_more_under_a_filter_or_sort_keeps_the_view(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1360., |pane| {
        pane.set_keys(vec![key("big", "hash")]);
        pane.done = true;
        pane.selected = "big".into();
        pane.detail_key = "big".into();
        pane.entry_focus = Some(3);
        pane.hash_query = "field:00".into();
        pane.detail = Some(Arc::new(RedisKeyDetail {
            key_type: "hash".into(),
            ttl_ms: Some(-1),
            value: RedisValue::Hash { entries: hash_entries(0..200) },
            total: Some(600),
            next: Some("200".into()),
            memory: None,
            encoding: None,
        }));
    });
    let render = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| {
            window.render_frame(cx);
            window.render_frame(cx);
        })
        .unwrap()
    };
    render(cx);
    let table = cx.update(|cx| view.read(cx).tables.get(ENTRY_TABLE).map(|slot| slot.table.clone()).expect("条目表"));
    let state = |cx: &mut gpui_kit::TestAppContext| {
        cx.update(|cx| {
            let table = table.read(cx);
            (table.rows_len(cx), table.selection(cx).map(|selection| selection.focus.0), table.scroll_offsets(cx).0, view.read(cx).entry_focus)
        })
    };
    let (rows, selected, _, _) = state(cx);
    assert_eq!((rows, selected), (100, Some(3)), "筛选后 field:0000–0099，选中第 3 行");
    let scrolled = gpui_kit::Point { x: px(0.), y: px(-1200.) };
    cx.update(|cx| table.update(cx, |table, cx| table.set_vertical_offset(scrolled, cx)));
    render(cx);
    let before = state(cx).2;
    assert!(before.y < px(-600.), "已滚到表的下部：{before:?}");

    // 筛选下新一页 field:0200–0399 一条都不匹配：可见行不变，但视图也不能跳回选中行。
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.value_loading = true;
            pane.finish_value_page(Ok(("big".into(), ValuePage { value: RedisValue::Hash { entries: hash_entries(200..400) }, next: Some("400".into()) })));
        })
    });
    render(cx);
    let (rows, selected, after, focus) = state(cx);
    assert_eq!(rows, 100, "新一页没有匹配项");
    assert_eq!(selected, Some(3));
    assert_eq!(focus, Some(3));
    assert_eq!(after, before, "筛选下加载更多不回弹");

    // 换成降序、去掉筛选（视图变了，选中对齐到当前条目），再滚到表尾加载更多。
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.hash_query.clear();
            pane.entry_sort = 2;
        })
    });
    render(cx);
    let (rows, selected, _, focus) = state(cx);
    assert_eq!((rows, focus), (400, Some(3)));
    assert_eq!(selected, Some(396), "降序后 field:0003 在倒数第 4 行");
    let scrolled = gpui_kit::Point { x: px(0.), y: px(-3000.) };
    cx.update(|cx| table.update(cx, |table, cx| table.set_vertical_offset(scrolled, cx)));
    render(cx);
    let before = state(cx).2;

    // 新一页 field:0400–0599 降序排在最前：同一条目的行号后移 200，选中跟过去，视口不动。
    cx.update(|cx| {
        view.update(cx, |pane, _| {
            pane.value_loading = true;
            pane.finish_value_page(Ok(("big".into(), ValuePage { value: RedisValue::Hash { entries: hash_entries(400..600) }, next: None })));
        })
    });
    render(cx);
    let (rows, selected, after, focus) = state(cx);
    assert_eq!(rows, 600);
    assert_eq!(focus, Some(3), "右栏仍是同一个条目");
    assert_eq!(selected, Some(596), "选中跟着同一条目换行");
    assert_eq!(after, before, "排序下加载更多不回弹");
    let _ = std::fs::remove_dir_all(dir);
}

/// 改分隔符后，键名拆分缓存作废：平铺行按新分隔符拆前缀与末段。
#[gpui_kit::test]
fn changing_separators_resplits_flat_key_names(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.set_keys(vec![key("alpha:beta_gamma", "string")]);
        pane.done = true;
        pane.view_mode = ViewMode::Flat;
    });
    let lasts = |cx: &mut gpui_kit::TestAppContext| {
        cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.update(|cx| view.read(cx).fit_cache.values().map(|fit| fit.last.to_string()).collect::<Vec<_>>())
    };
    assert_eq!(lasts(cx), vec!["beta_gamma".to_owned()]);
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.prefs.separators = vec![":".to_owned(), "_".to_owned()];
            pane.invalidate_rows();
            cx.notify();
        })
    });
    assert_eq!(lasts(cx), vec!["gamma".to_owned()], "分隔符一变，旧的拆分结果不再使用");
    let _ = std::fs::remove_dir_all(dir);
}

/// R13：键列表「继续扫描」追加的一批键不让列表回弹：滚动位置与选中键都不变。
#[gpui_kit::test]
fn continuing_the_scan_keeps_the_key_list_view(cx: &mut gpui_kit::TestAppContext) {
    let (window, view, dir) = open(cx, 1280., |pane| {
        pane.set_keys((0..300).map(|i| key(&format!("user:{i:04}"), "string")).collect());
        pane.view_mode = ViewMode::Flat;
        pane.selected = "user:0002".into();
    });
    let render = |cx: &mut gpui_kit::TestAppContext| cx.update_window(window, |_, window, cx| window.render_frame(cx)).unwrap();
    render(cx);
    let offset = |cx: &mut gpui_kit::TestAppContext| cx.update(|cx| view.read(cx).key_scroll.0.borrow().base_handle.offset());
    cx.update(|cx| view.update(cx, |pane, _| pane.key_scroll.0.borrow().base_handle.set_offset(gpui_kit::Point { x: px(0.), y: px(-3000.) })));
    render(cx);
    let before = offset(cx);
    assert!(before.y < px(-1000.), "已滚到下面：{before:?}");
    cx.update(|cx| {
        view.update(cx, |pane, cx| {
            pane.apply_scan_events(vec![(pane.scan.epoch, ScanEvent::Batch((300..600).map(|i| key(&format!("user:{i:04}"), "string")).collect()))], cx);
        })
    });
    render(cx);
    render(cx);
    let (after, count, selected) = (offset(cx), cx.update(|cx| view.read(cx).keys().len()), cx.update(|cx| view.read(cx).selected().to_owned()));
    assert_eq!(count, 600, "追加了一批键");
    assert_eq!(after, before, "列表不回弹");
    assert_eq!(selected, "user:0002", "选中键不变");
    let _ = std::fs::remove_dir_all(dir);
}
