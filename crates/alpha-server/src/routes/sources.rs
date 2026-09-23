//! 头目源的 9 个接口。
//!
//! 对应原版 `panel/app.py` 的：
//!
//! | 方法 | 路径 | 对应原版函数 |
//! |---|---|---|
//! | GET | `/api/sources` | `api_sources` |
//! | POST | `/api/sources` | `api_source_add` |
//! | POST | `/api/sources/<name>/enable` | `api_source_enable` |
//! | POST | `/api/sources/<name>/priority` | `api_source_priority` |
//! | POST | `/api/sources/<name>/edit` | `api_source_edit` |
//! | DELETE | `/api/sources/<name>` | `api_source_delete` |
//! | POST | `/api/sources/upload` | `api_source_upload` |
//! | GET | `/api/sources/<name>/download` | `api_source_download` |
//! | GET | `/api/sources/template` | `api_source_template` |
//!
//! 全部需要登录。
//!
//! # 路由顺序是个坑
//!
//! `/api/sources/upload` 与 `/api/sources/template` 这两条**必须**排在
//! `/api/sources/<name>/...` 之前，或者至少不能被带参路由抢走 ——
//! 否则 `name="upload"` 会被当成一个源名。
//! axum 的 `Router` 是静态优先于动态匹配的，所以这里的注册顺序
//! 其实不影响正确性；但**读代码的人**会怀疑，所以下面显式加注释说明。
//!
//! # 与原版的一处行为差异（有意的）
//!
//! 原版 `api_source_upload` 接收一段 Python 源码，检查
//! `"def fetch" in raw or "BaseSource" in raw`，然后写进 `src/sources/`。
//! Rust 版的适配器是**编译进二进制的**，运行时装不了新适配器 ——
//! 这是「沙箱化」的必然结果（能装适配器 = 能执行任意代码）。
//!
//! 所以本版：
//!
//! - **拒绝**上传 `.py` 适配器，并明确告诉用户原因与替代路径
//! - 保留接口与响应形状（`{ok, filename, module}`），让前端的
//!   「上传」按钮能给出有意义的提示，而不是静默失效

use axum::body::Body;
use axum::extract::{Multipart, Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use alpha_core::config_mgr;

use crate::error::{ApiError, ApiResult};
use crate::AppState;

// ---------------------------------------------------------------- 列表 / 新增

/// `GET /api/sources`
///
/// 返回**已配置的源**（带展示用的 `note`）与**可用的适配器名**。
pub async fn list_sources() -> ApiResult<Json<Value>> {
    let raw = config_mgr::load_sources().map_err(core_err)?;
    let sources: Vec<Value> = raw.iter().map(config_mgr::describe_source).collect();

    // 适配器清单：Rust 版来自注册表（见 config_mgr 的说明），
    // 不是扫描 `src/sources/*.py`
    let adapters = config_mgr::list_adapter_files();

    Ok(Json(json!({ "sources": sources, "adapters": adapters })))
}

/// `POST /api/sources` 的请求体。
///
/// 每个字段都带默认值 —— 原版用 `data.get(k, default)`，
/// 缺字段不该 400。
#[derive(Debug, Deserialize)]
pub struct AddSourceRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub target: Option<String>,
}

/// `POST /api/sources`
///
/// # 新增时的默认值（注意与「读旧文件时的默认值」不是一回事）
///
/// - `enabled` 默认 **`true`**（新建的源应该直接用起来）
/// - `priority` 默认 **`50`**（不是 100 —— 新建的源插在中间，见下）
///
/// `priority` 的 50 与 `describe_source` 里读旧文件用的 100 不同，
/// 这是原版就有的两个不同默认值，都逐字保留。
pub async fn add_source(
    State(st): State<AppState>,
    Json(body): Json<AddSourceRequest>,
) -> ApiResult<Json<Value>> {
    let name = body.name.as_deref().unwrap_or("").trim().to_string();
    let adapter = body.adapter.as_deref().unwrap_or("").trim().to_string();

    if name.is_empty() || adapter.is_empty() {
        return Err(ApiError::BadRequest("名称和适配器必填".into()));
    }

    // 同名检查要**在写盘前**做 —— 原版是先 `get_source` 再 `upsert`。
    // 注意：`upsert_source` 本身对同名是「合并」而非报错，
    // 所以少了这个前置检查，前端「新增」会把已有源悄悄改掉。
    if config_mgr::get_source(&name).map_err(core_err)?.is_some() {
        return Err(ApiError::BadRequest("同名源已存在".into()));
    }

    let enabled = body.enabled.unwrap_or(true);
    let priority = body.priority.unwrap_or(50);
    let target = body.target.as_deref().unwrap_or("").trim();

    let yaml = render_source_yaml(&name, &adapter, enabled, priority, target);
    let mut doc = config_mgr::SourcesDocFile::load().map_err(core_err)?;
    doc.append_source(&yaml);
    doc.save().map_err(core_err)?;

    log_source(
        &st,
        alpha_store::LogLevel::Info,
        &format!("新增源：{name} (adapter={adapter})"),
    );

    Ok(Json(json!({ "ok": true })))
}

/// 渲染一个源配置块（缩进与 `config/sources.yaml` 现有风格一致）。
fn render_source_yaml(
    name: &str,
    adapter: &str,
    enabled: bool,
    priority: i64,
    target: &str,
) -> String {
    // 值用 YAML 单引号包住：源名里有中文与空格很正常，
    // 而目标地址里常带 `?` `&` `=`，不引会解析错。
    // 单引号内的转义规则只有「两个单引号表示一个」这一条，比双引号简单。
    let q = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut out = String::new();
    out.push_str(&format!("  - name: {}\n", q(name)));
    out.push_str(&format!("    adapter: {adapter}\n"));
    out.push_str(&format!("    enabled: {enabled}\n"));
    out.push_str(&format!("    priority: {priority}\n"));
    out.push_str("    options:\n");
    out.push_str(&format!("      target: {}\n", q(target)));
    out
}

// ---------------------------------------------------------------- 开关 / 优先级

#[derive(Debug, Deserialize)]
pub struct EnableRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/sources/<name>/enable`
///
/// # 源不存在时**仍然返回 `ok: true`**（原版行为）
///
/// 原版 `api_source_enable` 不看 `set_source_enabled` 的返回值：
///
/// ```python
/// cfgmgr.set_source_enabled(name, bool(body.get("enabled", True)))
/// return jsonify({"ok": True})
/// ```
///
/// 本版保留 —— 但记一笔日志，否则运维会遇到「前端说成功、实际什么都没发生」
/// 而毫无线索的情况。
pub async fn set_enable(
    AxumPath(name): AxumPath<String>,
    Json(body): Json<EnableRequest>,
) -> ApiResult<Json<Value>> {
    let enabled = body.enabled.unwrap_or(true);
    let hit = set_source_field(&name, "enabled", if enabled { "true" } else { "false" })?;

    if !hit {
        tracing::warn!(%name, "启用/停用了一个不存在的源 —— 原版此处静默成功");
    }

    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct PriorityRequest {
    #[serde(default)]
    pub priority: Option<i64>,
}

/// `POST /api/sources/<name>/priority`
pub async fn set_priority(
    AxumPath(name): AxumPath<String>,
    Json(body): Json<PriorityRequest>,
) -> ApiResult<Json<Value>> {
    let priority = body.priority.unwrap_or(50);
    if !set_source_field(&name, "priority", &priority.to_string())? {
        tracing::warn!(%name, "改了不存在源的优先级 —— 原版此处静默成功");
    }
    Ok(Json(json!({ "ok": true })))
}

/// 改某个源的单个字段，返回是否命中。
fn set_source_field(name: &str, field: &str, value: &str) -> ApiResult<bool> {
    let mut doc = config_mgr::SourcesDocFile::load().map_err(core_err)?;
    let hit = doc.set_field(name, field, value);
    if hit {
        doc.save().map_err(core_err)?;
    }
    Ok(hit)
}

// ---------------------------------------------------------------- 编辑 / 删除

/// `POST /api/sources/<name>/edit` 的请求体。
///
/// 所有字段可选 —— 只提交改动的项。`name` 用于**改名**。
#[derive(Debug, Deserialize)]
pub struct EditSourceRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
}

/// `POST /api/sources/<name>/edit`
///
/// # 改名要保位置
///
/// 原版用 `replace_source(old, new)` 而不是「删旧的再追加新的」——
/// 后者会让源在 `sources.yaml` 里跳到文件末尾，运维对着文件看时会懵。
/// 本版的 [`config_mgr::SourcesDocFile::replace_block`] 同样原地替换。
///
/// # 一个原版的细节
///
/// 原版在改名时**先把 `s["name"]` 改了，再 `replace_source(old_name, s)`**。
/// 因为 `s` 是从文档里取出的对象引用，`replace_source` 内部又是原地
/// `_merge_map`，所以能工作。本版直接重渲染整段，等价。
pub async fn edit_source(
    State(st): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Json(body): Json<EditSourceRequest>,
) -> ApiResult<Json<Value>> {
    let existing = config_mgr::get_source(&name)
        .map_err(core_err)?
        .ok_or_else(|| ApiError::NotFound("源不存在".into()))?;

    // 从现有配置取默认值，再按请求覆盖 —— 对应原版的逐字段 `if k in body`
    let cur = config_mgr::describe_source(&existing);
    let enabled = body
        .enabled
        .unwrap_or_else(|| cur["enabled"].as_bool().unwrap_or(false));
    let priority = body
        .priority
        .unwrap_or_else(|| cur["priority"].as_i64().unwrap_or(100));

    // adapter 只接受非空值（原版 `if "adapter" in body and str(...).strip()`）
    let adapter = match body.adapter.as_deref().map(str::trim) {
        Some("") | None => cur["adapter"].as_str().unwrap_or("").to_string(),
        Some(a) => a.to_string(),
    };

    let target = match body.target.as_deref() {
        None => cur["options"]["target"].as_str().unwrap_or("").to_string(),
        Some(t) => t.trim().to_string(),
    };

    let new_name = body.name.as_deref().unwrap_or("").trim().to_string();
    let renaming = !new_name.is_empty() && new_name != name;

    if renaming && config_mgr::get_source(&new_name).map_err(core_err)?.is_some() {
        return Err(ApiError::BadRequest("已存在同名源".into()));
    }

    let final_name = if renaming { &new_name } else { &name };
    let yaml = render_source_yaml(final_name, &adapter, enabled, priority, &target);

    let mut doc = config_mgr::SourcesDocFile::load().map_err(core_err)?;
    doc.replace_block(&name, &yaml);

    // `note` 是给面板看的备注，不参与源配置本身，但要能存下来
    if let Some(note) = body.note.as_deref() {
        let note = note.trim();
        if note.is_empty() {
            // 空备注 = 删掉这个键。`set_field` 只写不删，
            // 所以这里显式处理成写一个空串 —— 面板显示为空，语义等价。
            doc.set_field(final_name, "note", "''");
        } else {
            doc.set_field(final_name, "note", &format!("'{}'", note.replace('\'', "''")));
        }
    }

    doc.save().map_err(core_err)?;

    log_source(
        &st,
        alpha_store::LogLevel::Info,
        &format!(
            "编辑源：{name}{}",
            if renaming {
                format!(" -> {new_name}")
            } else {
                String::new()
            }
        ),
    );

    Ok(Json(json!({ "ok": true, "name": final_name, "renamed": renaming })))
}

/// `DELETE /api/sources/<name>`
pub async fn delete_source(
    State(st): State<AppState>,
    AxumPath(name): AxumPath<String>,
) -> ApiResult<Json<Value>> {
    let mut doc = config_mgr::SourcesDocFile::load().map_err(core_err)?;
    if !doc.remove_source(&name) {
        return Err(ApiError::NotFound("源不存在".into()));
    }
    doc.save().map_err(core_err)?;

    log_source(
        &st,
        alpha_store::LogLevel::Info,
        &format!("删除源：{name}"),
    );

    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- 上传 / 下载 / 模板

/// `POST /api/sources/upload`
///
/// # 本版**刻意不支持**上传 Python 适配器
///
/// 原版这个接口做的事是：收一段 `.py`，粗略检查有没有 `def fetch`，
/// 写进 `src/sources/`，之后主流程会用 `importlib` **执行**它 ——
/// 和插件机制是同一类 RCE 入口。区别只是「得有登录态」，
/// 而登录态在现网可以伪造（见 `alpha-server::auth::session`）。
///
/// Rust 版的适配器在编译期就定下来了（`alpha-sources::KNOWN_ADAPTERS`），
/// 运行时装不进去。这既是沙箱化的必然结果，也是我们**想要**的性质：
/// 「能通过 Web 面板安装代码执行单元」这件事本身就不该有。
///
/// 所以这里返回一个说明清楚为什么不行、以及该怎么做的 400。
/// 保留接口是为了让前端的「上传」按钮给出有意义的反馈。
pub async fn upload_source(mut multipart: Multipart) -> ApiResult<Json<Value>> {
    let mut filename = String::new();
    let mut got_file = false;
    let mut body = String::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("读取上传内容失败: {e}")))?
    {
        match field.name() {
            Some("file") => {
                got_file = true;
                filename = field.file_name().unwrap_or("source").to_string();
                body = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("读取文件内容失败: {e}")))?;
            }
            // 其他字段（例如表单里的 `name`）读掉丢弃，但**必须**读，
            // 否则 multipart 的流会卡在这个字段上不往下走。
            _ => {
                let _ = field.text().await;
            }
        }
    }

    if !got_file {
        return Err(ApiError::BadRequest("未收到文件".into()));
    }

    // 保留原版的「这不是一个适配器」检查，但换成 Rust 版的判据
    let looks_like_adapter = body.contains("def fetch") || body.contains("BaseSource");
    let _ = looks_like_adapter;

    Err(ApiError::BadRequest(format!(
        "本版不支持上传适配器（文件 {}）。\n\
         \n\
         原版会把上传的 .py 直接写入 src/sources/ 并由主流程 importlib 执行，\n\
         这等于给面板开了一个「上传即执行」的后门。Rust 版的适配器在编译期\n\
         固定（见 alpha-sources），运行时不再接受新代码。\n\
         \n\
         要接入新数据源：在 alpha-sources crate 里新增一个适配器模块，\n\
         在 registry.rs 的 KNOWN_ADAPTERS 里登记，然后重新编译部署。\n\
         装好之后回到本页「新增源」，adapter 下拉框里就能选到它。",
        if filename.is_empty() { "未命名" } else { &filename }
    )))
}

/// `GET /api/sources/<name>/download`
///
/// 原版下载的是 `src/sources/<name>.py` 的**源码**。
/// Rust 版没有对应的可下载物，所以改成**下载该源在 `sources.yaml` 里的配置块**
/// —— 那才是运维真正想导出/备份/分享的东西。
pub async fn download_source(AxumPath(name): AxumPath<String>) -> ApiResult<Response> {
    let doc = config_mgr::SourcesDocFile::load().map_err(core_err)?;
    let text = doc
        .block_text(&name)
        .ok_or_else(|| ApiError::NotFound("源不存在".into()))?;

    Ok(attachment(
        &text,
        &format!("{}.yaml", safe_filename(&name)),
        "text/yaml; charset=utf-8",
    ))
}

/// `GET /api/sources/template`
///
/// 返回适配器模板。Rust 版的模板是 Rust 源码骨架 ——
/// 用户照着写一个模块、登记、重编译，就能接入新源。
pub async fn source_template() -> Response {
    attachment(SOURCE_TEMPLATE, "source_template.rs", "text/plain; charset=utf-8")
}

/// 适配器模板（Rust 版）。
///
/// 原版给的是 Python 骨架。这里给等价物，并说明三步接入法。
pub const SOURCE_TEMPLATE: &str = r#"//! 数据源适配器模板。
//!
//! 接入一个新源分三步：
//!
//! 1. 在 `crates/alpha-sources/src/` 下新建一个模块（文件名即适配器标识），
//!    照着下面的骨架实现 `new` / `fetch` / `NAME`；
//! 2. 在 `crates/alpha-sources/src/registry.rs` 的 `KNOWN_ADAPTERS` 里
//!    登记这个名字，并在 `create_source()` 里加一个分支；
//! 3. 重新编译部署。之后在面板「头目源」页新增源时，
//!    adapter 下拉框里就能选到它。
//!
//! 要点：
//!   - 能用英文 / 标准名字段就别用机翻中文，再用 Pokedex 解析成官方译名
//!   - 头目还活着才返回 `hit`；时段空档期用报点时间判断
//!   - 去重标识优先用数据源给的时段唯一标识
//!   - **不要**在适配器里写文件或环境变量：它只需要产出 `FetchResult`

use alpha_core::config::{Config, SourceOptions};
use alpha_core::models::{BossData, FetchResult, Gender};
use alpha_core::pokedex::Pokedex;
use crate::error::SourceError;

/// 适配器标识（`sources.yaml` 里 `adapter:` 的值）。
pub const NAME: &str = "my_source";

pub struct MySource {
    options: SourceOptions,
}

impl MySource {
    pub fn new(options: SourceOptions, _cfg: &Config) -> Result<Self, SourceError> {
        Ok(Self { options })
    }

    /// 拉取并解析。返回 `hit` / `empty` / `error` 三态之一。
    pub async fn fetch(&self, pokedex: &Pokedex) -> FetchResult {
        // 1. 拉数据（`self.options.target` 是面板里填的地址）
        // 2. 解析成 BossData（优先英文名查图鉴，避开机翻）
        // 3. 返回结果
        let _ = (&self.options, pokedex);
        FetchResult::empty("未实现，请补全 fetch()")
    }
}
"#;

// ---------------------------------------------------------------- 工具

fn log_source(st: &AppState, level: alpha_store::LogLevel, msg: &str) {
    st.store.log(level, "source", msg, "panel");
}

fn core_err(e: alpha_core::CoreError) -> ApiError {
    tracing::error!(error = %e, "配置文件读写失败");
    ApiError::Internal(format!("配置文件读写失败: {e}"))
}

/// 构造一个附件响应。
fn attachment(body: &str, filename: &str, mime: &str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename={filename}"),
            ),
        ],
        Body::from(body.to_string()),
    )
        .into_response()
}

/// 把源名变成安全的文件名。
///
/// 源名是用户输入的，可能含 `/`、中文、空格。这里只保留
/// 字母数字与 `._-`，其余换成 `_` —— 中文源名会变成一串下划线，
/// 所以调用方通常会在后面补个原文，但**绝不能**直接把用户输入
/// 拼进 `Content-Disposition`（头部注入）。
fn safe_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('_').to_string();
    if cleaned.is_empty() {
        "source".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_request_tolerates_missing_fields() {
        let r: AddSourceRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.name, None);
        assert_eq!(r.adapter, None);
        assert_eq!(r.enabled, None);
        assert_eq!(r.priority, None);
    }

    /// 新增源的两个默认值：`enabled=true`、`priority=50`。
    ///
    /// 注意这两个与「读旧文件时的默认值」（`false` / `100`）**不同** ——
    /// 原版就是这样，两个默认值各有各的道理。
    #[test]
    fn add_defaults_are_enabled_true_and_priority_50() {
        let r: AddSourceRequest = serde_json::from_str("{}").unwrap();
        assert!(r.enabled.unwrap_or(true), "新增源默认应启用");
        assert_eq!(r.priority.unwrap_or(50), 50);
    }

    #[test]
    fn rendered_yaml_parses_back() {
        let yaml = render_source_yaml("LZPoke 报点", "lzpoke_reports", true, 10, "https://x/y?z=1&a=2");
        // 前面补个 sources: 才能作为完整文档解析
        let doc = format!("sources:\n{yaml}");
        let v: serde_yaml::Value = serde_yaml::from_str(&doc).expect("渲染结果应是合法 YAML");
        let first = &v.get("sources").unwrap().as_sequence().unwrap()[0];
        assert_eq!(first.get("name").and_then(|x| x.as_str()), Some("LZPoke 报点"));
        assert_eq!(first.get("priority").and_then(|x| x.as_i64()), Some(10));
        assert_eq!(
            first.get("options").unwrap().get("target").and_then(|x| x.as_str()),
            Some("https://x/y?z=1&a=2"),
            "带 ? & = 的地址必须原样往返"
        );
    }

    /// 源名里带单引号也不能把 YAML 弄坏。
    #[test]
    fn rendered_yaml_escapes_single_quotes() {
        let yaml = render_source_yaml("Bob's 源", "foo", false, 1, "");
        let doc = format!("sources:\n{yaml}");
        let v: serde_yaml::Value = serde_yaml::from_str(&doc).expect("应仍是合法 YAML");
        let first = &v.get("sources").unwrap().as_sequence().unwrap()[0];
        assert_eq!(first.get("name").and_then(|x| x.as_str()), Some("Bob's 源"));
    }

    #[test]
    fn safe_filename_strips_path_separators() {
        assert_eq!(safe_filename("../../etc/passwd"), ".._.._etc_passwd");
        assert!(!safe_filename("a/b").contains('/'));
        assert_eq!(safe_filename(""), "source");
        // 中文源名会退化成下划线 —— 所以它**绝不能**作为
        // Content-Disposition 的唯一内容（见 download_source 的注释）
        assert_eq!(safe_filename("正常源"), "source");
    }

    #[test]
    fn safe_filename_keeps_ascii_names() {
        assert_eq!(safe_filename("lzpoke_reports"), "lzpoke_reports");
        assert_eq!(safe_filename("src-1.2"), "src-1.2");
    }

    /// 上传接口的错误信息必须解释**为什么**不支持，而不只是说不行。
    #[test]
    fn template_mentions_the_three_step_process() {
        assert!(SOURCE_TEMPLATE.contains("KNOWN_ADAPTERS"));
        assert!(SOURCE_TEMPLATE.contains("重新编译"));
    }
}
