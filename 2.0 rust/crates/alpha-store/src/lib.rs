//! 面板持久层：SQLite 存储 + 表结构。
//!
//! # 这一层负责什么
//!
//! 面板需要落盘的东西一共几类，全在这一个 crate 里：
//!
//! | 表 | 用途 |
//! |---|---|
//! | `logs` | 运行日志（面板「日志」页） |
//! | `users` | 登录用户（面板「用户」页） |
//! | `channels` | 推送渠道（面板「渠道」页） |
//! | `dispatchers` / `evaluators` | 插件条目（面板「决策器」「评估器」页） |
//! | `kv` | 零散键值（调度开关、上次运行时间等） |
//! | `sessions` | 登录态（**新增**，见下） |
//!
//! # 与原版 `panel/db.py` 的兼容性契约
//!
//! 老库的 6 张表**一字不改**地沿用，所以 Rust 版可以直接指向现网的
//! `panel/panel.db` 而不做任何数据迁移。唯一新增的是 `sessions` 表 ——
//! 原版用 Flask 的签名 cookie 保存登录态，Rust 版换成服务端会话：
//!
//! - 签名 cookie 无法「立即登出」—— 用户点了登出，只要 cookie 还在有效期内，
//!   拷一份就依然能登录，除非轮换密钥（那会把所有人踢下线）。
//! - 服务端会话表可以精确做到「改密码 → 该用户所有设备立刻失效」，
//!   这正是面板「改密码」按钮应有的语义。
//!
//! 新增表不影响老库读取，是纯增量改动。
//!
//! # 时间格式
//!
//! 所有时间戳统一为北京时间 `YYYY-MM-DD HH:MM:SS`，与原版
//! `time.strftime("%Y-%m-%d %H:%M:%S")` 逐字一致。这样：
//!
//! 1. 接管老库时不会出现两种格式混排；
//! 2. 面板按字符串比较即可正确排序（`ORDER BY ts DESC` 直接可用）。

pub mod error;
pub mod schema;
pub mod store;

pub use error::{StoreError, StoreResult};
pub use schema::{DEFAULT_DISPATCHER, DEFAULT_EVALUATOR, SCHEMA};
pub use store::{
    now_stamp, stamp_days_ago, LogFilter, LogLevel, LogRow, NewPlugin, PluginRow, PluginTable,
    Session, Store, User, UserPublic,
};

#[cfg(test)]
mod tests {
    use super::*;
    use alpha_notify::{Channel, ChannelConfig, ChannelKind};

    fn mem() -> Store {
        Store::open_in_memory().expect("打开内存库")
    }

    fn mk_plugin(name: &str, priority: i64) -> NewPlugin {
        NewPlugin {
            name: name.into(),
            filename: format!("{name}.rhai"),
            enabled: true,
            active: false,
            priority,
            description: String::new(),
            is_builtin: false,
        }
    }

    // ------------------------------------------------------------ 时间格式

    /// 时间戳必须是 `YYYY-MM-DD HH:MM:SS` —— 老库的排序、面板的展示都依赖它。
    #[test]
    fn timestamp_is_lexicographically_sortable() {
        let s = now_stamp();
        assert_eq!(s.len(), 19, "长度必须是 19: {s}");
        assert!(
            s.chars().enumerate().all(|(i, c)| match i {
                4 | 7 => c == '-',
                10 => c == ' ',
                13 | 16 => c == ':',
                _ => c.is_ascii_digit(),
            }),
            "格式不符: {s}"
        );
        // 字符串序 == 时间序
        assert!(stamp_days_ago(1) < now_stamp());
    }

    // ------------------------------------------------------------ 初始化

    /// 首次打开必须自动写入内置条目，否则面板上「决策器」页是空的。
    #[test]
    fn init_seeds_builtin_dispatcher_and_evaluator() {
        let s = mem();
        let d = s.list_dispatchers().unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "默认决策器");
        assert!(d[0].active && d[0].enabled && d[0].is_builtin);

        let e = s.list_evaluators().unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "脚本队评估器");
        assert!(e[0].active && e[0].enabled && e[0].is_builtin);
    }

    /// `init()` 要可重复执行 —— 面板每次启动都会跑一遍。
    #[test]
    fn init_is_idempotent() {
        let s = mem();
        s.init().unwrap();
        s.init().unwrap();
        assert_eq!(s.list_dispatchers().unwrap().len(), 1);
        assert_eq!(s.list_evaluators().unwrap().len(), 1);
    }

    /// 重启后不能把用户已激活的插件改回默认项。
    #[test]
    fn init_does_not_override_existing_state() {
        let s = mem();
        let id = s
            .add_plugin(PluginTable::Dispatchers, &mk_plugin("我的决策器", 1))
            .unwrap();
        assert!(s.activate_plugin(PluginTable::Dispatchers, id).unwrap());

        s.init().unwrap();

        let a = s.active_plugin(PluginTable::Dispatchers).unwrap().unwrap();
        assert_eq!(a.name, "我的决策器", "重新 init 不应该动 active 标记");
        assert_eq!(s.list_dispatchers().unwrap().len(), 2);
    }

    // ------------------------------------------------------------ 日志

    #[test]
    fn log_filter_and_order() {
        let s = mem();
        s.log(LogLevel::Info, "fetch", "拉取成功", "lzpoke");
        s.log(LogLevel::Error, "notify", "推送失败", "wxpusher");
        s.log(LogLevel::Info, "fetch", "又拉取成功", "lzpoke");

        // 默认倒序
        let all = s.list_logs(&LogFilter::new(10)).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].id > all[2].id, "必须是时间倒序");

        // 按 kind 过滤
        let f = LogFilter {
            kind: Some("fetch".into()),
            ..LogFilter::new(10)
        };
        assert_eq!(s.list_logs(&f).unwrap().len(), 2);

        // 按 level 过滤
        let f = LogFilter {
            level: Some("error".into()),
            ..LogFilter::new(10)
        };
        let rows = s.list_logs(&f).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, "wxpusher");

        // limit 生效
        assert_eq!(s.list_logs(&LogFilter::new(1)).unwrap().len(), 1);

        // 来源去重列表
        let srcs = s.list_log_sources().unwrap();
        assert_eq!(srcs, vec!["lzpoke".to_string(), "wxpusher".to_string()]);
    }

    #[test]
    fn cleanup_logs_removes_only_old_rows() {
        let s = mem();
        s.log_info("fetch", "今天的", "src");
        // 手写一条 3 天前的日志
        {
            let conn = s.lock().unwrap();
            conn.execute(
                "INSERT INTO logs (ts, level, kind, message, source) VALUES (?,?,?,?,?)",
                rusqlite::params![stamp_days_ago(3), "info", "fetch", "三天前的", "src"],
            )
            .unwrap();
        }
        assert_eq!(s.list_logs(&LogFilter::new(10)).unwrap().len(), 2);

        let n = s.cleanup_logs(2).unwrap();
        assert_eq!(n, 1, "只应删掉 3 天前那条");

        let left = s.list_logs(&LogFilter::new(10)).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].message, "今天的");
    }

    /// 日志写失败**不能**中断主流程 —— 原版这里是 `try/except: pass`。
    #[test]
    fn log_never_panics_on_bad_table() {
        let s = mem();
        {
            let conn = s.lock().unwrap();
            conn.execute("DROP TABLE logs", []).unwrap();
        }
        // 表都没了，但这次调用必须安静返回
        s.log_info("kind", "message", "src");
    }

    // ------------------------------------------------------------ 用户

    #[test]
    fn user_crud_and_unique_conflict() {
        let s = mem();
        assert_eq!(s.count_users().unwrap(), 0);

        s.add_user("hanyx", "hash1", true, "zh").unwrap();
        assert_eq!(s.count_users().unwrap(), 1);

        // 重名 -> Conflict（面板据此回 409）
        let err = s.add_user("hanyx", "hash2", false, "zh").unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "实际: {err:?}");
        assert!(err.is_unique_violation());

        let u = s.get_user("hanyx").unwrap().unwrap();
        assert!(u.is_admin);
        assert_eq!(u.password_hash, "hash1");
        assert!(!u.created_at.is_empty());

        // 改语言 / 改密码
        s.set_user_lang("hanyx", "en").unwrap();
        s.set_user_password("hanyx", "hash3").unwrap();
        let u = s.get_user("hanyx").unwrap().unwrap();
        assert_eq!(u.lang, "en");
        assert_eq!(u.password_hash, "hash3");

        // 公开视图不能带密码哈希
        let pubview = serde_json::to_value(UserPublic::from(&u)).unwrap();
        assert!(pubview.get("password_hash").is_none());
        assert!(serde_json::to_value(&u).unwrap().get("password_hash").is_none());

        assert!(s.delete_user("hanyx").unwrap());
        assert!(!s.delete_user("hanyx").unwrap());
        assert!(s.get_user("hanyx").unwrap().is_none());
    }

    #[test]
    fn list_users_never_leaks_hashes() {
        let s = mem();
        s.add_user("a", "secret-hash", true, "zh").unwrap();
        let v = serde_json::to_value(s.list_users().unwrap()).unwrap();
        let txt = v.to_string();
        assert!(!txt.contains("secret-hash"), "泄露了密码哈希: {txt}");
    }

    // ------------------------------------------------------------ 会话

    #[test]
    fn session_lifecycle_and_expiry() {
        let s = mem();
        s.create_session("tok-1", "hanyx", 12).unwrap();

        let sess = s.get_session("tok-1").unwrap().unwrap();
        assert_eq!(sess.username, "hanyx");
        assert!(sess.expires_at > sess.created_at);

        // 不存在的 token
        assert!(s.get_session("nope").unwrap().is_none());

        s.delete_session("tok-1").unwrap();
        assert!(s.get_session("tok-1").unwrap().is_none());
    }

    /// 过期的会话必须读不出来 —— 否则「记住我 7 天」会变成永久有效。
    #[test]
    fn expired_session_is_rejected() {
        let s = mem();
        s.create_session("tok-old", "hanyx", 1).unwrap();
        {
            let conn = s.lock().unwrap();
            conn.execute(
                "UPDATE sessions SET expires_at=? WHERE token=?",
                rusqlite::params![stamp_days_ago(1), "tok-old"],
            )
            .unwrap();
        }
        assert!(
            s.get_session("tok-old").unwrap().is_none(),
            "过期会话不能被读出来"
        );
        assert_eq!(s.cleanup_sessions().unwrap(), 1);
    }

    /// 改密码要能一键踢掉该用户所有设备。
    #[test]
    fn delete_user_sessions_kicks_all_devices() {
        let s = mem();
        s.add_user("hanyx", "h", true, "zh").unwrap();
        s.create_session("a", "hanyx", 24).unwrap();
        s.create_session("b", "hanyx", 24).unwrap();
        s.create_session("c", "other", 24).unwrap();

        assert_eq!(s.delete_user_sessions("hanyx").unwrap(), 2);
        assert!(s.get_session("a").unwrap().is_none());
        assert!(s.get_session("b").unwrap().is_none());
        assert!(s.get_session("c").unwrap().is_some(), "不该误伤别人");
    }

    // ------------------------------------------------------------ 渠道

    #[test]
    fn channel_crud_roundtrip() {
        let s = mem();
        let cfg = ChannelConfig {
            app_token: Some("AT_xxx".into()),
            topic_ids: Some(serde_json::json!([45385])),
            ..Default::default()
        };
        let id = s
            .add_channel("微信主通道", ChannelKind::Wxpusher, &cfg, true)
            .unwrap();

        let c = s.get_channel(id).unwrap().unwrap();
        assert_eq!(c.name, "微信主通道");
        assert_eq!(c.kind, ChannelKind::Wxpusher);
        assert_eq!(c.config.app_token.as_deref(), Some("AT_xxx"));
        assert_eq!(
            c.config.topic_ids.as_ref().unwrap(),
            &serde_json::json!([45385])
        );

        assert_eq!(s.list_channels().unwrap().len(), 1);
        assert_eq!(s.list_enabled_channels().unwrap().len(), 1);

        // 停用后不再出现在「热路径」查询里
        s.update_channel(id, None, None, None, Some(false)).unwrap();
        assert_eq!(s.list_enabled_channels().unwrap().len(), 0);
        assert_eq!(s.list_channels().unwrap().len(), 1, "管理者仍应看得到");

        assert!(s.delete_channel(id).unwrap());
        assert!(!s.delete_channel(id).unwrap());
    }

    /// 面板每次只提交改动的字段，所以 `config` 必须**浅合并**。
    /// 如果直接整体覆盖，用户改个模板就会把 app_token 抹掉。
    #[test]
    fn update_channel_shallow_merges_config() {
        let s = mem();
        let id = s
            .add_channel(
                "ch",
                ChannelKind::Webhook,
                &ChannelConfig {
                    url: Some("http://127.0.0.1:18080/notify".into()),
                    template: Some(r#"{"msg":"{content}"}"#.into()),
                    ..Default::default()
                },
                true,
            )
            .unwrap();

        // 只改 template
        let patch = ChannelConfig {
            template: Some(r#"{"text":"{content}"}"#.into()),
            ..Default::default()
        };
        assert!(s.update_channel(id, None, None, Some(&patch), None).unwrap());

        let c = s.get_channel(id).unwrap().unwrap();
        assert_eq!(
            c.config.template.as_deref(),
            Some(r#"{"text":"{content}"}"#),
            "改动应生效"
        );
        assert_eq!(
            c.config.url.as_deref(),
            Some("http://127.0.0.1:18080/notify"),
            "url 不该被抹掉"
        );

        // 显式全量替换（导入配置场景）才会覆盖
        let full = ChannelConfig {
            url: Some("http://new/".into()),
            ..Default::default()
        };
        assert!(s.replace_channel_config(id, &full).unwrap());
        let c = s.get_channel(id).unwrap().unwrap();
        assert_eq!(c.config.url.as_deref(), Some("http://new/"));
        assert!(
            c.config.template.is_none(),
            "全量替换应清掉未提及的键"
        );
    }

    #[test]
    fn update_channel_partial_fields() {
        let s = mem();
        let id = s
            .add_channel("old", ChannelKind::Wxpusher, &ChannelConfig::default(), true)
            .unwrap();

        s.update_channel(id, Some("new"), Some(ChannelKind::Serverchan), None, None)
            .unwrap();
        let c = s.get_channel(id).unwrap().unwrap();
        assert_eq!(c.name, "new");
        assert_eq!(c.kind, ChannelKind::Serverchan);
        assert!(c.enabled, "enabled 没传就不该变");

        assert!(!s.update_channel(9999, Some("x"), None, None, None).unwrap());
    }

    /// 未知渠道类型必须显式报错，不能静默变成一个「永远发不出去」的渠道。
    #[test]
    fn unknown_channel_type_surfaces_as_error() {
        let s = mem();
        {
            let conn = s.lock().unwrap();
            conn.execute(
                "INSERT INTO channels (name, type, enabled, config, created_at) \
                 VALUES ('坏渠道','telegram',1,'{}','')",
                [],
            )
            .unwrap();
        }
        let err = s.list_channels().unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("telegram"), "错误信息应点出类型: {msg}");
    }

    /// 渠道能原样序列化成面板要的 JSON（`type` 是保留字，必须重命名）。
    #[test]
    fn channel_serializes_with_type_key() {
        let c = Channel {
            id: Some(1),
            name: "ch".into(),
            kind: ChannelKind::Webhook,
            enabled: true,
            config: ChannelConfig::default(),
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["type"], "webhook");
        assert_eq!(v["id"], 1);
        // 未设置的配置项不该出现在 JSON 里，否则表单会显示一堆空字段
        assert!(v["config"].get("app_token").is_none());
    }

    // ------------------------------------------------------ 插件不变量

    /// **核心不变量**：同表内 active 有且只有一个。
    /// 调度器每轮靠它取出「当前该用哪个插件」，出现 0 个或 2 个都会出问题。
    #[test]
    fn activate_plugin_keeps_exactly_one_active() {
        let s = mem();
        let a = s
            .add_plugin(PluginTable::Dispatchers, &mk_plugin("A", 10))
            .unwrap();
        let b = s
            .add_plugin(PluginTable::Dispatchers, &mk_plugin("B", 20))
            .unwrap();

        assert!(s.activate_plugin(PluginTable::Dispatchers, a).unwrap());
        let actives = s
            .list_dispatchers()
            .unwrap()
            .into_iter()
            .filter(|p| p.active)
            .count();
        assert_eq!(actives, 1);

        assert!(s.activate_plugin(PluginTable::Dispatchers, b).unwrap());
        let rows = s.list_dispatchers().unwrap();
        assert_eq!(
            rows.iter().filter(|p| p.active).count(),
            1,
            "不能出现两个 active"
        );
        assert_eq!(
            s.active_plugin(PluginTable::Dispatchers).unwrap().unwrap().id,
            b
        );
        assert!(!rows.iter().find(|p| p.id == a).unwrap().active);

        // 激活一个不存在的 id
        assert!(!s.activate_plugin(PluginTable::Dispatchers, 9999).unwrap());
        assert_eq!(
            s.active_plugin(PluginTable::Dispatchers).unwrap().unwrap().id,
            b,
            "失败不应破坏原状态"
        );
    }

    /// 激活时会顺带把 `enabled` 打开 —— 否则会出现「激活了一个被禁用的插件」。
    #[test]
    fn activate_plugin_enables_it() {
        let s = mem();
        let mut p = mk_plugin("X", 10);
        p.enabled = false;
        let id = s.add_plugin(PluginTable::Evaluators, &p).unwrap();

        s.activate_plugin(PluginTable::Evaluators, id).unwrap();
        let p = s.get_plugin(PluginTable::Evaluators, id).unwrap().unwrap();
        assert!(p.active && p.enabled);
    }

    /// 两张表互不干扰：激活决策器不能让评估器失去 active。
    #[test]
    fn dispatcher_and_evaluator_tables_are_independent() {
        let s = mem();
        let d = s
            .add_plugin(PluginTable::Dispatchers, &mk_plugin("D2", 1))
            .unwrap();
        s.activate_plugin(PluginTable::Dispatchers, d).unwrap();

        assert!(s.active_plugin(PluginTable::Evaluators).unwrap().is_some());
        assert_eq!(s.list_evaluators().unwrap().len(), 1);
        assert_eq!(s.list_dispatchers().unwrap().len(), 2);
    }

    #[test]
    fn plugin_list_is_ordered_by_priority_then_id() {
        let s = mem();
        for (name, prio) in [("low", 30), ("high", 1), ("mid", 20)] {
            s.add_plugin(PluginTable::Dispatchers, &mk_plugin(name, prio))
                .unwrap();
        }
        let names: Vec<String> = s
            .list_dispatchers()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        // 内置项 priority=10，"mid" 也是 10 —— 同优先级按 id（即插入顺序）排，
        // 内置项 id=1 所以排在 mid 前面。
        assert_eq!(names, vec!["high", "默认决策器", "mid", "low"]);
    }

    /// 内置插件删不掉 —— 用户把两个内置项都删了，面板就没法工作了。
    #[test]
    fn builtin_plugins_cannot_be_deleted() {
        let s = mem();
        let builtin = s.list_dispatchers().unwrap()[0].id;
        assert!(
            !s.delete_plugin(PluginTable::Dispatchers, builtin).unwrap(),
            "内置项应删不动"
        );
        assert_eq!(s.list_dispatchers().unwrap().len(), 1);

        let mine = s
            .add_plugin(PluginTable::Dispatchers, &mk_plugin("mine", 5))
            .unwrap();
        assert!(s.delete_plugin(PluginTable::Dispatchers, mine).unwrap());
    }

    #[test]
    fn add_plugin_rejects_duplicate_name() {
        let s = mem();
        s.add_plugin(PluginTable::Dispatchers, &mk_plugin("dup", 10))
            .unwrap();
        let err = s
            .add_plugin(PluginTable::Dispatchers, &mk_plugin("dup", 10))
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "实际: {err:?}");
        // 同名但不同表是允许的
        s.add_plugin(PluginTable::Evaluators, &mk_plugin("dup", 10))
            .unwrap();
    }

    #[test]
    fn update_plugin_partial_fields() {
        let s = mem();
        let id = s.list_dispatchers().unwrap()[0].id;
        let before = s.get_plugin(PluginTable::Dispatchers, id).unwrap().unwrap();

        s.update_plugin(
            PluginTable::Dispatchers,
            id,
            Some("改名了"),
            None,
            Some(99),
            None,
        )
        .unwrap();

        let after = s.get_plugin(PluginTable::Dispatchers, id).unwrap().unwrap();
        assert_eq!(after.name, "改名了");
        assert_eq!(after.priority, 99);
        assert_eq!(after.description, before.description, "没传的字段不该变");
        assert_eq!(after.filename, before.filename, "filename 不允许被改");

        assert!(!s
            .update_plugin(PluginTable::Dispatchers, 9999, Some("x"), None, None, None)
            .unwrap());
    }

    // ------------------------------------------------------------ kv

    #[test]
    fn kv_upsert_and_json() {
        let s = mem();
        assert_eq!(s.get_kv("nope").unwrap(), None);
        assert_eq!(s.get_kv_or("nope", "fallback"), "fallback");

        s.set_kv("scheduler.enabled", "1").unwrap();
        s.set_kv("scheduler.enabled", "0").unwrap();
        assert_eq!(s.get_kv("scheduler.enabled").unwrap().unwrap(), "0");

        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct Cfg {
            interval: u32,
        }
        s.set_kv_json("cfg", &Cfg { interval: 60 }).unwrap();
        let back: Cfg = serde_json::from_value(s.get_kv_json("cfg").unwrap().unwrap()).unwrap();
        assert_eq!(back, Cfg { interval: 60 });

        assert!(s.get_kv_json("nope").unwrap().is_none());

        // 空串视为「没有值」，不该让调用方拿到解析错误
        s.set_kv("empty", "").unwrap();
        assert!(s.get_kv_json("empty").unwrap().is_none());

        let all = s.all_kv().unwrap();
        assert_eq!(all.len(), 3);

        s.delete_kv("cfg").unwrap();
        assert!(s.get_kv("cfg").unwrap().is_none());
    }

    // ------------------------------------------------------ 落盘 + 共享

    /// 真正的文件库：写进去、重开、还能读出来。
    /// 内存库测不出「建目录」「WAL」这些只在真实路径上才会出的问题。
    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        // 故意用两层不存在的子目录，验证 open() 会自己建
        let path = dir.path().join("nested").join("panel.db");

        {
            let s = Store::open(&path).unwrap();
            s.add_user("hanyx", "h", true, "zh").unwrap();
            s.set_kv("last_run", "2026-09-22 11:22:13").unwrap();
            s.add_channel(
                "ch",
                ChannelKind::Wxpusher,
                &ChannelConfig {
                    app_token: Some("AT_keep".into()),
                    ..Default::default()
                },
                true,
            )
            .unwrap();
        }

        let s = Store::open(&path).unwrap();
        assert_eq!(s.get_user("hanyx").unwrap().unwrap().password_hash, "h");
        assert_eq!(s.get_kv("last_run").unwrap().unwrap(), "2026-09-22 11:22:13");
        let ch = &s.list_channels().unwrap()[0];
        assert_eq!(ch.config.app_token.as_deref(), Some("AT_keep"));
        assert!(path.exists());
        assert_eq!(s.path(), path);
    }

    /// Store 要能跨线程共享（axum 的 State 就是这么用的）。
    #[test]
    fn store_is_shareable_across_threads() {
        let s = mem();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let s = s.clone();
                std::thread::spawn(move || {
                    for j in 0..20 {
                        s.log_info("concurrent", &format!("线程{i} 第{j}条"), "test");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let rows = s.list_logs(&LogFilter::new(1000)).unwrap();
        assert_eq!(rows.len(), 160, "并发写不能丢日志");
    }
}
