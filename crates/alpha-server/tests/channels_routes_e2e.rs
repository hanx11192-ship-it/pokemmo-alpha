//! 分发渠道 6 个接口的端到端测试。
//!
//! # 这批测试的重点在脱敏
//!
//! 渠道路由的 CRUD 部分与插件路由大同小异，真正容易出事的是
//! 「密钥既要不回显、又要在编辑时不被覆盖」这对矛盾要求。
//! 相关用例占了一半篇幅，且都是**走完 HTTP 再直读数据库**验证的 ——
//! 只看响应体会漏掉「回显被脱敏了，但库里已经被掩码覆盖了」这种事。
//!
//! # 不碰网络
//!
//! `test` / `test-all` 会真的往外发 HTTP。所以这两个用例只用
//! **渠道不存在** 或 **没有启用渠道** 这类必然在发请求前就返回的路径。
//! 需要真发请求的验证放在真机冒烟里做，不放在单元测试里 ——
//! 否则测试会依赖外网、变得不可靠。

use std::sync::OnceLock;

use alpha_server::auth::layer::{CSRF_HEADER, CSRF_HEADER_VALUE};
use alpha_server::{build_router, AppState, Config};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

struct TestRoot {
    _dir: tempfile::TempDir,
    app: Router,
    state: AppState,
}

fn root() -> &'static TestRoot {
    static ROOT: OnceLock<TestRoot> = OnceLock::new();
    ROOT.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();

        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/sources.yaml"), "sources: []\n").unwrap();

        // `ALPHA_ROOT` 必须**在**任何 `project_root()` 调用之前设好。
        std::env::set_var("ALPHA_ROOT", &base);

        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 0,
            secret: "test-secret-value-at-least-32-bytes-long".into(),
            data_dir: base.join("data"),
            config_dir: base.join("config"),
            session_ttl: 3600,
            envs: std::collections::HashMap::new(),
        };
        let store = alpha_store::Store::open(cfg.db_path()).unwrap();
        store.init().unwrap();
        let state = AppState::new(cfg, store);
        let app = build_router(state.clone());

        TestRoot {
            _dir: dir,
            app,
            state,
        }
    })
}

/// 拿排他锁并清空渠道表。
///
/// guard **必须**绑到变量上（`let _g = sandbox().await;`），
/// 写成 `sandbox();` 的话锁在语句结束时就放了 —— 测试会偶发失败。
async fn sandbox() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let guard = LOCK.lock().await;
    {
        let conn = root().state.store.lock().unwrap();
        conn.execute("DELETE FROM channels", []).unwrap();
    }
    guard
}

async fn cookie() -> String {
    use tokio::sync::OnceCell;
    static COOKIE: OnceCell<String> = OnceCell::const_new();

    COOKIE
        .get_or_init(|| async {
            let req = Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("content-type", "application/json")
                .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "username": "hanyx", "password": "test-password"
                    }))
                    .unwrap(),
                ))
                .unwrap();
            let resp = root().app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "测试登录应该成功");
            resp.headers()
                .get("set-cookie")
                .expect("登录要下发 cookie")
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_string()
        })
        .await
        .clone()
}

async fn send(req: Request<Body>) -> (StatusCode, Value) {
    let resp = root().app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn get(path: &str) -> (StatusCode, Value) {
    let c = cookie().await;
    send(
        Request::builder()
            .uri(path)
            .header("cookie", c)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn post(path: &str, body: Value) -> (StatusCode, Value) {
    let c = cookie().await;
    send(
        Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("cookie", c)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
}

async fn delete(path: &str) -> (StatusCode, Value) {
    let c = cookie().await;
    send(
        Request::builder()
            .method("DELETE")
            .uri(path)
            .header("cookie", c)
            .header(CSRF_HEADER, CSRF_HEADER_VALUE)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

/// 新建一个渠道，返回它的 id。
async fn add(kind: &str, name: &str, config: Value) -> i64 {
    let (status, body) = post(
        "/api/channels",
        json!({"name": name, "type": kind, "config": config}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "建渠道该成功: {body}");
    body["id"].as_i64().unwrap()
}

/// 直读数据库里某个渠道的原始 config。
///
/// **这一步是这批测试的关键**：只断言响应体的话，
/// 「回显脱敏了、但库里也被掩码覆盖」这个 bug 不会被发现。
fn db_config(cid: i64) -> Value {
    let ch = root().state.store.get_channel(cid).unwrap().unwrap();
    serde_json::to_value(&ch.config).unwrap()
}

// ------------------------------------------------------------ 鉴权

#[tokio::test]
async fn all_channel_routes_require_login() {
    let _g = sandbox().await;
    let paths = [
        ("GET", "/api/channels"),
        ("POST", "/api/channels"),
        ("POST", "/api/channels/test-all"),
        ("POST", "/api/channels/1/edit"),
        ("POST", "/api/channels/1/enable"),
        ("POST", "/api/channels/1/test"),
        ("DELETE", "/api/channels/1"),
    ];
    assert_eq!(paths.len(), 7, "6 个端点，但 GET/POST 同路径算两条");

    for (method, path) in paths {
        let resp = root()
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(CSRF_HEADER, CSRF_HEADER_VALUE)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {path} 未登录时该返回 401"
        );
    }
}

// ------------------------------------------------------------ 列表与类型元信息

#[tokio::test]
async fn list_returns_channels_and_types() {
    let _g = sandbox().await;
    add("wxpusher", "微信推送", json!({"app_token": "AT_12345678"})).await;

    let (status, body) = get("/api/channels").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["channels"].as_array().unwrap().len(), 1);
    assert!(body["types"].is_object(), "该带上渠道类型元信息: {body}");
    assert_eq!(body["types"]["webhook"]["label"], "Webhook（通用 JSON）");
}

#[tokio::test]
async fn list_masks_the_app_token_and_keeps_the_real_one_in_db() {
    let _g = sandbox().await;
    let cid = add(
        "wxpusher",
        "微信",
        json!({"app_token": "AT_abcdefg1234", "topic_ids": "45385"}),
    )
    .await;

    let (_, body) = get("/api/channels").await;
    let cfg = &body["channels"][0]["config"];

    assert_eq!(cfg["app_token"], "••••••1234", "响应里必须是掩码");
    assert_eq!(cfg["topic_ids"], "45385", "非密钥字段不该被掩码");

    // 库里必须还是明文 —— 否则下次真发推送时拿到的就是 `••••••1234`
    assert_eq!(
        db_config(cid)["app_token"], "AT_abcdefg1234",
        "脱敏只能发生在回显这一层，不能写回数据库"
    );
}

#[tokio::test]
async fn list_masks_the_serverchan_send_key() {
    let _g = sandbox().await;
    add("serverchan", "Server酱", json!({"send_key": "SCT_zzzz9999"})).await;

    let (_, body) = get("/api/channels").await;
    assert_eq!(body["channels"][0]["config"]["send_key"], "••••••9999");
}

#[tokio::test]
async fn list_masks_empty_secrets_as_empty_not_as_dots() {
    let _g = sandbox().await;
    // 建一个没填 token 的渠道 —— 前端该看到「空」，而不是一串点
    add("wxpusher", "空 token", json!({"topic_ids": "1"})).await;

    let (_, body) = get("/api/channels").await;
    let cfg = &body["channels"][0]["config"];
    assert!(
        cfg.get("app_token").is_none() || cfg["app_token"] == json!(null),
        "没配过的密钥不该显示成掩码: {cfg}"
    );
}

// ------------------------------------------------------------ 新增

#[tokio::test]
async fn add_requires_name_and_known_type() {
    let _g = sandbox().await;

    let (status, body) = post("/api/channels", json!({"name": "", "type": "webhook"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("名称必填"));

    let (status, body) = post(
        "/api/channels",
        json!({"name": "x", "type": "telegram"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("未知渠道类型"));
}

#[tokio::test]
async fn add_defaults_to_enabled() {
    let _g = sandbox().await;
    // 不带 enabled 字段 —— 原版 `bool(data.get("enabled", True))` 该默认启用
    let cid = add("webhook", "钩子", json!({"url": "https://example.test/h"})).await;
    let ch = root().state.store.get_channel(cid).unwrap().unwrap();
    assert!(ch.enabled, "缺 enabled 字段时该默认启用");
}

#[tokio::test]
async fn add_can_create_a_disabled_channel() {
    let _g = sandbox().await;
    let (status, body) = post(
        "/api/channels",
        json!({"name": "停用的", "type": "webhook", "enabled": false}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cid = body["id"].as_i64().unwrap();
    assert!(!root().state.store.get_channel(cid).unwrap().unwrap().enabled);
}

#[tokio::test]
async fn add_writes_a_log_entry() {
    let _g = sandbox().await;
    add("webhook", "记我一笔", json!({})).await;

    let logs = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(50))
        .unwrap();
    assert!(
        logs.iter()
            .any(|l| l.message.contains("新增分发渠道：记我一笔")),
        "新增该写日志，实际: {:?}",
        logs.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------ 编辑与脱敏的往返

#[tokio::test]
async fn edit_does_not_overwrite_secret_with_its_own_mask() {
    let _g = sandbox().await;
    let cid = add("wxpusher", "微信", json!({"app_token": "AT_real_token_9999"})).await;

    // 模拟前端：把列表里拿到的（脱敏后的）config 原样提交回来，只改别的字段
    let (_, listed) = get("/api/channels").await;
    let round_tripped = listed["channels"][0]["config"].clone();
    assert_eq!(round_tripped["app_token"], "••••••9999");

    let (status, body) = post(
        &format!("/api/channels/{cid}/edit"),
        json!({"name": "改个名", "config": round_tripped}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // 库里的真 token 必须原封不动 —— 这是本模块最要紧的不变量
    assert_eq!(
        db_config(cid)["app_token"], "AT_real_token_9999",
        "掩码值绝不能被写进库里"
    );
    // 而名字该改掉
    assert_eq!(
        root().state.store.get_channel(cid).unwrap().unwrap().name,
        "改个名"
    );
}

#[tokio::test]
async fn edit_accepts_a_genuinely_new_secret() {
    let _g = sandbox().await;
    let cid = add("serverchan", "酱", json!({"send_key": "SCT_old_1111"})).await;

    let (status, _) = post(
        &format!("/api/channels/{cid}/edit"),
        json!({"config": {"send_key": "SCT_new_2222"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        db_config(cid)["send_key"], "SCT_new_2222",
        "真正的新密钥该被写进去"
    );
}

#[tokio::test]
async fn edit_only_touches_submitted_keys() {
    let _g = sandbox().await;
    let cid = add(
        "webhook",
        "钩子",
        json!({"url": "https://old.test/h", "template": "{\"a\":\"{content}\"}"}),
    )
    .await;

    // 只提交 url
    let (status, _) = post(
        &format!("/api/channels/{cid}/edit"),
        json!({"config": {"url": "https://new.test/h"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let cfg = db_config(cid);
    assert_eq!(cfg["url"], "https://new.test/h");
    assert_eq!(
        cfg["template"], "{\"a\":\"{content}\"}",
        "没提交的字段该保持原值（浅合并）"
    );
}

#[tokio::test]
async fn edit_unknown_channel_is_404() {
    let _g = sandbox().await;
    let (status, body) = post("/api/channels/999999/edit", json!({"name": "x"})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("渠道不存在"));
}

#[tokio::test]
async fn edit_blank_name_keeps_the_old_one() {
    let _g = sandbox().await;
    let cid = add("webhook", "原名", json!({})).await;

    let (status, _) = post(
        &format!("/api/channels/{cid}/edit"),
        json!({"name": "   "}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        root().state.store.get_channel(cid).unwrap().unwrap().name,
        "原名",
        "纯空白名字该被忽略"
    );
}

#[tokio::test]
async fn edit_with_all_masked_config_leaves_config_untouched() {
    let _g = sandbox().await;
    let cid = add(
        "wxpusher",
        "微信",
        json!({"app_token": "AT_keep_me_7777", "topic_ids": "1"}),
    )
    .await;

    // 提交一个「全是掩码」的 config —— 等价于用户只改了开关、没动配置
    let (status, _) = post(
        &format!("/api/channels/{cid}/edit"),
        json!({"config": {"app_token": "••••••7777"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let cfg = db_config(cid);
    assert_eq!(cfg["app_token"], "AT_keep_me_7777");
    assert_eq!(cfg["topic_ids"], "1", "其余字段也该完好");
}

// ------------------------------------------------------------ 开关与删除

#[tokio::test]
async fn enable_toggles_the_flag() {
    let _g = sandbox().await;
    let cid = add("webhook", "x", json!({})).await;

    post(&format!("/api/channels/{cid}/enable"), json!({"enabled": false})).await;
    assert!(!root().state.store.get_channel(cid).unwrap().unwrap().enabled);

    post(&format!("/api/channels/{cid}/enable"), json!({"enabled": true})).await;
    assert!(root().state.store.get_channel(cid).unwrap().unwrap().enabled);
}

#[tokio::test]
async fn enable_unknown_channel_still_returns_ok() {
    let _g = sandbox().await;
    // 原版不检查存在性，update 影响 0 行也回 `{ok: true}`。保留这个行为。
    let (status, body) = post("/api/channels/999999/enable", json!({"enabled": true})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
}

#[tokio::test]
async fn delete_removes_the_row() {
    let _g = sandbox().await;
    let cid = add("webhook", "要删的", json!({})).await;

    let (status, _) = delete(&format!("/api/channels/{cid}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(root().state.store.get_channel(cid).unwrap().is_none());
}

#[tokio::test]
async fn delete_unknown_channel_is_404() {
    let _g = sandbox().await;
    let (status, body) = delete("/api/channels/999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("渠道不存在"));
}

#[tokio::test]
async fn delete_logs_the_channel_name_not_just_the_id() {
    let _g = sandbox().await;
    let cid = add("webhook", "有名字的", json!({})).await;
    delete(&format!("/api/channels/{cid}")).await;

    let logs = root()
        .state
        .store
        .list_logs(&alpha_store::LogFilter::new(50))
        .unwrap();
    assert!(
        logs.iter()
            .any(|l| l.message.contains("删除分发渠道：有名字的")),
        "删除日志该带渠道名（所以要删之前先查一次）: {:?}",
        logs.iter().map(|l| &l.message).collect::<Vec<_>>()
    );
}

// ------------------------------------------------------------ 测试推送（不碰网络）

#[tokio::test]
async fn test_unknown_channel_is_404() {
    let _g = sandbox().await;
    let (status, body) = post("/api/channels/999999/test", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("渠道不存在"));
}

#[tokio::test]
async fn test_all_with_no_enabled_channel_is_graceful() {
    let _g = sandbox().await;
    // 建一个**停用**的渠道 —— 不该被发送，于是必然走到「没有启用渠道」分支，
    // 全程不发任何网络请求。
    //
    // 注意 `add()` 不传 `enabled` 时是**启用**的（沿用原版缺省），
    // 所以这里必须显式传 `false` —— 一开始漏了，测试就真的去发请求了
    // （target 是 `example.test`，靠 DNS 失败才没发出去）。
    let (status, body) = post(
        "/api/channels",
        json!({
            "name": "停用的",
            "type": "webhook",
            "enabled": false,
            "config": {"url": "https://example.test/never"},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = post("/api/channels/test-all", json!({})).await;
    // 外层 ok 表达的是「请求被受理」，不是「都发出去了」。原版如此。
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["sent"], json!(0));
    assert_eq!(body["failed"], json!(0), "停用的渠道不该被算进失败数");
    assert!(
        body["error"].as_str().unwrap_or("").contains("没有启用的推送渠道"),
        "该说明为什么一条都没发: {body}"
    );
}

#[tokio::test]
async fn test_all_accepts_custom_content() {
    let _g = sandbox().await;
    // 同样走「没有启用渠道」的分支，但验证自定义正文被接受（不报错）
    let (status, body) = post(
        "/api/channels/test-all",
        json!({"content": "自定义测试正文"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], json!(true));
}
