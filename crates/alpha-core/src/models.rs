//! 领域模型。
//!
//! 这里定义的是「数据源」和「打法引擎」之间的契约：
//! 不管上游 API 长什么样、返回什么语言，适配器都必须把数据捏成 `BossData`，
//! 引擎只认这一种结构。
//!
//! 对应原版 `src/core/models.py`，字段语义逐一对齐。

use serde::{Deserialize, Serialize};

/// 性别比例。
///
/// `male_percent` 为雄性百分比；`None` 表示无性别（不能走甜蜜球那套打法）。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Gender {
    pub male_percent: Option<f64>,
}

impl Gender {
    /// 无性别。
    pub fn genderless() -> Self {
        Self { male_percent: None }
    }

    pub fn from_male_percent(v: Option<f64>) -> Self {
        Self { male_percent: v }
    }

    /// 同进化链存在异性 —— 出招方案只对这种情况生效。
    pub fn is_dual(&self) -> bool {
        match self.male_percent {
            Some(p) => p > 0.0 && p < 100.0,
            None => false,
        }
    }
}

/// 附加信息行。
///
/// 不同 API 能提供的额外字段不一样（比如只有 Alphapedia 给秘传机信息），
/// 由适配器决定要不要给、给什么内容，主流程负责插到固定位置：
/// 「头目信息」和「打法推荐」之间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraLine {
    pub zh: String,
    pub en: String,
}

impl ExtraLine {
    pub fn new(zh: impl Into<String>, en: impl Into<String>) -> Self {
        Self {
            zh: zh.into(),
            en: en.into(),
        }
    }

    /// 按语言取文本。
    pub fn text(&self, lang: &str) -> &str {
        if lang == "en" {
            &self.en
        } else {
            &self.zh
        }
    }
}

/// 归一化后的头目信息。
///
/// 三个必需字段是打法引擎真正依赖的全部内容：
/// - `name`    —— 头目名称（已归一化为官方中文）
/// - `ability` —— 特性（已归一化为官方中文）
/// - `moves`   —— 四个技能（已归一化为官方中文）
///
/// 其余都是可选项，缺了不影响出招，只是推送内容少几行。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BossData {
    // ---- 必需 ----
    pub name: String,
    pub ability: String,
    #[serde(default)]
    pub moves: Vec<String>,

    // ---- 展示用可选信息 ----
    #[serde(default)]
    pub period: String,
    /// 中文地点
    #[serde(default)]
    pub location: String,
    /// 英文地点（源提供时填，否则英文推送里会夹中文）
    #[serde(default)]
    pub location_en: String,
    #[serde(default)]
    pub reporter: Option<String>,
    #[serde(default)]
    pub gender: Gender,
    #[serde(default)]
    pub egg_groups: Vec<String>,
    #[serde(default)]
    pub egg_groups_en: Vec<String>,
    #[serde(default)]
    pub extra_lines: Vec<ExtraLine>,

    // ---- 解析副产物：canonical id，引擎用来做规则匹配 ----
    #[serde(default)]
    pub pokedex_id: Option<i64>,
    #[serde(default)]
    pub ability_id: Option<i64>,
    #[serde(default)]
    pub move_ids: Vec<Option<i64>>,

    // ---- 溯源 ----
    #[serde(default)]
    pub source: String,
    /// 报点时间（ISO），用于多源投票裁决与跨源去重分桶
    #[serde(default)]
    pub reported_at: String,
}

impl BossData {
    /// 引擎匹配用的技能 id 集合（解析失败的技能会被跳过）。
    pub fn move_id_set(&self) -> std::collections::HashSet<i64> {
        self.move_ids.iter().filter_map(|i| *i).collect()
    }

    pub fn has_move_id(&self, mid: i64) -> bool {
        self.move_ids.contains(&Some(mid))
    }
}

/// 数据源适配器返回值。
///
/// `status`:
/// - `hit`   —— 拿到头目且有效，应该推送
/// - `empty` —— 请求成功，但当前时段确实没刷 / 头目已过期
/// - `error` —— 请求或解析失败（主流程会据此决定是否换下一个源）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FetchStatus {
    Hit,
    Empty,
    Error,
}

impl FetchStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FetchStatus::Hit => "hit",
            FetchStatus::Empty => "empty",
            FetchStatus::Error => "error",
        }
    }

    /// 解析状态字符串。
    ///
    /// 用 [`FetchStatus::parse`] 而不是 `FromStr`：`FromStr` 的 `Err` 类型
    /// 要求调用方处理「未知状态」这个分支，但本项目的语义是
    /// **任何不认识的输入一律归为 `Error`**（宁可当失败重试，也不能当成
    /// `Hit` 去写去重标记）。这样调用方就不必写 `unwrap_or`。
    pub fn parse(s: &str) -> Self {
        match s {
            "hit" => FetchStatus::Hit,
            "empty" => FetchStatus::Empty,
            _ => FetchStatus::Error,
        }
    }
}

/// 数据源 `fetch()` 的返回结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    pub status: FetchStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boss: Option<BossData>,

    // 下面三项都由适配器决定 —— 不是所有数据源都有「时段」这个概念。
    // 没有时段概念的源：填 dedup_key 就行，slot_name 留空（摘要走兜底）；
    // 连 dedup_key 都不填也没关系，基类会用头目内容算指纹兜底。
    /// 去重用标识（通常是报点时间）
    #[serde(default)]
    pub dedup_key: String,
    /// 时段名（早头/午头/晚头/凌晨头），用作推送摘要
    #[serde(default)]
    pub slot_name: String,
    /// 时段名英文
    #[serde(default)]
    pub slot_name_en: String,
    #[serde(default)]
    pub message: String,
}

impl FetchResult {
    pub fn hit(boss: BossData) -> Self {
        Self {
            status: FetchStatus::Hit,
            boss: Some(boss),
            dedup_key: String::new(),
            slot_name: String::new(),
            slot_name_en: String::new(),
            message: String::new(),
        }
    }

    pub fn empty(message: impl Into<String>) -> Self {
        Self {
            status: FetchStatus::Empty,
            boss: None,
            dedup_key: String::new(),
            slot_name: String::new(),
            slot_name_en: String::new(),
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            status: FetchStatus::Error,
            boss: None,
            dedup_key: String::new(),
            slot_name: String::new(),
            slot_name_en: String::new(),
            message: message.into(),
        }
    }

    pub fn with_dedup_key(mut self, key: impl Into<String>) -> Self {
        self.dedup_key = key.into();
        self
    }

    pub fn with_slot(mut self, zh: impl Into<String>, en: impl Into<String>) -> Self {
        self.slot_name = zh.into();
        self.slot_name_en = en.into();
        self
    }

    pub fn with_message(mut self, msg: impl Into<String>) -> Self {
        self.message = msg.into();
        self
    }

    /// 是否命中（status == hit 且有 boss）。
    pub fn is_hit(&self) -> bool {
        self.status == FetchStatus::Hit && self.boss.is_some()
    }
}
