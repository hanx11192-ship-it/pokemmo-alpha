//! 决策器与评估器的 17 个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_dispatchers*` / `api_evaluators*`。
//! 两张表的**结构完全相同**（`dispatchers` / `evaluators` 各 9 列同名），
//! 原版也是同一套 handler 写两遍。这里抽成共享实现，只在必要处分支 ——
//! 两边的差异一共就四处，全部标注在下面。
//!
//! | 方法 | 路径 | 对应原版函数 |
//! |---|---|---|
//! | GET | `/api/dispatchers` | `api_dispatchers` |
//! | POST | `/api/dispatchers/<id>/enable` | `api_disp_enable` |
//! | POST | `/api/dispatchers/<id>/activate` | `api_disp_activate` |
//! | POST | `/api/dispatchers/<id>/edit` | `api_disp_edit` |
//! | POST | `/api/dispatchers/<id>/delete` | `api_disp_delete` |
//! | POST | `/api/dispatchers/upload` | `api_disp_upload` |
//! | GET | `/api/dispatchers/<id>/download` | `api_disp_download` |
//! | GET | `/api/dispatchers/template` | `api_disp_template` |
//! | ... | 同上 8 条，`evaluators` | `api_eval_*` |
//! | POST | `/api/evaluators/preview` | `api_eval_preview` |
//!
//! # 四处差异（原版就是这样，不是笔误）
//!
//! 1. **`enable` 的日志**：决策器**不写**日志，评估器写
//!    `启用评估器 #3` / `停用评估器 #3`。
//! 2. **`activate` 的日志**：决策器不写，评估器写 `切换当前评估器 #3`。
//! 3. **`upload` 的 `desc` 兜底**：决策器是 `"用户上传的分发器"`，
//!    评估器是 `"用户上传的评估器"`。
//! 4. **`edit` 的 404 文案**：两边都是 `不存在`；但**上传的前缀**不同 ——
//!    决策器写 `user_<base>.py`，评估器也是 —— 一样。
//!
//! （第 4 条核对后确实一致，保留说明是因为这类「以为有差异但其实没有」
//! 的假设最容易在下一轮改动时变成 bug。）
//!
//! # 「首次启用时自动激活」这个怪癖
//!
//! 原版 `enable` 里有一段：
//!
//! ```python
//! if enabled:
//!     active_cnt = SELECT COUNT(*) FROM dispatchers WHERE active=1
//!     if active_cnt == 0:
//!         UPDATE dispatchers SET active=0
//!         UPDATE dispatchers SET active=1 WHERE id=?
//! ```
//!
//! 即：**打开一个开关，如果当前没有任何激活项，就顺手把它设为激活**。
//! 这看着像多余的判断（`active_cnt == 0` 时，那个 `SET active=0`
//! 本来就是空操作），但它是**面板的实际使用方式** ——
//! 用户「启用一个新插件」的意图通常就是「开始用它」。
//!
//! 本版保留，因为去掉它会让用户启用后仍然没有激活项、报表生成直接失败。

use axum::body::Body;
use axum::extract::{Multipart, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use alpha_plugin::{PluginKind, PluginRegistry};
use alpha_store::{LogLevel, NewPlugin, PluginRow, PluginTable};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// 插件类别 —— 决定「决策器侧」还是「评估器侧」的行为差异。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Dispatcher,
    Evaluator,
}

impl Side {
    fn table(&self) -> PluginTable {
        match self {
            Self::Dispatcher => PluginTable::Dispatchers,
            Self::Evaluator => PluginTable::Evaluators,
        }
    }

    fn kind(&self) -> PluginKind {
        match self {
            Self::Dispatcher => PluginKind::Dispatcher,
            Self::Evaluator => PluginKind::Evaluator,
        }
    }

    /// 日志里的 `kind` 字段。
    fn log_kind(&self) -> &'static str {
        match self {
            Self::Dispatcher => "dispatcher",
            Self::Evaluator => "evaluator",
        }
    }

    /// 面向用户的名字（错误信息里用）。
    fn label(&self) -> &'static str {
        match self {
            Self::Dispatcher => "分发器",
            Self::Evaluator => "评估器",
        }
    }

    /// 上传后的数据库名字兜底。
    fn upload_desc(&self) -> &'static str {
        match self {
            Self::Dispatcher => "用户上传的分发器",
            Self::Evaluator => "用户上传的评估器",
        }
    }

    /// 上传文件的固定前缀。
    ///
    /// 原版是 `user_<base>.py`。本版扩展名换成 `.rhai`（见
    /// `alpha_plugin::registry::UPLOAD_EXTENSIONS`），前缀保持不变 ——
    /// 这样运维一眼能分出「这是面板传上去的」和「内建的」。
    fn upload_prefix(&self) -> &'static str {
        "user_"
    }

    /// 入口函数名（`dispatch` / `evaluate`）。
    fn entry(&self) -> &'static str {
        self.kind().entry()
    }
}

// ---------------------------------------------------------------- 列表

/// `GET /api/dispatchers` 或 `GET /api/evaluators`
///
/// 返回数据库里的行，**不**包含磁盘扫描结果 —— 与原版一致。
/// 磁盘上有但库里没有的文件（用户手工丢进去的）不会出现在列表里，
/// 这一点的处理见 [`reconcile`]。
pub async fn list(state: AppState, side: Side) -> ApiResult<Json<Value>> {
    let rows = state.store.list_dispatchers_or(side.table())?;
    let arr: Vec<Value> = rows.iter().map(row_to_json).collect();
    Ok(Json(Value::Array(arr)))
}

/// 把数据库行转成前端吃的 JSON。
///
/// 原版是 `[dict(r) for r in rows]`，列名即键名。
fn row_to_json(r: &PluginRow) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "filename": r.filename,
        "enabled": r.enabled,
        "active": r.active,
        "priority": r.priority,
        "description": r.description,
        "is_builtin": r.is_builtin,
    })
}

/// 磁盘上有、库里没有的插件文件。
///
/// # 这是 Rust 版新增的一处能力
///
/// 原版没有这个功能，所以「手工把一个 `.py` 放进 `panel/evaluators/`」
/// 之后，面板上看不到它，也没有任何提示 —— 用户只能自己去猜
/// 为什么文件放对了却不生效。
///
/// 本版把它列出来（带一个 `unregistered: true` 标记），
/// 前端可以提示「发现未登记的脚本，点此登记」。
pub fn reconcile(state: &AppState, side: Side) -> Vec<Value> {
    let Some(reg) = state.registry.as_ref() else {
        return Vec::new();
    };
    let known: Vec<String> = match state.store.list_dispatchers_or(side.table()) {
        Ok(rows) => rows.into_iter().map(|r| r.filename).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "读取插件列表失败，跳过对账");
            return Vec::new();
        }
    };

    let Ok(files) = reg.scan(side.kind()) else {
        return Vec::new();
    };

    files
        .into_iter()
        .filter(|f| !known.contains(&f.filename))
        .map(|f| {
            // 顺便报告能不能编译 —— 用户最想知道的就是这个
            let status = match reg.load_one(f.clone()) {
                Ok(loaded) => match loaded.get() {
                    Ok(p) => json!({
                        "ok": true,
                        "name": f.display_name(p),
                        "description": p.manifest().description,
                    }),
                    Err(e) => json!({ "ok": false, "error": e.to_string() }),
                },
                Err(e) => json!({ "ok": false, "error": e.to_string() }),
            };
            json!({
                "filename": f.filename,
                "size": f.size,
                "unregistered": true,
                "status": status,
            })
        })
        .collect()
}

// ---------------------------------------------------------------- 启用 / 激活

#[derive(Debug, Deserialize)]
pub struct EnableRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/{dispatchers,evaluators}/<id>/enable`
///
/// 见模块头部关于「首次启用时自动激活」的说明。
pub async fn set_enabled(
    st: AppState,
    side: Side,
    id: i64,
    body: EnableRequest,
) -> ApiResult<Json<Value>> {
    let enabled = body.enabled.unwrap_or(true);
    let table = side.table();

    // 先做开关本身
    st.store
        .update_plugin(table, id, None, None, None, Some(enabled))?;

    if enabled {
        // 「没有任何激活项」时顺手激活它 —— 见模块头部说明。
        // `active_plugin` 走的是 `WHERE active=1 ORDER BY priority, id LIMIT 1`，
        // 与 `COUNT(*) WHERE active=1` 在「有没有」这件事上等价。
        let has_active = st.store.active_plugin(table)?.is_some();
        if !has_active {
            st.store.activate_plugin(table, id)?;
        }
    }

    // 差异 1：只有评估器写日志（原版如此）
    if side == Side::Evaluator {
        st.store.log(
            LogLevel::Info,
            side.log_kind(),
            &format!(
                "{}评估器 #{id}",
                if enabled { "启用" } else { "停用" }
            ),
            "panel",
        );
    }

    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/{dispatchers,evaluators}/<id>/activate`
///
/// `activate_plugin` 在事务里「先全清 active，再置一」，
/// 保证「同一个表里永远只有一个激活项」这个不变量 ——
/// 调度器依赖它（`active_plugin` 取的是 `LIMIT 1`）。
pub async fn activate(
    st: AppState,
    side: Side,
    id: i64,
) -> ApiResult<Json<Value>> {
    if !st.store.activate_plugin(side.table(), id)? {
        return Err(ApiError::NotFound("不存在".into()));
    }

    // 差异 2：只有评估器写日志（原版如此）
    if side == Side::Evaluator {
        st.store.log(
            LogLevel::Info,
            side.log_kind(),
            &format!("切换当前评估器 #{id}"),
            "panel",
        );
    }

    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- 编辑 / 删除

#[derive(Debug, Deserialize)]
pub struct EditRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub description: Option<Option<String>>,
}

/// `POST /api/{dispatchers,evaluators}/<id>/edit`
///
/// # `description` 的 `Option<Option<String>>`
///
/// 原版是：
///
/// ```python
/// desc = body.get("description")      # 可能是 None
/// desc = row["description"] if desc is None else str(desc)
/// ```
///
/// 即「没提交 → 保持原值；提交了 `null` → 也保持原值；提交了空串 → 清空」。
/// 这个三态用 `Option<Option<String>>` 表达：
/// - `None`（字段缺失 **或** 显式 `null` —— serde 不区分这两者）→ 不改
/// - `Some(Some(""))` → 清空
/// - `Some(Some(s))` → 设为 `s`
///
/// 原版里这两种「不改」的输入本来就等价（`dict.get` 对缺失键与
/// 值为 `None` 的键返回同一个东西），所以丢失这份区分度不影响行为。
/// 写错成 `Option<String>` 的话，「只改优先级」的请求会把描述清掉。
pub async fn edit(
    st: AppState,
    side: Side,
    id: i64,
    body: EditRequest,
) -> ApiResult<Json<Value>> {
    let table = side.table();
    let row = st
        .store
        .get_plugin(table, id)?
        .ok_or_else(|| ApiError::NotFound("不存在".into()))?;

    let name = match body.name.as_deref() {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => row.name.clone(),
    };

    // 同名检查（排除自己）—— 数据库有 UNIQUE 约束，但在这里给出
    // 与原版一致的 400 文案比让 SQLite 报 500 友好得多
    if name != row.name {
        let dup = st
            .store
            .list_dispatchers_or(table)?
            .into_iter()
            .any(|r| r.name == name && r.id != id);
        if dup {
            return Err(ApiError::BadRequest(format!("已存在同名{}", side.label())));
        }
    }

    let priority = body.priority.unwrap_or(row.priority);
    let desc = match &body.description {
        // 没提交 / 显式 null → 保持原值
        None | Some(None) => row.description.clone(),
        Some(Some(d)) => d.clone(),
    };

    st.store
        .update_plugin(table, id, Some(&name), Some(&desc), Some(priority), None)?;

    st.store.log(
        LogLevel::Info,
        side.log_kind(),
        &format!("编辑{}：{} -> {name}", side.label(), row.name),
        "panel",
    );

    Ok(Json(json!({ "ok": true, "name": name })))
}

/// `POST /api/{dispatchers,evaluators}/<id>/delete`
///
/// 两处守卫：
/// 1. **内建不可删**（`is_builtin=1` → 400）
/// 2. 删除时**同时删掉磁盘文件**（原版如此 —— 否则文件会一直留着，
///    下次有人在磁盘上看到它、手工加回数据库，幽灵插件就复活了）
pub async fn delete(
    st: AppState,
    side: Side,
    id: i64,
) -> ApiResult<Json<Value>> {
    let table = side.table();
    let row = st
        .store
        .get_plugin(table, id)?
        .ok_or_else(|| ApiError::NotFound("不存在".into()))?;

    if row.is_builtin {
        return Err(ApiError::BadRequest(format!(
            "内置{}不可删除",
            side.label()
        )));
    }

    // 先删文件再删库：反过来的话，删库成功但删文件失败会留下孤儿文件；
    // 而这个顺序即使删文件失败也只是留下一个孤儿（可被 reconcile 发现）。
    let path = st
        .registry
        .as_ref()
        .map(|r| r.path_of(side.kind(), &row.filename));
    if let Some(path) = path {
        if path.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), error = %e, "删除插件文件失败");
            }
        }
    }
    if let Some(reg) = st.registry.as_ref() {
        reg.invalidate();
    }

    st.store.delete_plugin(table, id)?;

    st.store.log(
        LogLevel::Info,
        side.log_kind(),
        &format!("删除{}：{}", side.label(), row.name),
        "panel",
    );

    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- 上传

/// `POST /api/{dispatchers,evaluators}/upload`
///
/// # 与原版的差异（有意）
///
/// | 项 | 原版 | 本版 |
/// |---|---|---|
/// | 扩展名 | 任意（`.py` 即可） | 只认 `.rhai` |
/// | 内容检查 | `"def dispatch" in raw` 文本搜索 | 真的编译一遍 |
/// | 写入时机 | 先写盘，`load_module` 失败再删 | 先编译，过了才写盘 |
/// | 执行风险 | `exec_module()` → RCE | Rhai 沙箱，无 IO |
///
/// 「先编译再写盘」这个顺序值得说一下：原版是先把用户的文件写下去、
/// 再尝试 import，失败就删 —— 中间那一小段时间里，磁盘上躺着一个
/// **没人校验过**的文件。如果服务在这个窗口里崩了，文件就永久留下了。
/// 本版反过来：编译通过才落盘，失败根本不碰文件系统。
pub async fn upload(
    st: AppState,
    side: Side,
    multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let (filename_in, name_in, source) = read_upload(multipart).await?;

    // 1. 定文件名。前缀 `user_` + 白名单扩展名。
    let base = sanitize_base(&filename_in, name_in.as_deref(), side);
    let filename = format!("{}{}.rhai", side.upload_prefix(), base);

    // 2. 校验（挡住 `..`、路径分隔符、非法字符、超长）
    let filename = PluginRegistry::validate_filename(&filename)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    // 3. **先编译**。这一步同时验了「语法正确」与「实现了入口函数」。
    let plugin = alpha_plugin::Plugin::compile(&source, side.kind(), None).map_err(|e| {
        ApiError::BadRequest(format!(
            "加载失败：{e}\n\
             \n\
             脚本必须实现 `fn {}(boss, ctx)` —— 见「下载模板」。",
            side.entry()
        ))
    })?;

    // 4. 重名检查（在写盘之前）
    let reg = registry(&st)?;
    let path = reg.path_of(side.kind(), &filename);
    if path.exists() {
        return Err(ApiError::BadRequest(format!(
            "已存在同名{}文件: {filename}",
            side.label()
        )));
    }

    // 5. 落盘
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApiError::Internal(format!("创建插件目录失败: {e}")))?;
    }
    std::fs::write(&path, &source)
        .map_err(|e| ApiError::Internal(format!("写入插件文件失败: {e}")))?;
    reg.invalidate();

    // 6. 登记到数据库
    //
    // 描述优先级与原版一致：脚本里的 DESCRIPTION > NAME > 兜底文案。
    // `Manifest::parse` 已经把「头部注释」与「const」两种来源都处理了，
    // 这里只负责在两者都空时用兜底。
    let display = name_in
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            let n = plugin.manifest().name.trim();
            if n.is_empty() {
                base.clone()
            } else {
                n.to_string()
            }
        });
    let desc = {
        let d = plugin.manifest().description.trim();
        if d.is_empty() {
            side.upload_desc().to_string()
        } else {
            d.to_string()
        }
    };

    // priority 用「当前最大值 + 1」—— 原版如此。
    // 含义是「新传的排在最后」，不动用户已有的排序。
    let maxp = st
        .store
        .list_dispatchers_or(side.table())?
        .iter()
        .map(|r| r.priority)
        .max()
        .unwrap_or(10);

    let id = st.store.add_plugin(
        side.table(),
        &NewPlugin {
            name: display.clone(),
            filename: filename.clone(),
            enabled: true,
            // 新上传的**不自动激活** —— 原版也是 0。
            // 激活是明确的用户动作（或首次启用时的自动激活逻辑）。
            active: false,
            priority: maxp + 1,
            description: desc,
            is_builtin: false,
        },
    )?;

    st.store.log(
        LogLevel::Info,
        side.log_kind(),
        &format!("上传{}：{display}", side.label()),
        "panel",
    );

    // 原版只回 `{ok: true}`，前端上传完要靠「重新拉一次列表」才知道新项的 id。
    // 这里多回 `id` / `name` / `filename` / `enabled` / `active` / `priority`，
    // 前端可以就地插进列表、立刻可选，少一次往返。
    Ok(Json(json!({
        "ok": true,
        "id": id,
        "filename": filename,
        "name": display,
        "priority": maxp + 1,
        "enabled": true,
        "active": false,
    })))
}

/// 从 multipart 里读 `file` 与 `name`。
async fn read_upload(mut multipart: Multipart) -> ApiResult<(String, Option<String>, String)> {
    let mut filename = String::new();
    let mut name: Option<String> = None;
    let mut source = String::new();
    let mut got = false;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("读取上传内容失败: {e}")))?
    {
        match field.name() {
            Some("file") => {
                got = true;
                filename = field.file_name().unwrap_or("").to_string();
                source = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("读取文件内容失败: {e}")))?;
            }
            Some("name") => {
                name = Some(
                    field
                        .text()
                        .await
                        .map_err(|e| ApiError::BadRequest(format!("读取表单字段失败: {e}")))?,
                );
            }
            // 必须读掉，否则 multipart 流会卡住
            _ => {
                let _ = field.text().await;
            }
        }
    }

    if !got {
        return Err(ApiError::BadRequest("未收到文件".into()));
    }
    Ok((filename, name, source))
}

/// 从上传文件名 / 表单名里定出一个安全的基名。
///
/// 原版：
///
/// ```python
/// base = secure_filename((name or f.filename or "dispatcher")).rsplit(".", 1)[0]
/// base = "".join(ch for ch in base if ch.isalnum() or ch in "_-")
/// if not base:
///     base = "dispatcher"
/// ```
///
/// # 一个原版的坑（本版踩不着）
///
/// 原版用的是 `str.isalnum()`，它对**中日韩字符返回 True** ——
/// 所以中文名的插件会生成一个中文文件名。在 Python 里没问题，
/// 但它会让 `secure_filename` 之前的判断失效。
/// 本版限定 ASCII 字母数字，中文名会退化成兜底名。
///
/// 这样更稳：插件文件名要出现在 URL 路径（`/download` 那个接口）
/// 与 `Content-Disposition` 头里，非 ASCII 会带来一堆编码问题。
fn sanitize_base(filename: &str, name: Option<&str>, side: Side) -> String {
    let raw = name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(Some(filename))
        .unwrap_or("");

    // 去掉扩展名
    let stem = raw.rsplit_once('.').map(|(s, _)| s).unwrap_or(raw);

    let cleaned: String = stem
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .collect();

    if cleaned.is_empty() {
        side.label().to_string() // 「分发器」/「评估器」—— 都是合法文件名
    } else {
        cleaned
    }
}

// ---------------------------------------------------------------- 下载 / 模板

/// `GET /api/{dispatchers,evaluators}/<id>/download`
///
/// 返回脚本源码。本版返回的是 **Rhai 源码**（原版返回 `.py`），
/// 文件名也与库里登记的 `filename` 一致。
pub async fn download(
    st: AppState,
    side: Side,
    id: i64,
) -> ApiResult<Response> {
    let row = st
        .store
        .get_plugin(side.table(), id)?
        .ok_or_else(|| ApiError::NotFound("不存在".into()))?;

    let reg = registry(&st)?;
    let path = reg.path_of(side.kind(), &row.filename);
    let text = std::fs::read_to_string(&path)
        .map_err(|_| ApiError::NotFound(format!("插件文件不存在: {}", row.filename)))?;

    Ok(attachment(
        &text,
        &row.filename,
        "text/plain; charset=utf-8",
    ))
}

/// `GET /api/{dispatchers,evaluators}/template`
pub async fn template(side: Side) -> Response {
    let (body, name) = match side {
        Side::Dispatcher => (DISPATCHER_TEMPLATE, "dispatcher_template.rhai"),
        Side::Evaluator => (EVALUATOR_TEMPLATE, "evaluator_template.rhai"),
    };
    attachment(body, name, "text/plain; charset=utf-8")
}

/// 决策器模板（Rhai）。
pub const DISPATCHER_TEMPLATE: &str = r#"// @name 我的决策器
// @description 一句话描述这个决策器的打法思路
//
// 决策器的契约：
//
//   fn dispatch(boss, ctx) -> String
//
//     boss : 归一化后的头目（见下面的字段说明）
//     ctx  : #{ lang: "zh"|"en", langs: [...], pokemon_name, ability_name,
//               move_names, ... }，另有可从脚本直接调的函数
//                resolve_move_id(name) / resolve_ability_id(name) /
//                resolve_pokemon_id(name) / engine_report(lang)
//     返回 : 一段可直接推送 / 展示的文本
//
// boss 的字段：
//   name, ability, moves[], move_ids[], location, location_en, period,
//   gender, egg_groups[], egg_groups_en[], pokedex_id, extra_lines[],
//   source, reported_at, reporter
//
// 想用内置打法引擎：调 engine_report(lang) 即可拿到官方报文，
// 再自己做二次加工。

fn dispatch(boss, ctx) {
    // 双语：两份报文用分隔线拼起来
    if ctx.langs.len() > 1 {
        let parts = [];
        for lang in ctx.langs {
            parts.push(engine_report(lang));
        }
        return parts.join("\n\n────────\n\n");
    }
    // 单语言：直接用内置引擎
    engine_report(ctx.lang)
}
"#;

/// 评估器模板（Rhai）。
///
/// 比决策器多讲两件事：返回 `()` 的语义，以及**评估器不干预分发**。
pub const EVALUATOR_TEMPLATE: &str = r#"// @name 我的评估器
// @description 一句话描述这个评估器的评估思路
//
// 评估器的契约：
//
//   fn evaluate(boss, ctx) -> map | ()
//
//     boss : 同决策器
//     ctx  : #{ lang: "zh"|"en", langs: [...] }
//     返回 : ()                          —— 这只头目不出评估行（报文照常推送）
//            #{ score: 3,                —— 分值 / 系数（含义由你自己定）
//               label: "白给头",          —— 评价词
//               detail: "无可评估威胁",    —— 一句话说明（可选）
//               factors: ["无高威胁技能"], —— 加分项明细（可选，供日志排查）
//               line: "好打 难度系数3" }   —— 【可选】直接指定报文那一行的完整文本
//
// 职责边界：评估器**只负责评估**，把结果加到报文上；不干预分发
// （是否推送、发到哪个渠道由调度器与分发渠道决定）。
//
// 同一时刻只有一个评估器处于激活状态 —— 一套队伍配一个，
// 换队伍就切换评估器。返回 () 是「这只不评估」，**不是**「交给下一个」。

fn evaluate(boss, ctx) {
    let score = 0;

    // 示例：带高威胁技能就加分
    for mv in boss.moves {
        if mv == "挑衅" {
            score += 1;
        }
    }

    if score == 0 {
        return #{ score: 0, label: "白给头", detail: "无可评估威胁" };
    }
    if score <= 3 {
        return #{ score: score, label: "简单头", detail: "" };
    }
    if score <= 6 {
        return #{ score: score, label: "看脸头", detail: "" };
    }
    // 想自定义措辞就带上 line
    return #{ score: score, label: "不做评价", line: `硬骨头 难度系数${score}` };
}
"#;

// ---------------------------------------------------------------- 预览

/// `POST /api/evaluators/preview` 的请求体。
#[derive(Debug, Deserialize)]
pub struct PreviewRequest {
    #[serde(default)]
    pub evaluator_id: Option<i64>,
    #[serde(default)]
    pub pokemon: Option<String>,
    #[serde(default)]
    pub ability: Option<String>,
    #[serde(default)]
    pub moves: Option<Vec<String>>,
    #[serde(default)]
    pub lang: Option<String>,
}

/// `POST /api/evaluators/preview`
///
/// 用给定的头目试跑一个评估器，返回评分明细 —— 供调试页与评估器页预览。
///
/// # 选择评估器的顺序（原版）
///
/// 1. 请求里 `evaluator_id` 且该评估器 `enabled=1`
/// 2. 否则回落当前 `active=1` 的那个
/// 3. 都没有 → 500 `没有可用的评估器`
///
/// **注意第 1 步要求 `enabled=1`**：显式指定一个已停用的评估器会
/// 静默回落到激活项，而不是报错。这有点反直觉，但它是原版行为，
/// 且实际上有用 —— 前端「预览」按钮传的是列表里那一行的 id，
/// 用户停用后按钮还在，此时给激活项的结果比报错更合理。
pub async fn preview(
    State(st): State<AppState>,
    Json(body): Json<PreviewRequest>,
) -> ApiResult<Json<Value>> {
    let row = resolve_preview_evaluator(&st, body.evaluator_id)?;

    let pk = alpha_core::pokedex::get_pokedex().map_err(core_err)?;

    // 解析输入（与原版 `api_eval_preview` 逐字对齐）
    let pokemon_in = body.pokemon.as_deref().unwrap_or("");
    let ability_in = body.ability.as_deref().unwrap_or("");
    let moves_in: Vec<String> = body
        .moves
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();

    let name_zh = pk.canonical_pokemon(pokemon_in);
    let pid = pk.resolve_pokemon_id(pokemon_in);
    let (ability_zh, aid) = if ability_in.is_empty() {
        ("无特性".to_string(), None)
    } else {
        (pk.canonical_ability(ability_in), pk.resolve_ability_id(ability_in))
    };
    let moves_zh: Vec<String> = moves_in.iter().map(|m| pk.canonical_move(m)).collect();
    let move_ids: Vec<Option<i64>> = moves_in.iter().map(|m| pk.resolve_move_id(m)).collect();

    // 性别按**图鉴**填（数据源推送时也是这么填的）—— 评估器现在对
    // 单性别/无性别头目不评估，试评分理应如实反映这一点：
    // 输入百变怪/自爆磁怪就该看到「不评估」，而不是假装它是各半性别。
    // 图鉴里查不到的精灵（用户乱输的）→ 双性别占位，保持「空壳也能
    // 测脚本逻辑」的用途。
    let gender = pid
        .and_then(|p| pk.pokemon.get(&p.to_string()))
        .map(|e| alpha_core::models::Gender::from_male_percent(e.gender_rate))
        .unwrap_or_else(|| alpha_core::models::Gender::from_male_percent(Some(50.0)));

    let boss = alpha_core::models::BossData {
        name: name_zh.clone(),
        ability: ability_zh.clone(),
        moves: moves_zh.clone(),
        pokedex_id: pid,
        ability_id: aid,
        move_ids,
        gender,
        source: "preview".to_string(),
        ..Default::default()
    };

    let lang = body.lang.as_deref().unwrap_or("zh").to_string();

    // 跑评估器。失败是**用户脚本的问题** → 500 + 原始错误，
    // 与原版的 `评估器执行失败: {e}` 文案一致。
    let plugin = load_plugin(&st, Side::Evaluator, &row.filename)?;
    let view = alpha_plugin::BossView::from_boss(&boss);
    let ctx = alpha_plugin::Context::new(lang.clone(), vec![lang.clone()]);

    let result = plugin
        .evaluate(&view, &ctx)
        .map_err(|e| ApiError::Internal(format!("评估器执行失败: {e}")))?;

    let line = result
        .as_ref()
        .map(|r| r.format_line(&lang))
        .unwrap_or_default();

    Ok(Json(json!({
        "ok": true,
        "evaluator": row.name,
        "result": result,
        "line": line,
    })))
}

/// 按原版顺序挑出预览用的评估器。
fn resolve_preview_evaluator(
    st: &AppState,
    wanted: Option<i64>,
) -> ApiResult<PluginRow> {
    if let Some(id) = wanted {
        if let Some(row) = st.store.get_plugin(PluginTable::Evaluators, id)? {
            if row.enabled {
                return Ok(row);
            }
        }
    }
    st.store
        .active_plugin(PluginTable::Evaluators)?
        .ok_or_else(|| ApiError::Internal("没有可用的评估器".into()))
}

// ---------------------------------------------------------------- 工具

/// 取插件注册表。
fn registry(st: &AppState) -> ApiResult<&std::sync::Arc<PluginRegistry>> {
    st.registry
        .as_ref()
        .ok_or_else(|| ApiError::Internal("插件系统未初始化".into()))
}

/// 按文件名加载并编译一个插件。
///
/// # 每次调用都重新编译
///
/// 因为面板的特点就是「刚上传就想试」。走注册表的缓存会让
/// 「改完文件立刻预览」看到旧结果 —— 那会让用户以为改动没生效。
/// 编译一个几百行的脚本在毫秒级，预览不是热路径。
pub fn load_plugin(
    st: &AppState,
    side: Side,
    filename: &str,
) -> ApiResult<std::sync::Arc<alpha_plugin::Plugin>> {
    let reg = registry(st)?;
    let path = reg.path_of(side.kind(), filename);
    let plugin = alpha_plugin::Plugin::load_file(&path, side.kind()).map_err(|e| {
        ApiError::Internal(format!("{}不可用: {filename} - {e}", side.label()))
    })?;
    Ok(std::sync::Arc::new(plugin))
}

fn core_err(e: alpha_core::CoreError) -> ApiError {
    tracing::error!(error = %e, "加载图鉴失败");
    ApiError::Internal(format!("加载图鉴失败: {e}"))
}

fn attachment(body: &str, filename: &str, mime: &str) -> Response {
    // `filename` 已经过校验（只含 ASCII 字母数字与 `._-`），
    // 不会造成头部注入
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Side 的四处差异 ----

    #[test]
    fn log_kinds_match_the_original() {
        assert_eq!(Side::Dispatcher.log_kind(), "dispatcher");
        assert_eq!(Side::Evaluator.log_kind(), "evaluator");
    }

    #[test]
    fn upload_descriptions_match_the_original() {
        assert_eq!(Side::Dispatcher.upload_desc(), "用户上传的分发器");
        assert_eq!(Side::Evaluator.upload_desc(), "用户上传的评估器");
    }

    #[test]
    fn labels_are_the_chinese_nouns() {
        assert_eq!(Side::Dispatcher.label(), "分发器");
        assert_eq!(Side::Evaluator.label(), "评估器");
    }

    #[test]
    fn entries_are_the_contract_function_names() {
        assert_eq!(Side::Dispatcher.entry(), "dispatch");
        assert_eq!(Side::Evaluator.entry(), "evaluate");
    }

    #[test]
    fn tables_are_distinct() {
        assert_ne!(Side::Dispatcher.table(), Side::Evaluator.table());
    }

    // ---- 文件名消毒 ----

    #[test]
    fn sanitize_keeps_ascii_alnum_and_dash_underscore() {
        assert_eq!(
            sanitize_base("my_disp-v2.rhai", None, Side::Dispatcher),
            "my_disp-v2"
        );
    }

    /// 上传名里的路径分隔符必须被清掉 —— 否则 `..%2f..%2fpanel.db` 之类
    /// 能指到目录外。
    #[test]
    fn sanitize_strips_path_separators_and_dots() {
        let b = sanitize_base("../../../panel.db", None, Side::Dispatcher);
        assert!(!b.contains('/'), "得到 {b}");
        assert!(!b.contains(".."), "得到 {b}");
        assert!(!b.contains('.'), "得到 {b}");
    }

    /// 中文名会退化成兜底名（原版会用 `isalnum()` 保留中文，
    /// 本版限定 ASCII —— 见 `sanitize_base` 的说明）。
    #[test]
    fn sanitize_falls_back_for_non_ascii_names() {
        assert_eq!(sanitize_base("我的分发器.rhai", None, Side::Dispatcher), "分发器");
        assert_eq!(sanitize_base("", None, Side::Evaluator), "评估器");
    }

    #[test]
    fn sanitize_prefers_form_name_over_filename() {
        assert_eq!(
            sanitize_base("upload123.rhai", Some("我起的名字"), Side::Dispatcher),
            "分发器",
            "表单名优先，但中文退化成兜底"
        );
        assert_eq!(
            sanitize_base("upload123.rhai", Some("good_name"), Side::Dispatcher),
            "good_name"
        );
    }

    #[test]
    fn sanitize_ignores_empty_form_name() {
        // 表单里填了空串 → 回落到文件名
        assert_eq!(
            sanitize_base("abc.rhai", Some("   "), Side::Evaluator),
            "abc"
        );
    }

    /// 消毒后的基名 + `user_` 前缀必须能过注册表的文件名校验。
    #[test]
    fn sanitized_names_pass_registry_validation() {
        for raw in [
            "../../etc/passwd",
            "a/b/c",
            "..",
            ".hidden",
            "",
            "正常名字",
            "with space.rhai",
        ] {
            let base = sanitize_base(raw, None, Side::Dispatcher);
            let fname = format!("user_{base}.rhai");
            assert!(
                PluginRegistry::validate_filename(&fname).is_ok(),
                "{raw:?} -> {fname} 没能通过校验"
            );
        }
    }

    // ---- edit 请求的三态 description ----

    #[test]
    fn edit_description_is_three_state() {
        // 字段缺失 → None（不改）
        let r: EditRequest = serde_json::from_str(r#"{"priority":5}"#).unwrap();
        assert_eq!(r.description, None);

        // 显式 null。注意：serde 无法区分「字段缺失」与「显式 null」，
        // 两者都落到 `None`。好在**原版语义下这两者本来就等价** ——
        // `body.get("description")` 拿到 None 时同样是「保持原值」，
        // 所以这个「丢失的区分度」没有任何行为影响。
        let r: EditRequest = serde_json::from_str(r#"{"description":null}"#).unwrap();
        assert_eq!(r.description, None);

        // 空串 → Some(Some(""))（清空）
        let r: EditRequest = serde_json::from_str(r#"{"description":""}"#).unwrap();
        assert_eq!(r.description, Some(Some(String::new())));

        // 正常值
        let r: EditRequest = serde_json::from_str(r#"{"description":"新说明"}"#).unwrap();
        assert_eq!(r.description, Some(Some("新说明".to_string())));
    }

    /// 只改优先级时，描述必须保持原值 —— 这是三态最容易写错的地方。
    #[test]
    fn editing_only_priority_keeps_description() {
        let r: EditRequest = serde_json::from_str(r#"{"priority":9}"#).unwrap();
        // 模拟 handler 里的 match
        let desc = match &r.description {
            None | Some(None) => None, // → 用 row.description
            Some(Some(d)) => Some(d.clone()),
        };
        assert!(desc.is_none(), "未提交 description 时必须走「保持原值」分支");
    }

    #[test]
    fn edit_request_tolerates_empty_body() {
        let r: EditRequest = serde_json::from_str("{}").unwrap();
        assert!(r.name.is_none());
        assert!(r.priority.is_none());
        assert!(r.description.is_none());
    }

    #[test]
    fn enable_request_defaults_to_true() {
        let r: EnableRequest = serde_json::from_str("{}").unwrap();
        assert!(r.enabled.unwrap_or(true));
    }

    #[test]
    fn preview_request_tolerates_missing_fields() {
        let r: PreviewRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(r.evaluator_id, None);
        assert_eq!(r.lang, None);
        assert!(r.moves.is_none());
    }

    // ---- 模板 ----

    /// 两个模板都必须能真的编译通过 —— 否则用户下载下来照着改，
    /// 第一步就撞编译错误。
    #[test]
    fn dispatcher_template_compiles() {
        alpha_plugin::Plugin::compile(DISPATCHER_TEMPLATE, PluginKind::Dispatcher, None)
            .expect("决策器模板必须能编译");
    }

    #[test]
    fn evaluator_template_compiles() {
        alpha_plugin::Plugin::compile(EVALUATOR_TEMPLATE, PluginKind::Evaluator, None)
            .expect("评估器模板必须能编译");
    }

    #[test]
    fn templates_declare_metadata_in_header_comments() {
        for (src, kind) in [
            (DISPATCHER_TEMPLATE, PluginKind::Dispatcher),
            (EVALUATOR_TEMPLATE, PluginKind::Evaluator),
        ] {
            let p = alpha_plugin::Plugin::compile(src, kind, None).unwrap();
            assert!(
                !p.manifest().name.trim().is_empty(),
                "{kind:?} 模板该带上 @name"
            );
            assert!(
                !p.manifest().description.trim().is_empty(),
                "{kind:?} 模板该带上 @description"
            );
        }
    }

    /// 模板里必须写着入口函数名 —— 用户照着改才不会再问「函数叫啥」。
    #[test]
    fn templates_document_their_entry_function() {
        assert!(DISPATCHER_TEMPLATE.contains("fn dispatch(boss, ctx)"));
        assert!(EVALUATOR_TEMPLATE.contains("fn evaluate(boss, ctx)"));
    }

    /// 评估器模板要讲清「返回 ()」与「不干预分发」这两条契约。
    #[test]
    fn evaluator_template_documents_the_empty_return() {
        assert!(EVALUATOR_TEMPLATE.contains("不干预分发"));
        assert!(EVALUATOR_TEMPLATE.contains("不是"));
    }
}
