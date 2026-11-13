//! T14：Redis 键浏览与值编辑的四态、窄宽重排与双主题双密度证据。
//! 这些用例只注入模型状态、不发网络请求；渲染走 GPUI 的无头测试窗口。

use super::*;

fn test_services(name: &str) -> (std::sync::Arc<crate::application::AppServices>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("rillsql-redis-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let services = std::sync::Arc::new(crate::application::AppServices::new(std::sync::Arc::new(
        crate::connections::Service::new_for_test(dir.join("connections.sqlite")),
    )));
    (services, dir)
}

/// 指定视口宽度开一个 Redis 面板；setup 在渲染前注入模型状态（不发网络请求）。
fn open_pane_at(
    cx: &mut gpui_kit::TestAppContext,
    services: std::sync::Arc<crate::application::AppServices>,
    width: f32,
    height: f32,
    setup: fn(&mut RedisPane),
) -> (gpui_kit::AnyWindowHandle, gpui_kit::Entity<RedisPane>) {
    use gpui_kit::AppContext as _;
    cx.update(|cx| {
        let bounds = gpui_kit::Bounds {
            origin: gpui_kit::Point::default(),
            size: gpui_kit::size(gpui_kit::px(width), gpui_kit::px(height)),
        };
        let (window, pane) = gpui_kit::open_window(
            gpui_kit::WindowOptions { window_bounds: Some(gpui_kit::WindowBounds::Windowed(bounds)), ..Default::default() },
            cx,
            |_window, cx| {
                cx.new(|_| {
                    let mut pane = RedisPane::new(services);
                    setup(&mut pane);
                    pane
                })
            },
        )
        .expect("redis pane");
        (window, pane)
    })
}

/// 四态判定：加载 → 错误 → 无命中 → 无数据 → 有行，任何两种都不会混成一句话。
/// 模式条件与"这一轮读完没有"必须参与判定，否则带模式扫出 0 个键会被说成"库里没有键"。
#[test]
fn key_list_state_keeps_busy_error_no_match_and_empty_apart() {
    // (busy, scan_failed, total, visible, pattern_active, done)
    assert_eq!(list_state(true, false, 0, 0, false, false), ListState::Loading, "首屏读取：骨架");
    assert_eq!(list_state(true, true, 0, 0, false, true), ListState::Loading, "加载优先于错误");
    assert_eq!(list_state(false, true, 0, 0, false, true), ListState::Error, "读不到不能退化成空态");
    assert_eq!(list_state(false, false, 0, 0, false, true), ListState::Empty, "读完且没有模式条件、键数 0 才是无数据");
    assert_eq!(list_state(false, false, 3, 0, false, true), ListState::NoMatch, "有键但被类型过滤/收起前缀挡住是无命中");
    assert_eq!(list_state(false, false, 3, 3, false, true), ListState::Rows);
    assert_eq!(list_state(true, false, 3, 3, false, true), ListState::Rows, "接着读下一页时已读到的行还在");
    // SCAN 在服务端按模式过滤：读完后命中 0 个键是"没命中"，下一步是清除条件，不是"新建第一个键"。
    assert_eq!(list_state(false, false, 0, 0, true, true), ListState::NoMatch, "带模式读完仍 0 命中：无命中");
    // 但没读完时不能下这个结论：空页是正常的（服务端 MATCH + COUNT 会返回空页），
    // 说"这一轮已读完 / 没有符合条件的键"会把未完成的扫描当结论，还把模式与"再读取一批"一起废掉。
    assert_eq!(list_state(false, false, 0, 0, true, false), ListState::Rows, "带模式但没读完：不下结论，等下一页");
    assert_eq!(list_state(false, false, 0, 0, false, false), ListState::Rows, "这一页 0 个键但还没读完：不宣布空库");
    assert_eq!(list_state(true, false, 0, 0, true, false), ListState::Loading, "带着模式读下一页：仍是加载");
}

/// 空态只在"读完且没有模式条件"时出现；带模式 0 命中画无命中态；没读完什么都不画。
#[gpui_kit::test]
fn empty_state_needs_a_finished_scan_and_no_pattern(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = test_services("empty-gate");
    cx.update(gpui_kit::init);

    // 带模式扫出 0 个键：无命中态 + 清除筛选，不能是"新建键"。
    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| {
        pane.set_pattern("user:*");
        pane.done = true;
    });
    let (no_match, create, clear) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.within("redis-keys").try_find("no-match-state").is_some(),
                window.within("redis-keys").try_find("empty-state").is_some(),
                window.within("redis-keys").try_find("redis-clear-filter").is_some(),
            )
        })
        .unwrap();
    assert!(no_match, "带模式没命中要给无命中态");
    assert!(!create, "带模式没命中不能说成\"这个库还没有键\"");
    assert!(clear, "无命中态的下一步是清除条件");

    // 第一页 0 个键、游标没读完：不画空态，页脚还留着"再读取一批"。
    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| {
        pane.done = false;
        pane.busy = false;
    });
    let (empty, no_match, more, blank) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.within("redis-keys").try_find("empty-state").is_some(),
                window.within("redis-keys").try_find("no-match-state").is_some(),
                window.try_find("redis-more").map(|button| button.visible()).unwrap_or(false),
                window.within("redis-keys").try_find("redis-keys-blank").is_some(),
            )
        })
        .unwrap();
    assert!(!empty, "还没读完不能宣布空库");
    assert!(!no_match, "没有模式条件就不是无命中");
    assert!(more, "页脚要继续提供\"再读取一批\"");
    assert!(blank, "只有页脚的空白列表要有一行弱提示，不能只剩页脚");

    // 带模式 + 没读完：顺序关键——不能因为模式就宣布"没有符合条件的键 / 这一轮已读完"，
    // 那会把未完成的扫描当结论，还会把模式与页脚的"再读取一批"一起废掉。
    let (window, _pane) = open_pane_at(cx, services, 900., 900., |pane| {
        pane.set_pattern("user:*");
        pane.done = false;
        pane.busy = false;
    });
    let (no_match, empty, clear, more, blank) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.within("redis-keys").try_find("no-match-state").is_some(),
                window.within("redis-keys").try_find("empty-state").is_some(),
                window.within("redis-keys").try_find("redis-clear-filter").is_some(),
                window.try_find("redis-more").map(|button| button.visible()).unwrap_or(false),
                window.within("redis-keys").try_find("redis-keys-blank").is_some(),
            )
        })
        .unwrap();
    assert!(!no_match, "带模式但没读完：不能宣布无命中");
    assert!(!empty, "带模式但没读完：更不能宣布空库");
    assert!(!clear, "结论没成立时不该给清除筛选这条下一步");
    assert!(more, "没读完时页脚仍是\"再读取一批\"");
    assert!(blank, "空页要有一行弱提示");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 键列表四态都换成底座组件，并各自带一个当场能点的下一步。
#[gpui_kit::test]
fn key_list_states_render_with_a_next_step(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = test_services("states");
    cx.update(gpui_kit::init);

    // 读完了、没有模式条件、键数为 0：这才是"这个库还没有键"。
    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| pane.done = true);
    let (empty, action, skeleton, error, no_match) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.find_all("empty-state").len(),
                // 键列表与值区域都可能画空态，主操作要按作用域取，避免同名歧义。
                window.within("redis-keys").find("state-action").visible(),
                window.try_find("skeleton-state").is_some(),
                window.try_find("error-state").is_some(),
                window.try_find("no-match-state").is_some(),
            )
        })
        .unwrap();
    assert!(empty >= 1, "没有键时键列表要画空态");
    assert!(action, "空态必须带一个可执行的下一步（新建键）");
    assert!(!skeleton && !error && !no_match, "空态不能同时叠出别的状态");

    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| pane.busy = true);
        let (skeleton, empty) = cx
            .update_window(window, |_, window, cx| {
                window.render_frame(cx);
                (
                    window.within("redis-keys").try_find("skeleton-state").is_some(),
                    // 值区域在未选键时本来就画"还没有选择键"；这里只看键列表有没有先画空态。
                    window.within("redis-keys").try_find("empty-state").is_some(),
                )
            })
            .unwrap();
    assert!(skeleton, "首屏读取画骨架，不用整块转圈也不先说没有键");
    assert!(!empty, "读取中不能先画空态");

    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| {
        pane.plant_key("user:1");
        pane.type_filter = "hash".into();
    });
    let (no_match, skeleton) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("no-match-state").map(|block| block.visible()), window.try_find("skeleton-state").is_some())
        })
        .unwrap();
    assert_eq!(no_match, Some(true), "类型过滤没命中要给无命中态");
    assert!(!skeleton);

    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| {
        pane.error_source = ErrorSource::Scan;
        pane.error = "SCAN 失败：连接已断开".into();
    });
    let (error, retry) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("error-state").map(|block| block.visible()),
                window.try_find("redis-retry-scan").map(|button| button.visible()),
            )
        })
        .unwrap();
    assert_eq!(error, Some(true), "读取失败要画错误态，不能画成没有键");
    assert_eq!(retry, Some(true), "错误态要带重试");

    let (window, _pane) = open_pane_at(cx, services, 900., 900., |pane| {
        pane.plant_key("user:1");
        pane.busy = true;
    });
    let (inline, row) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("inline-loading").is_some(), window.find("redis-select:0").visible())
        })
        .unwrap();
    assert!(inline, "读下一页时用一行增量指示");
    assert!(row, "增量读取不能盖住已读到的键");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 窄窗口：键列表与值区域交替占满整列，切换按钮在窗口内；宽窗口两栏并存且没有切换条。
#[gpui_kit::test]
fn narrow_window_swaps_the_key_list_and_the_value_area(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = test_services("narrow");
    cx.update(gpui_kit::init);

    // 注入选中键并断开连接 id：选中/切换只走本地状态，不会真的发出 READ。
    let (window, pane) = open_pane_at(cx, services.clone(), 640., 720., |pane| {
        pane.plant_key("user:1");
        pane.connection_id.clear();
    });
    let (keys, value, strip, toggle) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("redis-keys").is_some(),
                window.try_find("redis-value").is_some(),
                window.find("redis-nav-strip").visible(),
                window.find("redis-nav-toggle").bounds(),
            )
        })
        .unwrap();
    assert!(!keys, "窄窗口选中键后先看值区域，不把两栏挤成两条窄缝");
    assert!(value, "值区域占满整列");
    assert!(strip, "窄窗口要有切换条");
    assert!(
        f32::from(toggle.origin.x) >= -0.5 && f32::from(toggle.origin.x + toggle.size.width) <= 640.5,
        "切换按钮不能越出窗口"
    );

    cx.update_window(window, |_, window, cx| window.click("redis-nav-toggle", cx)).unwrap();
    let (keys, value, label) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("redis-keys").is_some(),
                window.try_find("redis-value").is_some(),
                window.find("redis-nav-toggle").label().unwrap_or_default().to_owned(),
            )
        })
        .unwrap();
    assert!(keys && !value, "切回来时键列表占满整列");
    assert_eq!(label, i18n::text(Locale::ZhCn, "redis.nav.value"), "切换按钮的文案指向要去的地方");

    // 选中键会让面板自动转到值区域（主从式：点行即看内容）。
    cx.update(|cx| pane.update(cx, |pane, cx| pane.command("redis-select:0", cx)));
    let (keys, value) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (window.try_find("redis-keys").is_some(), window.try_find("redis-value").is_some())
        })
        .unwrap();
    assert!(!keys && value, "窄窗口选中键后自动转到值区域");

    // 宽窗口不受这套切换影响：两栏都在，也没有切换条。
    let (window, _pane) = open_pane_at(cx, services, 1100., 720., |pane| pane.plant_key("user:1"));
    let (keys, value, strip) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("redis-keys").is_some(),
                window.try_find("redis-value").is_some(),
                window.try_find("redis-nav-strip").is_some(),
            )
        })
        .unwrap();
    assert!(keys && value, "宽窗口两栏并存");
    assert!(!strip, "宽窗口不需要切换条");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 读取键内容失败、SCAN 失败、命令失败各自落在自己的上下文里，都带下一步。
#[gpui_kit::test]
fn failures_land_in_the_context_that_produced_them(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    let (services, dir) = test_services("errors");
    cx.update(gpui_kit::init);

    let (window, _pane) = open_pane_at(cx, services.clone(), 900., 900., |pane| {
        pane.plant_key("user:1");
        pane.detail = None;
        pane.error_source = ErrorSource::Detail;
        pane.error = "读取 user:1 失败：连接已断开".into();
    });
    let (error, retry) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("error-state").map(|block| block.visible()),
                window.try_find("redis-retry-read").map(|button| button.visible()),
            )
        })
        .unwrap();
    assert_eq!(error, Some(true), "读不到键内容时值区域给错误态");
    assert_eq!(retry, Some(true), "值区域的错误带重试读取");

    let (window, _pane) = open_pane_at(cx, services, 900., 900., |pane| {
        pane.plant_key("user:1");
        pane.error_source = ErrorSource::Command;
        pane.error = "WRONGTYPE Operation against a key holding the wrong kind of value".into();
        pane.command_input = "LPUSH user:1 x".into();
    });
    let (error, dismiss, status) = cx
        .update_window(window, |_, window, cx| {
            window.render_frame(cx);
            (
                window.try_find("error-state").map(|block| block.visible()),
                window.try_find("redis-error-dismiss").map(|button| button.visible()),
                window.find("redis-status").visible(),
            )
        })
        .unwrap();
    assert_eq!(error, Some(true), "命令失败给统一错误块");
    assert_eq!(dismiss, Some(true), "命令失败带关闭提示，命令原文留在输入框");
    assert!(status, "状态行仍在，但不重复堆同一段错误");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 新增的可执行下一步确实接到模型上：清除筛选、关闭提示、键列表/值切换都能落到状态。
#[gpui_kit::test]
fn state_actions_drive_the_model(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::AppContext as _;
    let (services, dir) = test_services("actions");
    let pane = cx.update(|cx| cx.new(|_| RedisPane::new(services)));
    cx.update(|cx| {
        pane.update(cx, |pane, cx| {
            pane.plant_key("kept");
            pane.set_pattern("user:*");
            pane.type_filter = "hash".into();
            // 没有打开的连接：清筛选后不会真的发出 SCAN，正好只验证状态迁移。
            pane.connection_id.clear();
            pane.command("redis-clear-filter", cx);
            assert!(pane.pattern().is_empty(), "清除筛选要清掉模式");
            assert!(pane.type_filter.is_empty(), "清除筛选要清掉类型过滤");
            assert_eq!(pane.error_source, ErrorSource::Scan);
            assert!(!pane.selected().is_empty(), "清筛选不动选中键");

            pane.error = "连接已断开".into();
            pane.error_source = ErrorSource::Command;
            pane.command("redis-error-dismiss", cx);
            assert!(pane.error().is_empty(), "关闭提示清掉错误");

            pane.viewport_w = 640.;
            pane.nav_choice = None;
            assert!(!pane.keys_visible(640.), "窄窗口已经选中键时先看值区域");
            pane.selected.clear();
            assert!(pane.keys_visible(640.), "窄窗口还没选键时先看键列表");
            pane.command("redis-nav-toggle", cx);
            assert!(!pane.keys_visible(640.));
            pane.command("redis-nav-toggle", cx);
            assert!(pane.keys_visible(640.));
            assert!(pane.keys_visible(1360.), "宽窗口无条件显示键列表");
            pane.nav_choice = Some(false);
            assert!(pane.keys_visible(1360.), "宽窗口不听窄窗口留下的选择");
        });
    });
    let _ = std::fs::remove_dir_all(&dir);
}

/// 浅/深色 × 紧凑/标准 × 1360/900/640：面板不横向溢出，行高随密度，切换条只在窄窗口。
#[gpui_kit::test]
fn redis_pane_holds_both_themes_both_densities_and_three_widths(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::AppContext as _;
    let (services, dir) = test_services("matrix");
    for dark in [false, true] {
        for metrics in [kit::Metrics::compact(), kit::Metrics::standard()] {
            for width in [1360., 900., 640.] {
                cx.update(|cx| {
                    gpui_kit::init(cx);
                    kit::apply_theme(dark, cx);
                    metrics.publish(cx);
                });
                let (window, _pane) = open_pane_at(cx, services.clone(), width, 800., |pane| pane.plant_key("user:1"));
                let (pane_bounds, row_h, value, toggle) = cx
                    .update_window(window, |_, window, cx| {
                        window.render_frame(cx);
                        (
                            window.find("redis-pane").bounds(),
                            window.try_find("redis-select:0").map(|row| f32::from(row.bounds().size.height)),
                            window.try_find("redis-value").is_some(),
                            window.try_find("redis-nav-toggle").map(|button| button.visible()).unwrap_or(false),
                        )
                    })
                    .unwrap();
                let theme = if dark { "深色" } else { "浅色" };
                let pane_w = f32::from(pane_bounds.size.width);
                assert!(pane_w <= width + 0.5, "{theme} 面板宽 {pane_w} 越出 {width}");
                let narrow = width < kit::NARROW_PAGE_WIDTH;
                assert_eq!(toggle, narrow, "{width}: 切换条只在窄窗口出现");
                if narrow {
                    assert!(value, "{theme} {width}: 窄窗口选中键后值区域要占满整列");
                } else {
                    let expected = metrics.tree_row_h + 2.;
                    let row_h = row_h.expect("宽窗口要有关键行");
                    assert!((row_h - expected).abs() < 0.5, "{theme} 行高 {row_h} 应为 {expected}");
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 本文件里用到的 redis.* / common.* 文案键必须真的在字典里（未知键会原样显示成键名）。
#[test]
fn every_redis_copy_key_used_in_the_source_exists_in_the_catalog() {
    fn literals(source: &str) -> Vec<String> {
        let mut keys = Vec::new();
        let mut rest = source;
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('"') else { break };
            let candidate = &after[..end];
            if candidate.starts_with("redis.") || candidate.starts_with("common.") {
                keys.push(candidate.to_owned());
            }
            rest = &after[end + 1..];
        }
        keys
    }
    let catalog = i18n::catalog_keys();
    let found = literals(include_str!("mod.rs"));
    assert!(found.len() >= 30, "扫描到 {} 个文案键，太少说明扫描没生效", found.len());
    for key in found {
        assert!(catalog.contains(&key.as_str()), "{key} 被当作文案键使用，但字典里没有它");
    }
}
