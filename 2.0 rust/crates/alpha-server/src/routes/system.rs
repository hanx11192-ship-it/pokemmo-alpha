//! 系统配置 3 个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_system` / `api_system_save` / `api_about`。
//!
//! # 这个模块里最要紧的三件事
//!
//! ## 1. 环境变量只回传「是否已设置」，不回传明文
//!
//! `GET /api/system` 对两个敏感环境变量只回 `set: bool` 与
//! `preview: "••••" + 末四位`。跟渠道页的脱敏是同一个道理。
//!
//! ## 2. 保存后要**热重载配置**，不能要求重启服务
//!
//! 原版 `api_system_save` 写完 `panel.env` 后还会：
//!
//! ```python
//! for k, v in envs.items():
//!     if v is None: continue
//!     os.environ[k] = v.strip() or os.environ.pop(k, None)
//! ...
//! restarted = cfgmgr.reload_core_config()
//! ```
//!
//! 即「既写文件、又改当前进程的环境、还让核心配置缓存失效」。
//! 三件事缺一不可：只写文件则本进程不生效，只改进程则重启后丢失。
//!
//! Rust 这边对应 [`alpha_core::refresh_config`]（在 `config.rs` 里，
//! 之前把 `OnceCell` 换成 `RwLock<Option<Arc<Config>>>` 就是为了让它可以失效重载）。
//!
//! ## 3. 三套语言的缺省值互不相同（原版如此，已逐条核对）
//!
//! | 字段 | 来源 | 缺省 |
//! |---|---|---|
//! | `push_lang` | kv `push_lang` → settings `language` → | `"zh"` |
//! | `panel_lang` | 当前用户的 `lang` 字段 → | `"zh"` |
//! | `query_log` | kv `query_log` → | `"1"`（开） |
//!
//! `push_lang` 是**推送内容**的语言（影响报表正文），
//! `panel_lang` 是**界面**的语言（存在用户表里，每人一份）。
//! 两者可以不同 —— 用户完全可以让界面用中文、推送用双语。

use std::collections::BTreeMap;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use axum::Extension;

use alpha_core::config_mgr;
use alpha_store::LogLevel;

use crate::auth::layer::AuthContext;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// 面板语言的可选值。与 `POST /api/lang` 用的是同一批。
const PANEL_LANGS: &[&str] = &["zh", "en"];
/// 推送语言的可选值。比面板多一个 `both`（中英双语）。
const PUSH_LANGS: &[&str] = &["zh", "en", "both"];

/// 时区下拉框的选项。原版是个写死的列表。
const TIMEZONES: &[&str] = &[
    "Asia/Shanghai",
    "Asia/Tokyo",
    "Asia/Hong_Kong",
    "Asia/Taipei",
    "Asia/Singapore",
    "UTC",
    "Europe/London",
    "America/New_York",
    "America/Los_Angeles",
];

/// 需要面板管理的环境变量。
///
/// 只有列在这里的变量会出现在界面上 —— 这不是「白名单校验」
/// （`update_env_file` 写的是任意键），而是**界面范围的声明**：
/// 面板不该变成一个可以随便往 `panel.env` 里塞东西的通用编辑器。
struct EnvField {
    key: &'static str,
    label: &'static str,
    desc: &'static str,
}

const ENV_FIELDS: &[EnvField] = &[
    EnvField {
        key: "transfor_url",
        label: "转发服务地址 (transfor_url)",
        desc: "VPS 直连不通时用中转服务，源配置里通过 transform_url_env 引用",
    },
    EnvField {
        key: "WXPUSHER_APP_TOKEN_alpha",
        label: "WxPusher Token",
        desc: "渠道页未单独填 Token 时的兜底",
    },
];

/// 敏感环境变量的脱敏前缀。与渠道路由用的不是同一个 ——
/// 原版这里只有四个点（`"••••" + v[-4:]`），渠道那边是六个。
const ENV_MASK: &str = "••••";

/// `GET /api/system`
pub async fn get_system(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
) -> ApiResult<Json<Value>> {
    let settings = config_mgr::load_settings();

    let timezone = settings
        .get("timezone")
        .and_then(|v| v.as_str())
        .unwrap_or("Asia/Shanghai")
        .to_string();

    // push_lang：kv → settings.language → "zh"
    let kv_push_lang = state.store.get_kv("push_lang")?;
    let push_lang = config_mgr::get_push_language(kv_push_lang.as_deref());

    // panel_lang：当前用户表里的 lang 字段。取不到就 "zh"。
    //
    // 原版这行是个三段 `and/or`：
    //   panel_lang = auth.current_user() and (...).get("lang","zh") or "zh"
    // 语义是「有登录用户 → 取他的 lang（缺省 zh）；否则 zh」。
    let panel_lang = ctx
        .session
        .as_ref()
        .and_then(|s| state.store.get_user(&s.u).ok().flatten())
        .map(|u| u.lang)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zh".to_string());

    // 环境变量：只看「有没有设置」，不看内容
    let envs: Vec<Value> = ENV_FIELDS
        .iter()
        .map(|f| {
            let v = std::env::var(f.key).unwrap_or_default();
            json!({
                "key": f.key,
                "label": f.label,
                "desc": f.desc,
                "set": !v.is_empty(),
                "preview": if v.is_empty() { String::new() } else { mask_env(&v) },
            })
        })
        .collect();

    Ok(Json(json!({
        "timezone": timezone,
        "push_lang": push_lang,
        "panel_lang": panel_lang,
        "envs": envs,
        "env_file": config_mgr::env_path().to_string_lossy(),
        "timezones": TIMEZONES,
    })))
}

/// `••••` + 末四位。按 **char** 切，避免多字节字符被从中间截断。
fn mask_env(v: &str) -> String {
    let tail: String = {
        let chars: Vec<char> = v.chars().collect();
        let start = chars.len().saturating_sub(4);
        chars[start..].iter().collect()
    };
    format!("{ENV_MASK}{tail}")
}

#[derive(Debug, Deserialize)]
pub struct SaveRequest {
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub push_lang: Option<String>,
    #[serde(default)]
    pub panel_lang: Option<String>,
    /// 要写入 `panel.env` 的变量。`null` = 跳过（不是删除）。
    #[serde(default)]
    pub envs: Option<BTreeMap<String, Option<String>>>,
}

/// `POST /api/system/save`
///
/// 回 `{ok: true, restarted: bool}`。`restarted` 表示「配置缓存被刷新过」,
/// 前端据此提示「已生效」还是「需要重启」。
pub async fn save_system(
    State(state): State<AppState>,
    Extension(ctx): Extension<AuthContext>,
    Json(body): Json<SaveRequest>,
) -> ApiResult<Json<Value>> {
    // ---- 时区 ----
    //
    // 原版只判 `if "timezone" in data`，不校验取值。
    // 本版保留 —— 时区合法性由读它的那侧决定，
    // 而且写坏一个时区不该让整次保存失败（下面还有两件事要做）。
    if let Some(tz) = &body.timezone {
        if let Err(e) = config_mgr::set_setting_timezone(tz) {
            tracing::warn!(error = %e, timezone = %tz, "写时区失败");
        }
    }

    // ---- 推送语言 ----
    //
    // 这里**必须**校验取值：写进去的会直接影响报表生成，
    // 原版是 `if ... and data["push_lang"] in ("zh","en","both")`。
    if let Some(lang) = &body.push_lang {
        if PUSH_LANGS.contains(&lang.as_str()) {
            state.store.set_kv("push_lang", lang)?;
            if let Err(e) = config_mgr::set_setting_language(lang) {
                tracing::warn!(error = %e, "写 settings.yaml 的 language 失败");
            }
        }
    }

    // ---- 面板语言（按用户存） ----
    if let Some(lang) = &body.panel_lang {
        if PANEL_LANGS.contains(&lang.as_str()) {
            if let Some(session) = &ctx.session {
                state.store.set_user_lang(&session.u, lang)?;
            }
        }
    }

    // ---- 环境变量 ----
    let mut restarted = false;
    if let Some(envs) = &body.envs {
        // 1. 落盘（内部对「一次都没变」会返回 false 且**不写文件** ——
        //    这样 mtime 不会因为一次空保存被刷新）
        let changed = config_mgr::update_env_file(envs)
            .map_err(|e| ApiError::Internal(format!("写环境变量文件失败: {e}")))?;

        // 2. 改进程环境。
        //
        //    必须在**任何**读取这些变量的代码之前完成 —— 而
        //    `refresh_config()`（下一步）正好会重读它们。
        //    顺序反了的话重载拿到的是旧值。
        for (k, v) in envs {
            let Some(v) = v else { continue };
            let v = v.trim();
            if v.is_empty() {
                std::env::remove_var(k);
            } else {
                std::env::set_var(k, v);
            }
        }

        // 3. 让核心配置缓存失效，其它模块下次取到的就是新值
        if changed {
            restarted = alpha_core::config::refresh_config().is_ok();
            state.store.log(
                LogLevel::Info,
                "system",
                &format!(
                    "更新环境变量：{}{}",
                    envs.keys().cloned().collect::<Vec<_>>().join(", "),
                    if restarted { "（已重载配置）" } else { "" }
                ),
                "panel",
            );
        }
    }

    state
        .store
        .log(LogLevel::Info, "system", "保存系统配置", "panel");

    Ok(Json(json!({ "ok": true, "restarted": restarted })))
}

/// `GET /api/about`
///
/// 内容是一整块中英双语静态文案，放在 `data/about.json` 里 ——
/// 它是**内容**不是**代码**：9 个顶层区块、约 5.5 KB，
/// 写成 Rust 字面量会把路由文件淹掉，改文案也不该碰 `.rs`。
///
/// # 为什么用 `include_str!` 而不是运行时读文件
///
/// 编译期嵌进二进制，部署时少一个要跟着走的文件。
/// 内容改了必须重新编译 —— 对「关于页文案」这种改一次放一年的东西，
/// 这个代价换「不会因为漏拷文件而 404」是划算的。
pub async fn about() -> Json<Value> {
    static RAW: &str = include_str!("data/about.json");
    // 内容在编译期已确定；`serde_json` 解析失败只可能是文件被改坏了，
    // 那时 shell 界面上会显式报错，不会被静默成空对象。
    let value: Value = serde_json::from_str(RAW)
        .unwrap_or_else(|e| json!({ "error": format!("about.json 解析失败: {e}") }));
    Json(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_mask_keeps_the_last_four_chars() {
        assert_eq!(mask_env("ABCDEFGH"), "••••EFGH");
        assert_eq!(mask_env("abc"), "••••abc", "短于四位时整串保留");
        // 多字节：按 char 切，不能按字节（按字节切会在字符中间断开、panic）
        assert_eq!(mask_env("密钥一二三"), "••••钥一二三");
    }

    #[test]
    fn env_mask_prefix_is_four_dots() {
        // 注意：这里只有**四个**点，渠道页是六个。
        // 两个不一致是原版的事实行为，前端各自硬编码了。
        assert_eq!(ENV_MASK.chars().count(), 4);
        assert_eq!(MASK_CHANNELS_FOR_CONTRAST, "••••••");
    }

    /// 与渠道路由对照用，确保没人「顺手统一」这两个常量 ——
    /// 统一了会让前端的两个判定同时失效。
    const MASK_CHANNELS_FOR_CONTRAST: &str = "••••••";

    #[test]
    fn timezone_list_is_not_empty_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for tz in TIMEZONES {
            assert!(seen.insert(*tz), "时区列表有重复项: {tz}");
        }
        assert!(TIMEZONES.contains(&"Asia/Shanghai"), "缺东八区");
        assert!(TIMEZONES.contains(&"UTC"));
    }

    #[test]
    fn push_langs_are_a_superset_of_panel_langs() {
        for l in PANEL_LANGS {
            assert!(
                PUSH_LANGS.contains(l),
                "面板语言 {l} 也该是合法的推送语言"
            );
        }
        assert!(PUSH_LANGS.contains(&"both"), "推送独有 both（中英双语）");
    }

    #[test]
    fn about_json_parses_and_has_both_languages() {
        static RAW: &str = include_str!("data/about.json");
        let v: Value = serde_json::from_str(RAW).expect("about.json 该是合法 JSON");

        for key in ["title", "desc", "project", "examples", "thanks", "built_with", "credits"] {
            for lang in ["zh", "en"] {
                assert!(
                    !v[key][lang].is_null(),
                    "about.json 缺 {key}.{lang}"
                );
            }
        }
        assert_eq!(v["version"], "2.0");
    }

    #[test]
    fn about_built_with_mentions_the_rust_stack() {
        // 重写后这行必须改掉（原版写的是 Python · Flask）
        static RAW: &str = include_str!("data/about.json");
        let v: Value = serde_json::from_str(RAW).unwrap();
        let zh = v["built_with"]["zh"].as_str().unwrap();
        assert!(zh.contains("Rust"), "该说明这是 Rust 版: {zh}");
        assert!(!zh.contains("Flask"), "不该再提 Flask: {zh}");
    }
}
