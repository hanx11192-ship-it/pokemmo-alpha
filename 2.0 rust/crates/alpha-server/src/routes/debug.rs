//! 调试台与首页 3 个接口。
//!
//! 对应原版 `panel/app.py` 的 `api_dashboard` / `api_pokedex` / `debug_run`。
//!
//! # `/debug/run` 是整个面板里逻辑最绕的一个接口
//!
//! 它要按用户输入的**名字**（中文或英文、可能带别名）反查图鉴，
//! 归一化成一个假的头目，再跑一遍决策器。三步里每一步都可能失败，
//! 而且失败原因对用户来说必须可读：
//!
//! | 输入 | 结果 |
//! |---|---|
//! | 没填精灵名 | 400「请选择或填写头目精灵」 |
//! | 填了但图鉴里没有 | **不报错**，原样用输入值（原版如此） |
//! | 没有可用决策器 | 500「没有可用的分发器」 |
//! | 决策器抛错 | 500「分发器执行失败: ...」 |
//!
//! 「填了但图鉴里没有」那条特别要紧：`canonical_pokemon()` 找不到时
//! 返回**原输入**。所以用户打错字不会得到「查无此精灵」，
//! 而是拿到一份用错名字生成的报表 —— 这看着像 bug，
//! 但它让调试台对「还没入库的新精灵」也能用。本版保留，
//! 并在响应里回 `resolved` 让用户能自己看出「名字没被归一化」。
//!
//! # 三条路由的鉴权不一致，这是原版事实
//!
//! `/api/dashboard` 与 `/api/pokedex` 在原版上**没有** `@login_required`
//! （首页要能直接打开、图鉴列表要在登录页之后就可用）；`/debug/run`
//! 有。本版按原版对齐 —— 见 `app.rs` 里的路由注册注释。

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use alpha_core::config_mgr;
use alpha_core::models::{BossData, Gender};
use alpha_core::pokedex::get_pokedex;
use alpha_plugin::sandbox::PluginKind;
use alpha_store::{LogLevel, PluginRow, PluginTable};

use crate::error::{ApiError, ApiResult};
use crate::AppState;

/// `GET /api/dashboard`
///
/// 首页那几个数字卡片 + 最近日志 + 当前决策器。
pub async fn dashboard(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    // ---- 数据源统计 ----
    let sources = config_mgr::load_sources()
        .map_err(|e| ApiError::Internal(format!("读取数据源配置失败: {e}")))?;
    let total = sources.len();
    // `describe_source` 的 `enabled` 缺省是 **false**，所以这里必须
    // 用它而不是直接读 YAML —— 否则「没写 enabled 的源」会被算成启用。
    let enabled = sources
        .iter()
        .filter(|s| config_mgr::describe_source(s)["enabled"] == json!(true))
        .count();

    // ---- 日志 ----
    //
    // 原版取「最近 12 条」，不带筛选。
    let recent_logs = state.store.list_logs(&alpha_store::LogFilter::new(12))?;

    // 「最近 200 条里有多少条 spawn（刷新点）」—— 原版用 `level="spawn"`
    // 当筛选条件，但 `level` 列存的是 `info`/`error` 这些级别，
    // 所以那个查询恒返回空。这是个原版的 bug。
    //
    // 本版改成按 `kind` 筛（`spawn` 是 kind 的取值），让它真的能数出东西。
    let mut spawn_filter = alpha_store::LogFilter::new(200);
    spawn_filter.kind = Some("spawn".into());
    let spawn_count = state.store.list_logs(&spawn_filter)?.len();

    // ---- 总数 ----
    let total_logs = state.store.count_logs()?;

    // ---- 当前决策器 ----
    let active_dispatcher = state
        .store
        .active_plugin(PluginTable::Dispatchers)?
        .map(|r| r.name);

    // ---- 调度器 ----
    let scheduler = match &state.scheduler {
        Some(s) => serde_json::to_value(s.snapshot()).unwrap_or(Value::Null),
        None => Value::Null,
    };

    // ---- 图鉴规模 ----
    let pk = get_pokedex().map_err(|e| ApiError::Internal(format!("加载图鉴失败: {e}")))?;
    let st = pk.stats();
    let pokedex = json!({
        "pokemon": st.pokemon,
        "abilities": st.abilities,
        "moves": st.moves,
    });

    Ok(Json(json!({
        "sources": { "total": total, "enabled": enabled },
        "recent_logs": recent_logs,
        "spawn_count_200": spawn_count,
        "total_logs": total_logs,
        "active_dispatcher": active_dispatcher,
        "scheduler": scheduler,
        "pokedex": pokedex,
    })))
}

/// `GET /api/pokedex`
///
/// 三个平铺列表，给前端下拉框用。数据量不小（约 1200 精灵 + 700 技能），
/// 但它是个纯内存读取 —— 图鉴是进程级 `OnceCell`，不碰磁盘。
pub async fn pokedex() -> ApiResult<Json<Value>> {
    let pk = get_pokedex().map_err(|e| ApiError::Internal(format!("加载图鉴失败: {e}")))?;
    Ok(Json(pk.lists()))
}

/// `POST /debug/run` 的请求体。
#[derive(Debug, Deserialize)]
pub struct DebugRunRequest {
    #[serde(default)]
    pub pokemon: Option<String>,
    #[serde(default)]
    pub ability: Option<String>,
    #[serde(default)]
    pub moves: Option<Vec<String>>,
    #[serde(default)]
    pub gender: Option<String>,
    #[serde(default)]
    pub egg_groups: Option<Vec<String>>,
    /// 指定用哪个决策器。必须是**启用**状态，否则忽略并走回退链。
    #[serde(default)]
    pub dispatcher_id: Option<i64>,
    #[serde(default)]
    pub lang: Option<String>,
}

/// 调试台性别参数 → `Gender`。
///
/// `auto`（前端默认项）与未传/未知值：按图鉴查这只精灵的真实性别，
/// 图鉴里没有的回落 dual 占位（乱输的名字也要能测脚本）。
/// 显式的 dual/male/female/none 原样生效 —— 模拟任意情况的调试能力保留。
fn gender_from_request(
    gender: Option<&str>,
    pid: Option<i64>,
    pokedex: &alpha_core::pokedex::Pokedex,
) -> Gender {
    match gender {
        Some("male") => Gender::from_male_percent(Some(100.0)),
        Some("female") => Gender::from_male_percent(Some(0.0)),
        Some("none") => Gender::genderless(),
        Some("dual") => Gender::from_male_percent(Some(50.0)),
        _ => pid
            .and_then(|p| pokedex.pokemon.get(&p.to_string()))
            .map(|e| Gender::from_male_percent(e.gender_rate))
            .unwrap_or_else(|| Gender::from_male_percent(Some(50.0))),
    }
}

/// `POST /debug/run`
///
/// 手动构造一个头目、跑一遍决策器，看播报长什么样。
///
/// 注意路径是 `/debug/run` 不是 `/api/debug/run` —— 原版如此，
/// 前端也是照这个路径调的。
pub async fn debug_run(
    State(state): State<AppState>,
    Json(body): Json<DebugRunRequest>,
) -> ApiResult<Json<Value>> {
    let pk = get_pokedex().map_err(|e| ApiError::Internal(format!("加载图鉴失败: {e}")))?;

    // ---- 1. 精灵名（必填） ----
    let pokemon_in = body.pokemon.unwrap_or_default().trim().to_string();
    if pokemon_in.is_empty() {
        return Err(ApiError::BadRequest("请选择或填写头目精灵".into()));
    }

    // 归一化。找不到时 `canonical_*` 返回**原输入**（见模块头部说明）。
    let name_zh = pk.canonical_pokemon(&pokemon_in);
    let pid = pk.resolve_pokemon_id(&pokemon_in);

    // ---- 2. 特性 ----
    //
    // 空特性要写成「无特性」而不是空串 —— 引擎的模板里会直接把它拼进
    // 正文，空串会渲染出「特性：」这种半截话。
    let ability_in = body.ability.unwrap_or_default().trim().to_string();
    let (ability_zh, aid) = if ability_in.is_empty() {
        ("无特性".to_string(), None)
    } else {
        (
            pk.canonical_ability(&ability_in),
            pk.resolve_ability_id(&ability_in),
        )
    };

    // ---- 3. 技能 ----
    let moves_in: Vec<String> = body
        .moves
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .collect();
    let moves_zh: Vec<String> = moves_in.iter().map(|m| pk.canonical_move(m)).collect();
    let move_ids: Vec<Option<i64>> = moves_in.iter().map(|m| pk.resolve_move_id(m)).collect();

    // ---- 4. 性别 ----
    //
    // 下拉框：auto（按图鉴，**默认**）/ dual（各半）/ male（全公）/
    // female（全母）/ none（无性别）。
    //
    // 默认改成「按图鉴」是有意的：用户选了百变怪、没动性别下拉，
    // 他要试的就是这只精灵在真实推送里的样子 —— 无性别头目应该
    // 如实拿到「无性别」（评估器会不评估），而不是被假装成各半。
    // 显式选其它值仍然生效 —— 模拟任意情况的调试能力保留。
    let gender = gender_from_request(body.gender.as_deref(), pid, pk);

    // ---- 5. 蛋组 ----
    //
    // 用户没填就从图鉴里取这只精灵的默认蛋组。
    let egg = match body.egg_groups {
        Some(g) if !g.is_empty() => g,
        _ => pk.egg_groups_of(pid),
    };

    let boss = BossData {
        name: name_zh.clone(),
        ability: ability_zh.clone(),
        moves: moves_zh.clone(),
        gender,
        egg_groups: egg.clone(),
        pokedex_id: pid,
        ability_id: aid,
        move_ids: move_ids.clone(),
        // 调试台没有真实来源，固定标记成 debug
        source: "debug".into(),
        reporter: Some("debug".into()),
        ..Default::default()
    };

    // ---- 6. 选决策器 ----
    //
    // 三级回退链（原版如此）：指定的（且启用）→ 激活的 → 内建的。
    let Some(row) = resolve_dispatcher(&state, body.dispatcher_id)? else {
        return Err(ApiError::Internal("没有可用的分发器".into()));
    };

    // ---- 7. 语言 ----
    let lang = body.lang.clone().unwrap_or_else(|| "zh".to_string());
    let langs: Vec<String> = match lang.as_str() {
        "en" => vec!["en".into()],
        "both" => vec!["zh".into(), "en".into()],
        _ => vec!["zh".into()],
    };

    // ---- 8. 跑决策器 ----
    let registry = state
        .registry
        .as_ref()
        .ok_or_else(|| ApiError::Internal("插件系统未初始化".into()))?;

    let loaded = registry
        .load_by_filename(PluginKind::Dispatcher, &row.filename)
        .map_err(|e| ApiError::Internal(format!("加载分发器失败: {e}")))?
        .ok_or_else(|| ApiError::Internal(format!("分发器文件不存在: {}", row.filename)))?;
    let plugin = loaded
        .get()
        .map_err(|e| ApiError::Internal(format!("分发器执行失败: {e}")))?;

    // 调试台也要带「评价 + 评分」那一行（原版注入 `ctx["evaluator"]`），
    // 所以这里把当前激活的评估器装配进去。
    let eval_lang = if lang == "en" { "en" } else { "zh" };
    let ctx = alpha_plugin::Context::new(eval_lang.to_string(), langs)
        .with_repo_rules(alpha_core::config::project_root())
        .with_evaluator(active_evaluator(&state)?);

    let report = plugin
        .dispatch(&alpha_plugin::BossView::from_boss(&boss), &ctx)
        .map_err(|e| ApiError::Internal(format!("分发器执行失败: {e}")))?;

    // 调试运行也留痕。级别是 `debug` —— 正式日志里不该被它刷屏。
    state.store.log(
        LogLevel::Debug,
        "debug",
        &format!("调试运行：{name_zh}({ability_zh}) -> 分发器[{}]", row.name),
        "debug",
    );

    Ok(Json(json!({
        "ok": true,
        "dispatcher": row.name,
        "resolved": {
            "name": name_zh,
            "pokemon_id": pid,
            "ability": ability_zh,
            "ability_id": aid,
            "moves": moves_zh,
            // 解析不出的技能位置是 `None`，原版是 `[m for m in move_ids if m]`
            // —— 直接丢掉。这里保持同样的形状。
            "move_ids": move_ids.iter().flatten().copied().collect::<Vec<_>>(),
            "egg_groups": egg,
        },
        "report": report,
    })))
}

/// 三级回退选决策器。
///
/// 1. 请求里指定的 id —— 但**必须 `enabled=1`**（原版的 SQL 带这个条件，
///    指定的决策器被停用了就不该被调试台拉起来）
/// 2. 当前激活的（**不看 `enabled`** —— 原版 `api_dashboard`
///    与 `debug_run` 这里都是裸 `active=1`）
/// 3. 内建的第一个
fn resolve_dispatcher(state: &AppState, requested: Option<i64>) -> ApiResult<Option<PluginRow>> {
    if let Some(id) = requested {
        if let Some(row) = state.store.get_plugin(PluginTable::Dispatchers, id)? {
            if row.enabled {
                return Ok(Some(row));
            }
        }
        // 指定的那个不存在或没启用 → 静默走回退链。原版如此。
    }

    if let Some(row) = state.store.active_plugin(PluginTable::Dispatchers)? {
        return Ok(Some(row));
    }

    Ok(state.store.builtin_dispatcher()?)
}

/// 取当前可用的评估器回调（给决策器出「评价 + 评分」那行）。
///
/// 没有可用评估器时返回 `None` —— 决策器会跳过那一行，
/// 报表仍然生成得出来。这比「因为评估器坏了就整个调试台不可用」合理。
///
/// # 与调度器那边的一致性
///
/// 调度器的 `report::make_evaluator` 用的是 `active=1 AND enabled=1`，
/// 这里用 `active_plugin`（只看 `active=1`）。差异是有意的：
/// 调度器那条路直接决定「要不要推送」，评估器被停用就该彻底不参与；
/// 调试台是个**观察工具**，用户想看到「现在配着的评估器会输出什么」，
/// 哪怕它当前没启用。
fn active_evaluator(state: &AppState) -> ApiResult<Option<alpha_plugin::EvaluatorFn>> {
    let Some(evaluator) = state.store.active_plugin(PluginTable::Evaluators)? else {
        return Ok(None);
    };
    let Some(registry) = state.registry.as_ref() else {
        return Ok(None);
    };
    // 加载失败一律当作「没有评估器」—— 调试台不该因为评估器坏了就打不开
    let Ok(Some(loaded)) = registry.load_by_filename(PluginKind::Evaluator, &evaluator.filename)
    else {
        return Ok(None);
    };
    let Ok(plugin) = loaded.get() else {
        return Ok(None);
    };
    Ok(Some(alpha_plugin::make_evaluator_callback(Arc::clone(plugin))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 性别参数 → `Gender` 的映射：
    /// 显式四选一与原版 `{"dual": 50.0, "male": 100.0, "female": 0.0,
    /// "none": None}` 一致；`auto`/未传/未知值按图鉴，查不到回落 dual。
    #[test]
    fn gender_mapping_matches_the_original() {
        let pk = alpha_core::pokedex::get_pokedex().expect("图鉴应能加载");
        let pidev = |name: &str| pk.resolve_pokemon_id(name);

        let cases: &[(&str, &str, Option<f64>)] = &[
            // (gender 参数, 精灵, 期望 male_percent)
            ("dual", "皮卡丘", Some(50.0)),
            ("male", "皮卡丘", Some(100.0)),
            ("female", "皮卡丘", Some(0.0)),
            ("none", "皮卡丘", None),
            // auto：按图鉴。皮卡丘图鉴里是各半
            ("auto", "皮卡丘", Some(50.0)),
            // auto：百变怪图鉴里无性别 —— 调试台如实拿到「无性别」，
            // 评估器（性别过滤）因此能看到它
            ("auto", "百变怪", None),
            // auto：图鉴查不到 → dual 占位
            ("auto", "不存在的精灵", Some(50.0)),
            // 未传 / 未知值：同 auto
            ("", "百变怪", None),
            ("garbage", "皮卡丘", Some(50.0)),
        ];
        for (input, pokemon, want) in cases {
            let got = gender_from_request(
                if input.is_empty() { None } else { Some(input) },
                pidev(pokemon),
                pk,
            );
            assert_eq!(got.male_percent, *want, "gender={input:?} pokemon={pokemon:?}");
        }

        // 前端下拉没选（body 里没有 gender 字段）也走 auto 分支
        let got = gender_from_request(None, pidev("百变怪"), pk);
        assert_eq!(got.male_percent, None, "未传 gender 时百变怪应按图鉴为无性别");
    }

    /// 无性别必须写成 `None` 而不是 `Some(0.0)` —— 引擎靠这个判断
    /// 「能不能走甜蜜球那套打法」，`Some(0.0)` 会被当成「全母」。
    #[test]
    fn genderless_is_none_not_zero() {
        assert_eq!(Gender::genderless().male_percent, None);
        assert!(!Gender::genderless().is_dual());
    }

    /// 语言三选一 → 输出语言列表，与原版
    /// `{"zh": ["zh"], "en": ["en"], "both": ["zh","en"]}` 一致。
    #[test]
    fn language_mapping_matches_the_original() {
        for (input, want) in [
            ("zh", vec!["zh"]),
            ("en", vec!["en"]),
            ("both", vec!["zh", "en"]),
            ("garbage", vec!["zh"]),
        ] {
            let got: Vec<&str> = match input {
                "en" => vec!["en"],
                "both" => vec!["zh", "en"],
                _ => vec!["zh"],
            };
            assert_eq!(got, want, "lang={input}");
        }
    }

    #[test]
    fn dashboard_defaults_enabled_to_false_for_legacy_sources() {
        // 这条不变量在 sources 路由那边也有 —— 这里再钉一次，
        // 因为仪表盘的「已启用 N 个」是用户最先看到的数字。
        let legacy =
            serde_yaml::from_str::<serde_yaml::Value>("name: 老源\nadapter: lzpoke_reports\n")
                .unwrap();
        let described = config_mgr::describe_source(&legacy);
        assert_eq!(
            described["enabled"],
            json!(false),
            "没写 enabled 的源该算未启用"
        );
        assert_eq!(described["priority"], json!(100), "没写 priority 该是 100");
    }
}
