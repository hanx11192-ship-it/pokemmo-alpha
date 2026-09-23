//! 现网数据库兼容性验证。
//!
//! # 这个测试在防什么
//!
//! `alpha-store` 的核心承诺是「**直接接管现网的 `panel.db`，不用数据迁移**」。
//! 这个承诺如果只是写在文档里，等真正切换部署时才发现读不出来，代价就太大了
//! —— 那时线上还跑着老 Python 面板，数据是活的。
//!
//! 所以这里拿**真实的生产库副本**跑一遍完整的读取路径。
//!
//! # 为什么不提交这个库文件
//!
//! 里面有真实用户的密码哈希和推送 token。所以把它放在仓库之外，
//! 通过环境变量 `ALPHA_LIVE_DB` 指定路径；没设就跳过（CI 上不会失败）。
//!
//! ```bash
//! scp -P 10026 root@120.220.76.140:/opt/pokemmo_alpha/panel/panel.db /tmp/live_panel.db
//! ALPHA_LIVE_DB=/tmp/live_panel.db cargo test -p alpha-store --test live_compat -- --nocapture
//! ```
//!
//! # 前提：只读
//!
//! 测试必须**绝不写入**原库 —— 所以先复制到临时目录再打开。
//! 所有断言都是读操作。

use alpha_store::{LogFilter, PluginTable, Store};

fn live_db_path() -> Option<String> {
    std::env::var("ALPHA_LIVE_DB").ok().filter(|p| {
        let exists = std::path::Path::new(p).exists();
        if !exists {
            eprintln!("跳过：ALPHA_LIVE_DB={p} 不存在");
        }
        exists
    })
}

/// 把生产库副本复制到临时目录再打开 —— 保证测试过程绝不碰原文件。
fn open_copy() -> Option<(tempfile::TempDir, Store)> {
    let src = live_db_path()?;
    let dir = tempfile::tempdir().expect("建临时目录");
    let dst = dir.path().join("panel.db");
    std::fs::copy(&src, &dst).expect("复制生产库副本");
    let store = Store::open(&dst).expect("用 Rust 打开现网数据库");
    Some((dir, store))
}

/// 每个用例都自带 `_dir` 保证临时目录活到测试结束。
macro_rules! live_test {
    ($name:ident, $store:ident, $body:block) => {
        #[test]
        fn $name() {
            let Some((_dir, $store)) = open_copy() else {
                eprintln!(concat!("跳过 ", stringify!($name), "：未设置 ALPHA_LIVE_DB"));
                return;
            };
            $body
        }
    };
}

// 打开 + `init()` 之后，老表的数据一条都不能少。
live_test!(live_db_reads_all_tables, store, {
    let users = store.list_users().expect("读 users");
    let channels = store.list_channels().expect("读 channels");
    let dispatchers = store.list_dispatchers().expect("读 dispatchers");
    let evaluators = store.list_evaluators().expect("读 evaluators");
    let logs = store.list_logs(&LogFilter::new(20)).expect("读 logs");

    println!(
        "  现网数据: users={} channels={} dispatchers={} evaluators={} logs(样本)={}",
        users.len(),
        channels.len(),
        dispatchers.len(),
        evaluators.len(),
        logs.len()
    );

    assert!(!users.is_empty(), "现网应该有用户");
    assert!(!channels.is_empty(), "现网应该有渠道");
    assert!(!dispatchers.is_empty(), "现网应该有决策器");
    assert!(!evaluators.is_empty(), "现网应该有评估器");
    assert!(!logs.is_empty(), "现网应该有日志");
});

// 用户必须能读出来，且 `is_admin` / `lang` 字段映射正确。
live_test!(live_db_users_map_correctly, store, {
    let users = store.list_users().expect("读 users");
    for u in &users {
        println!("  用户 {} admin={} lang={}", u.username, u.is_admin, u.lang);
        assert!(!u.username.is_empty());
        assert!(u.lang == "zh" || u.lang == "en", "意外的语言值: {}", u.lang);
        assert!(
            u.created_at.is_empty() || u.created_at.len() == 19,
            "created_at 格式异常: {:?}",
            u.created_at
        );
    }

    // 能按用户名查回来，且是同一条记录（这里才拿得到密码哈希）
    let first = &users[0];
    let by_name = store
        .get_user(&first.username)
        .expect("按名查")
        .expect("应能查到");
    assert_eq!(by_name.id, first.id);
    assert_eq!(by_name.is_admin, first.is_admin);
    assert_eq!(by_name.lang, first.lang);
    assert!(
        !by_name.password_hash.is_empty(),
        "{} 的密码哈希不该为空",
        by_name.username
    );
    // 密码哈希必须是 Flask/werkzeug 生成的格式，否则 Rust 版无法校验
    assert!(
        by_name.password_hash.contains('$') || by_name.password_hash.contains(':'),
        "密码哈希格式看起来不是 werkzeug 生成的: {:?}",
        &by_name.password_hash[..by_name.password_hash.len().min(20)]
    );
});

// 渠道配置是现网 JSON —— 必须能被 `ChannelConfig` 反序列化，
// 且 `type` 都是已知类型（否则 `map_channel` 会直接报错）。
live_test!(live_db_channel_configs_deserialize, store, {
    let channels = store.list_channels().expect("读 channels");
    for c in &channels {
        println!(
            "  渠道 #{} {} [{}] enabled={}",
            c.id.unwrap_or(-1),
            c.name,
            c.kind.as_str(),
            c.enabled
        );
        // 类型相关字段必须与类型匹配，否则面板会渲染出一个填不满的表单
        match c.kind {
            alpha_notify::ChannelKind::Wxpusher => assert!(
                c.config.app_token.is_some()
                    || std::env::var("WXPUSHER_APP_TOKEN_alpha").is_ok(),
                "wxpusher 渠道 #{} 既没配 token 也没环境变量",
                c.id.unwrap_or(-1)
            ),
            alpha_notify::ChannelKind::Webhook => {
                assert!(c.config.url.is_some(), "webhook 渠道缺 url")
            }
            alpha_notify::ChannelKind::Serverchan => {
                assert!(c.config.send_key.is_some(), "serverchan 渠道缺 send_key")
            }
        }
    }
});

// 现网的 `active` 不变量：每张表恰好一个激活项。
//
// 如果老库本来就违反这个前提，Rust 版调度器切换后行为会异常 ——
// 必须在这里就发现，而不是等上线。
live_test!(live_db_preserves_single_active_invariant, store, {
    for (label, t) in [
        ("dispatchers", PluginTable::Dispatchers),
        ("evaluators", PluginTable::Evaluators),
    ] {
        let rows = match t {
            PluginTable::Dispatchers => store.list_dispatchers(),
            PluginTable::Evaluators => store.list_evaluators(),
        }
        .expect("读插件表");

        let actives: Vec<&str> = rows
            .iter()
            .filter(|p| p.active)
            .map(|p| p.name.as_str())
            .collect();
        println!(
            "  {label}: 共 {} 项，激活 {} 项 -> {actives:?}",
            rows.len(),
            actives.len()
        );
        assert_eq!(
            actives.len(),
            1,
            "{label} 表激活项不是恰好 1 个 —— 调度器依赖这个不变量"
        );
        assert!(
            store.active_plugin(t).expect("取激活项").is_some(),
            "{label} 取不到激活项"
        );
    }
});

// 时间戳格式必须与 Rust 版一致（都是北京时间 `YYYY-MM-DD HH:MM:SS`）。
// 否则面板按字符串排序会把老数据排错。
live_test!(live_db_timestamps_are_comparable, store, {
    let logs = store.list_logs(&LogFilter::new(30)).expect("读 logs");
    for row in &logs {
        assert_eq!(
            row.ts.len(),
            19,
            "时间戳长度不是 19（老格式混入？）: {:?}",
            row.ts
        );
    }
    // 倒序查询的结果必须真的按时间递减
    let ts: Vec<&str> = logs.iter().map(|r| r.ts.as_str()).collect();
    for w in ts.windows(2) {
        assert!(w[0] >= w[1], "时间倒序被破坏: {} < {}", w[0], w[1]);
    }
    if let Some(first) = ts.first() {
        println!("  最新日志时间: {first}");
        assert!(
            first.starts_with("20"),
            "最新日志时间看起来不像日期: {first}"
        );
    }
});

// kv 表里的值要能原样读出来。
live_test!(live_db_kv_roundtrip, store, {
    let all: Vec<(String, String)> = store.all_kv().expect("读 kv");
    println!("  kv 共 {} 项", all.len());
    for (k, v) in all.iter().take(10) {
        let shown: String = if v.chars().count() > 50 {
            format!("{}…", v.chars().take(50).collect::<String>())
        } else {
            v.clone()
        };
        println!("    {k} = {shown}");
    }
    // 逐个键都能单独查回来
    for (k, v) in &all {
        assert_eq!(store.get_kv(k).expect("读 kv").as_deref(), Some(v.as_str()));
    }
});

// 日志去重来源能读出来（面板筛选下拉要用）。
live_test!(live_db_log_sources, store, {
    let srcs: Vec<String> = store.list_log_sources().expect("读来源");
    println!("  日志来源: {srcs:?}");
    assert!(!srcs.is_empty());
    assert!(srcs.iter().all(|s| !s.is_empty()));
});

// 现网库的 `logs` 表很大，分页查询要能正常工作。
live_test!(live_db_log_pagination_is_fast, store, {
    let start = std::time::Instant::now();
    let page = store.list_logs(&LogFilter::new(100)).expect("读 logs");
    let elapsed = start.elapsed();
    println!("  取 100 条日志耗时 {elapsed:?}");
    assert_eq!(page.len(), 100);
    assert!(
        elapsed.as_millis() < 500,
        "分页查询过慢（{elapsed:?}）—— 检查 idx_logs_ts 索引是否生效"
    );

    // 按 kind 过滤。现网实际用到的 kind 见下面这个列表 ——
    // 注意**不是**我原本猜的 fetch/notify/push，所以断言必须基于真实取值。
    let mut total = 0;
    for kind in ["query", "channel", "scheduler", "evaluator", "debug"] {
        let f = LogFilter {
            kind: Some(kind.to_string()),
            ..LogFilter::new(10)
        };
        let rows = store.list_logs(&f).expect("按 kind 查");
        println!("  kind={kind:<10} 样本 {} 条", rows.len());
        assert!(rows.iter().all(|r| r.kind == kind), "过滤串味了");
        total += rows.len();
    }
    assert!(total > 0, "至少应有一个已知 kind 有数据");
});

// 现网 `logs.level` 里出现了 `spawn` —— 不在标准四档（debug/info/warning/error）里。
//
// 这不是 Rust 版的 bug，是**老库本来就有**的现象：原版 Python 写日志时
// level 参数直接透传字符串，没有任何校验，于是各种值都能落库。
//
// 对 Rust 版的影响：`LogLevel::parse` 把不认识的值归为 `Info`。
// 但 `list_logs` 返回的是**原始字符串**而非枚举，所以面板筛选
// `level=spawn` 仍然能查到东西 —— 这个测试就是钉住这一点。
live_test!(live_db_unusual_log_levels_are_still_queryable, store, {
    let f = LogFilter {
        level: Some("spawn".to_string()),
        ..LogFilter::new(50)
    };
    let rows = store.list_logs(&f).expect("按 level=spawn 查");
    println!("  level=spawn 查到 {} 条", rows.len());
    assert!(
        rows.iter().all(|r| r.level == "spawn"),
        "level 筛选必须按原始字符串精确匹配"
    );

    // 不认识的 level 落到 LogLevel::parse 时归为 Info（这是有意的：
    // 内部日志级别不该因为老库里的脏值而变成 Error）
    assert_eq!(
        alpha_store::LogLevel::parse("spawn"),
        alpha_store::LogLevel::Info
    );
});
