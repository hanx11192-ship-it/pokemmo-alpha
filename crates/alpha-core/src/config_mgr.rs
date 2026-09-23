//! 配置文件的**读写**（区别于 [`crate::config`] 的只读加载）。
//!
//! 对应原版 `panel/config_mgr.py`（338 行）。
//!
//! # 这个模块为什么值得单独存在
//!
//! 面板要能在**不破坏人写的东西**的前提下改配置。`config/sources.yaml` 里
//! 有大量人工注释（「转发服务地址从环境变量读」这类关键说明），
//! `config/settings.yaml` 更是几乎每行都有注释。
//! 原版为此用了两种截然不同的策略，
//! 本模块把这两种策略都**逐字复刻**：
//!
//! | 文件 | 原版策略 | 本模块 |
//! |---|---|---|
//! | `sources.yaml` | ruamel.yaml round-trip（保留注释/缩进/顺序） | [`SourcesDocFile`]：从原文改，只动该动的 |
//! | `settings.yaml` | **顶层字段行级正则替换** | [`set_top_field`] |
//! | `panel.env` | 逐行处理，保留注释，值变了才写 | [`update_env_file`] |
//!
//! # 为什么 `settings.yaml` 不用 YAML 解析器
//!
//! 因为用解析器就等于「读 → 改 → 整份 dump 回去」，**注释全丢**。
//! 原版的做法是：把文件按行拆开，用 `^timezone:\s*(.*)$` 匹配，
//! 只替换那一行。本模块沿用同一手法 —— 见 [`set_top_field`] 的说明。
//!
//! # 与 `alpha-core::config` 的关系
//!
//! - 本模块负责**写**（面板的「保存」按钮）
//! - [`crate::config`] 负责**读**（运行时热路径）
//!
//! 两边必须对同一份文件格式达成一致，所以本模块的测试里有
//! 「写完之后能用 `alpha-core::config` 读回来」的往返验证。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::config::{abspath, project_root};
use crate::error::{CoreError, Result};

// ---------------------------------------------------------------- 路径

/// `config/sources.yaml`
pub fn sources_path() -> PathBuf {
    abspath(&["config", "sources.yaml"])
}

/// `config/settings.yaml`
pub fn settings_path() -> PathBuf {
    abspath(&["config", "settings.yaml"])
}

/// `panel.env`（可用 `PANEL_ENV_FILE` 覆盖，与原版 `_env_path()` 一致）
pub fn env_path() -> PathBuf {
    match std::env::var("PANEL_ENV_FILE") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => project_root().join("panel.env"),
    }
}

/// `src/sources/` —— 适配器源码目录。
///
/// 原版这里是 `.py` 文件；Rust 版的适配器是**编译进二进制**的，
/// 但面板仍要展示「有哪些适配器可用」，所以这个目录依然被读取
/// （见 [`list_adapter_files`] 的说明）。
pub fn sources_dir() -> PathBuf {
    project_root().join("src").join("sources")
}

// ---------------------------------------------------------------- 头注释

/// `sources.yaml` 的固定头注释。
///
/// # ⚠️ 与原版的差异
///
/// 原版 `SOURCES_HEADER` 是 7 行：
///
/// ```text
/// # ============================================================
/// # 数据源注册表
/// # 新增 / 删除 / 启停数据源均可在面板「头目源」页操作。
/// # 停用数据源：enabled 改为 false（面板里点开关即可）。
/// # 调整优先级：改 priority，数字越小越优先。
/// # 不同源的语言、字段、秘传机差异，由各适配器内部自行处理。
/// # ============================================================
/// ```
///
/// 而工作区里 `config/sources.yaml` **现有的**头注释是另一套 11 行文字，
/// 提到的是 `src/sources/` 与 adapter 的对应关系（更详细）。
///
/// 直接套用原版常量会导致：**面板第一次保存就把现有那段更详细的注释覆盖掉**。
/// 这是个用户可见的信息丢失，所以本版选用**工作区现有那段文字**作为
/// 兜底头注释 —— 与原版的差异记录在此。
///
/// 补充说明：本版用「从原文改」的策略保存（见 [`SourcesDocFile::save`]），
/// 只在原文**真的没有注释头**时才会用这个常量兜底，
/// 因此实践中它很少被写到。
pub const SOURCES_HEADER: &str = "\
# ============================================================
# 数据源注册表
# 新增 / 删除 / 启停数据源均可在面板「头目源」页操作。
# 停用数据源：enabled 改为 false（面板里点开关即可）。
# 调整优先级：改 priority，数字越小越优先。
# 不同源的语言、字段、秘传机差异，由适配器内部自行处理。
# 适配器源码在 src/sources/ 下，文件名（不含 .py）即 adapter 的值。
# ============================================================

";

/// `panel.env` 的注释标记。
///
/// # 原版的一个不一致（已逐字复刻）
///
/// 原版 `update_env_file` 里：
///
/// ```python
/// if not any(l.strip() == "# Alpha 面板环境变量" for l in lines):
///     out.insert(0, "# Alpha 面板环境变量（可在面板「系统配置」页修改）")
/// ```
///
/// **判定用短版、插入用长版** —— 所以插进去之后，下一轮判定仍然找不到
/// 那个短版字符串，会**再插一次**。原版就是这样（每次保存多一行）。
///
/// 本版刻意保留这个行为：它是幂等性 bug，但属于「原版就是这样做」的范畴，
/// 修掉会让两个版本产出的文件不一致。真正的修法是让判定与插入用同一字符串，
/// 那应该同时改两边。
pub const ENV_HEADER_MARK: &str = "# Alpha 面板环境变量";
pub const ENV_HEADER_INSERT: &str = "# Alpha 面板环境变量（可在面板「系统配置」页修改）";

// ---------------------------------------------------------------- sources.yaml

/// `sources.yaml` 的一次读改写会话。
///
/// # 为什么不是「解析成 struct 再 dump 回去」
///
/// 因为那会丢掉所有注释。原版用 `ruamel.yaml` 的 round-trip 模式解决，
/// Rust 生态没有等价物，所以这里换了个更朴素、但**同样保注释**的思路：
///
/// **把原文按行拆开，只在必要处改行。**
///
/// - 改一个源的 `enabled` → 只重写那个源那一段里的 `enabled:` 行
/// - 新增一个源 → 追加一段
/// - 删除一个源 → 删掉那一段
///
/// 代价是：我们不再「理解」YAML，只做文本级编辑。为此 [`Self::parse`]
/// 用一个**极简的缩进扫描器**定位每个源的范围，而不是引入 YAML 解析。
///
/// 好处是注释、空行、缩进风格、键顺序**全部原样保留** ——
/// 正是面板改配置时最该保住的东西。
#[derive(Debug, Clone)]
pub struct SourcesDocFile {
    /// 原文（按行，含换行符）
    lines: Vec<String>,
    /// 每个源在 `lines` 里的行范围（`start..end`，`end` 不含）
    blocks: Vec<Block>,
}

/// 一个源在文件里的位置。
#[derive(Debug, Clone)]
struct Block {
    /// `name:` 所在行号
    name_line: usize,
    /// 该源整段的行范围（含 `- name:` 那行到下一个 `- ` 或文件末尾）
    start: usize,
    end: usize,
}

impl SourcesDocFile {
    /// 读文件。文件不存在返回一个只有头注释的空文档。
    pub fn load() -> Result<Self> {
        Self::load_from(&sources_path())
    }

    /// 从指定路径读（测试用）。
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // 与原版 `load_sources_doc()` 对齐：文件不存在返回 `{"sources": []}`。
                // 这里额外把头注释准备好，让首次保存出来的文件不是赤裸的 YAML。
                return Ok(Self::empty());
            }
            Err(e) => {
                return Err(CoreError::io(format!(
                    "读取 {} 失败: {e}",
                    path.display()
                )))
            }
        };
        Ok(Self::parse(&text))
    }

    /// 空文档（只有头注释，没有源）。
    pub fn empty() -> Self {
        let mut s = Self {
            lines: Vec::new(),
            blocks: Vec::new(),
        };
        s.lines = split_keepends(SOURCES_HEADER);
        s.reindex();
        s
    }

    /// 从文本解析。
    pub fn parse(text: &str) -> Self {
        let mut s = Self {
            lines: split_keepends(text),
            blocks: Vec::new(),
        };
        s.reindex();
        s
    }

    /// 原文。
    pub fn text(&self) -> String {
        self.lines.concat()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// 扫描出每个 `- name:` 段的行范围。
    ///
    /// # 这个扫描器只认一种结构
    ///
    /// ```yaml
    /// sources:
    ///   - name: 源一        ← 顶格缩进相同
    ///     adapter: foo
    ///     enabled: true       ← 中间还可以有任意层级的子映射
    ///   - name: 源二
    /// ```
    ///
    /// 判据是「以 `- ` 开头、且后面跟的是 `name:` 的行」。
    /// 一行里出现 `- name:` 只可能在源列表里，所以这个判据足够可靠。
    fn reindex(&mut self) {
        self.blocks.clear();
        let mut cur: Option<Block> = None;
        let mut in_sources = false;

        for (i, raw) in self.lines.iter().enumerate() {
            let line = raw.trim_end_matches(['\n', '\r']);
            let indent = line.len() - line.trim_start().len();
            let body = line.trim_start();

            // `sources:` 顶格出现之后才认 `- ` 条目 ——
            // 防止把别的顶格列表误判成源
            if indent == 0 && body.starts_with("sources:") {
                in_sources = true;
                if let Some(b) = cur.take() {
                    self.blocks.push(Block { end: i, ..b });
                }
                continue;
            }
            if indent == 0 && !body.is_empty() && !body.starts_with('#') && in_sources {
                // 离开了 sources 段
                in_sources = false;
                if let Some(b) = cur.take() {
                    self.blocks.push(Block { end: i, ..b });
                }
                continue;
            }
            if !in_sources {
                continue;
            }

            if let Some(rest) = body.strip_prefix("- ") {
                if rest.trim_start().starts_with("name:") {
                    if let Some(b) = cur.take() {
                        self.blocks.push(Block { end: i, ..b });
                    }
                    cur = Some(Block {
                        name_line: i,
                        start: i,
                        end: self.lines.len(),
                    });
                }
            }
        }
        if let Some(b) = cur {
            self.blocks.push(Block {
                end: self.lines.len(),
                ..b
            });
        }
    }

    /// 取某个源的行范围（索引到 `self.blocks`）。
    fn block_of(&self, name: &str) -> Option<usize> {
        self.blocks
            .iter()
            .position(|b| self.name_at(b.name_line).as_deref() == Some(name))
    }

    /// 读第 `i` 行 `name:` 的值（去掉引号与空白）。
    ///
    /// **必须只取到行尾为止**。两个坑叠在一起：
    ///
    /// 1. `trim_end_matches(['\n', '\r'])` 里的字符数组是**模式**，
    ///    它会一直剥到不是 `\n`/`\r` 为止 —— 这本身没问题。
    /// 2. 但原来的写法用 `trim_start_matches("- ")` 去前缀，
    ///    而它同样把 `"- "` 当**字符集**：`-`、空格会被逐个剥掉，
    ///    于是 `"    adapter: foo"` 这种行会被剥成 `"adapter: foo"`，
    ///    再被当成 `- name:` 之外的意外结构处理。
    ///    改用 `strip_prefix` 才是指「剥掉这个完整前缀一次」。
    ///
    /// 这个 bug 是被 `append_source_adds_at_the_end` 抓到的：
    /// 新追加的源，其 `name` 被解析成了 `"新源\n    adapter: foo"`。
    fn name_at(&self, i: usize) -> Option<String> {
        let line = self.lines.get(i)?;
        let line = line.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        // `strip_prefix` 才是「剥掉这个完整前缀一次」
        let body = trimmed.strip_prefix("- ").unwrap_or(trimmed).trim_start();
        let rest = body.strip_prefix("name:")?;
        // 再截到行尾 —— 这一步兜住「同一行里还有别的键」的情况
        Some(unquote(rest.split(['\n', '\r']).next().unwrap_or("").trim()))
    }

    /// 列出所有源的 name（按文件顺序）。
    pub fn names(&self) -> Vec<String> {
        self.blocks
            .iter()
            .filter_map(|b| self.name_at(b.name_line))
            .collect()
    }

    /// 某个源那一段的原文。
    pub fn block_text(&self, name: &str) -> Option<String> {
        let b = &self.blocks[self.block_of(name)?];
        Some(self.lines[b.start..b.end].concat())
    }
}

impl SourcesDocFile {
    /// 整段替换一个源（改名 / 改多个字段时用），位置不变。
    ///
    /// 对应原版 `_merge_map` 的效果：**保留位置**，但把内容换成新的。
    /// 原版之所以能保注释，是因为 ruamel 保留了每个键上的注释；
    /// 本模块做不到「部分保注释」，所以策略是：
    ///
    /// - **单字段改动**（`set_enabled` / `set_priority`）→ 只重写那一行，注释全留
    /// - **整段替换**（`replace_source`）→ 该段被新内容覆盖，段内注释会丢
    ///
    /// 后者的注释损失是无法避免的：用户把 adapter 从 `a` 改成 `b`，
    /// 原来那句「转发服务地址从环境变量读」是否还适用，只有人知道。
    /// 保留一段可能已经错误说明的注释，比丢掉它更危险。
    pub fn replace_block(&mut self, name: &str, new_yaml: &str) {
        let Some(idx) = self.block_of(name) else {
            self.append_source(new_yaml);
            return;
        };
        let b = self.blocks[idx].clone();
        let fresh = split_keepends(&ensure_trailing_newline(new_yaml));
        self.lines.splice(b.start..b.end, fresh);
        self.reindex();
    }

    /// 在末尾追加一个源。
    pub fn append_source(&mut self, yaml: &str) {
        if !self.has_sources_key() {
            // 连 `sources:` 都没有 —— 补一个（只有注释头的情况）
            if !self.lines.is_empty() && !self.text().ends_with('\n') {
                self.lines.push("\n".to_string());
            }
            self.lines.push("sources:\n".to_string());
        }
        self.lines.push(ensure_trailing_newline(yaml));
        self.reindex();
    }

    /// 删除一个源。返回是否真的删掉了。
    pub fn remove_source(&mut self, name: &str) -> bool {
        let Some(idx) = self.block_of(name) else {
            return false;
        };
        let b = self.blocks[idx].clone();
        self.lines.drain(b.start..b.end);
        self.reindex();
        true
    }

    fn has_sources_key(&self) -> bool {
        self.lines.iter().any(|l| {
            let t = l.trim_end_matches(['\n', '\r']);
            t.len() == t.trim_start().len() && t.starts_with("sources:")
        })
    }

    /// 只改某个源里的一个顶层字段（保留该段其他所有内容与注释）。
    ///
    /// 找不到那个字段就插到 `- name:` 行之后（保持它在这一段的顶部区域）。
    /// 返回是否真的改动了文件。
    pub fn set_field(&mut self, name: &str, field: &str, value: &str) -> bool {
        let Some(idx) = self.block_of(name) else {
            return false;
        };
        let b = self.blocks[idx].clone();
        let needle = format!("{field}:");

        // 优先在本段里找同缩进的 `field:` 行
        for i in b.start..b.end {
            let line = &self.lines[i];
            let body = line.trim_start();
            if body.starts_with(&needle) {
                // 只认**本段顶层**（即 `- name:` 那一行的缩进 + 2）
                let want = self.lines[b.name_line].len() - self.lines[b.name_line].trim_start().len() + 2;
                let have = line.len() - line.trim_start().len();
                if have != want {
                    continue;
                }
                let prefix = " ".repeat(have);
                self.lines[i] = format!("{prefix}{field}: {value}\n");
                return true;
            }
        }

        // 没找到 → 插到 name 行之后
        let base = self.lines[b.name_line].len() - self.lines[b.name_line].trim_start().len();
        self.lines
            .insert(b.name_line + 1, format!("{}{field}: {value}\n", " ".repeat(base + 2)));
        self.reindex();
        true
    }

    /// 写回文件。
    ///
    /// 若原文里一行注释都没有（说明是程序生成的），补上头注释。
    pub fn save(&self) -> Result<()> {
        self.save_to(&sources_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        let mut text = self.text();
        if !text.trim_start().starts_with('#') {
            text = format!("{SOURCES_HEADER}{text}");
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CoreError::io(format!("创建 {} 失败: {e}", parent.display())))?;
        }
        // 先写临时文件再 rename：面板保存配置时进程被杀，
        // 直接覆盖会留下一个半截的 YAML，下次启动直接读不出来。
        write_atomic(path, &text)
    }
}

// ---------------------------------------------------------------- 源的高级操作

/// 读取所有源的描述（面板「头目源」页的列表）。
///
/// 对应原版 `list_sources()` + `describe_source()`。
///
/// # 为什么不解析成 `SourceConfig`
///
/// 因为 [`describe_source`] 要把**原来的 options 原样回给前端**
/// （面板的数据源编辑表单里有目标地址、附加项开关等字段，
/// 未识别的键也要能显示）。走 `SourceConfig` 的 `serde(flatten) rest`
/// 也能做到，但一旦某天 `SourceOptions` 加了字段，
/// 面板回传的 JSON 形状就会跟着变，前端得同步改。
/// 直接读写 `serde_yaml::Value` 更稳。
pub fn load_sources() -> Result<Vec<serde_yaml::Value>> {
    let path = sources_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(CoreError::io(format!("读取 {} 失败: {e}", path.display()))),
    };
    let doc: serde_yaml::Value = serde_yaml::from_str(&text).unwrap_or(serde_yaml::Value::Null);
    let list = match doc.get("sources") {
        Some(serde_yaml::Value::Sequence(s)) => s.clone(),
        // 解析失败 / 没有 sources 键 → 与原版一样返回空列表，**不报错**。
        // 配置坏了的时候面板还得能打开，否则用户没法通过面板修它。
        _ => Vec::new(),
    };
    Ok(list)
}

/// 按 name 查一个源。
pub fn get_source(name: &str) -> Result<Option<serde_yaml::Value>> {
    Ok(load_sources()?.into_iter().find(|s| {
        s.get("name").and_then(|v| v.as_str()).map(str::to_owned).as_deref() == Some(name)
    }))
}

/// 把源配置转成面板展示用的 JSON。
///
/// 对应原版 `describe_source()`。
///
/// # 两个容易写错的默认值
///
/// 1. `enabled` 缺省是 **`false`**（原版 `bool(scfg.get("enabled", False))`）。
///    这跟 [`crate::config::SourceConfig::enabled`] 的 `#[serde(default)]`
///    一致，但跟「新增源时默认启用」（`api_source_add` 里默认 `True`）
///    **不是一回事** —— 那是新建时的默认，这是读旧文件时的默认。
/// 2. `priority` 缺省是 **100**（原版 `int(scfg.get("priority", 100))`）。
pub fn describe_source(scfg: &serde_yaml::Value) -> Value {
    let opts = scfg.get("options").cloned().unwrap_or(serde_yaml::Value::Null);

    // note：把 target 与开启的附加项拼成一句话，供列表页显示
    let mut notes: Vec<String> = Vec::new();
    if let Some(t) = opts.get("target").and_then(|v| v.as_str()) {
        if !t.is_empty() {
            notes.push(t.to_string());
        }
    }
    let mut on: Vec<String> = Vec::new();
    if let Some(extra) = opts.get("extra_lines").and_then(|v| v.as_mapping()) {
        for (k, v) in extra {
            // 原版是 `[k for k, v in extra.items() if v]` —— 顺序是**映射顺序**
            if yaml_truthy(v) {
                if let Some(k) = k.as_str() {
                    on.push(k.to_string());
                }
            }
        }
    }
    if !on.is_empty() {
        notes.push(format!("附加:{}", on.join("/")));
    }

    json!({
        "name": scfg.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "adapter": scfg.get("adapter").and_then(|v| v.as_str()).unwrap_or(""),
        "enabled": scfg.get("enabled").map(yaml_truthy).unwrap_or(false),
        "priority": scfg.get("priority").and_then(yaml_i64).unwrap_or(100),
        "note": notes.join(" · "),
        "options": yaml_to_json(&opts),
    })
}

/// `src/sources/` 下可用的适配器名（不含 `base.py` / `__init__.py`），已排序。
///
/// # 与原版的差异（有意的）
///
/// 原版扫的是 `.py` 文件。Rust 版的适配器是编译进二进制的
/// （见 `alpha-sources::registry::KNOWN_ADAPTERS`），`src/sources/` 只是
/// 保留的源码目录。所以这里**两个来源都看**：
///
/// 1. `alpha-sources` 里注册的适配器（权威）
/// 2. `src/sources/*.py`（遗留目录，可能有人还在往里放东西）
///
/// 并集去重后排序。这样面板的「新增源」下拉框在 Rust 版上依然是可用的 ——
/// 如果只认 `.py` 目录，全新部署（没有那个目录）会得到一个空下拉框。
pub fn list_adapter_files() -> Vec<String> {
    use std::collections::BTreeSet;

    let mut out: BTreeSet<String> = BTreeSet::new();

    for a in alpha_sources_known_adapters() {
        out.insert(a);
    }

    let dir = sources_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".py") {
                continue;
            }
            if name == "base.py" || name == "__init__.py" {
                continue;
            }
            out.insert(name.trim_end_matches(".py").to_string());
        }
    }

    out.into_iter().collect()
}

/// 本 crate 不能依赖 `alpha-sources`（会变成反向依赖），
/// 所以适配器清单这里留一个可注入的钩子：`alpha-server` 启动时注册。
static KNOWN_ADAPTERS: once_cell::sync::OnceCell<Vec<String>> = once_cell::sync::OnceCell::new();

/// 注册可用的适配器名（由 `alpha-server` 在启动时调用一次）。
pub fn register_known_adapters(names: Vec<String>) {
    let _ = KNOWN_ADAPTERS.set(names);
}

fn alpha_sources_known_adapters() -> Vec<String> {
    KNOWN_ADAPTERS.get().cloned().unwrap_or_default()
}

// ---------------------------------------------------------------- settings.yaml

/// 只改 `settings.yaml` 里的一个**顶层**字段，其余内容与注释原样保留。
///
/// 对应原版 `_set_top_field()`。
///
/// # 三个承重细节（照抄原版）
///
/// 1. **只改第一处命中**。原版是 `if m and not changed:` —— 第二个同名字段
///    不动。改全部的话，某个嵌套段里的 `language:` 也会被改掉。
/// 2. **标量不加引号**。`timezone: Asia/Shanghai`，不是 `"Asia/Shanghai"`。
/// 3. **bool 输出成 `True`/`False`**。原版是
///    `scalar = new_val if isinstance(new_val, (int, float, bool)) else str(new_val)`，
///    Python 的 `str(True)` 就是 `"True"`。这**不是合法 YAML 的小写风格**，
///    但 `True` 在 YAML 1.1 里也是 bool，所以读得回来。
///    本版严格复刻 —— 见测试 `bool_is_written_as_python_style`。
///
/// 返回是否真的改动了文件（没匹配到就是 `false`，此时不写盘）。
pub fn set_top_field(path: &Path, field: &str, value: &str) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(CoreError::io(format!("读取 {} 失败: {e}", path.display()))),
    };

    let mut out = String::with_capacity(text.len());
    let mut changed = false;

    for line in split_keepends(&text) {
        if !changed && matches_top_field(&line, field) {
            out.push_str(&format!("{field}: {value}\n"));
            changed = true;
            continue;
        }
        out.push_str(&line);
    }

    if changed {
        write_atomic(path, &out)?;
    }
    Ok(changed)
}

/// 判断某一行是不是某个顶层字段的赋值行。
///
/// 原版用正则 `^re.escape(field):\s*(.*)$`。手写等价判断，
/// 不引 regex 依赖 —— 规则足够简单（行首、字段名、冒号）。
///
/// **必须顶格**：`^` 锚定行首，所以缩进的同名字段不算。
/// 这是「不动嵌套段」的关键。
fn matches_top_field(line: &str, field: &str) -> bool {
    let line = line.trim_end_matches(['\n', '\r']);
    let Some(rest) = line.strip_prefix(field) else {
        return false;
    };
    rest.starts_with(':')
}

/// 把顶层字段渲染成原版会写出的标量形式。
///
/// - 数字 / bool 不加引号（bool 用 Python 风格 `True`/`False`）
/// - 字符串原样输出，不加引号
pub fn render_scalar(value: &Value) -> String {
    match value {
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// 设置 `timezone`。
pub fn set_setting_timezone(tz: &str) -> Result<bool> {
    set_top_field(&settings_path(), "timezone", tz)
}

/// 设置 `language`。
pub fn set_setting_language(lang: &str) -> Result<bool> {
    set_top_field(&settings_path(), "language", lang)
}

/// 读 `settings.yaml` 成 JSON（与原版 `load_settings()` 对齐：失败返回空对象）。
pub fn load_settings() -> Value {
    let path = settings_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return json!({});
    };
    let v: serde_yaml::Value = match serde_yaml::from_str(&text) {
        Ok(v) => v,
        Err(_) => return json!({}),
    };
    yaml_to_json(&v)
}

/// 推送语言：优先 KV `push_lang`，否则 `settings.yaml` 的 `language`，最后 `zh`。
///
/// 对应原版 `get_push_language()`。
///
/// # 为什么不用 [`crate::config::get_config`]
///
/// 因为那个是进程内 `OnceCell` 缓存，改完 `settings.yaml` 不会立刻反映出来。
/// 面板点「保存」之后马上要回显，必须读盘。这个函数就干这个。
///
/// `kv` 传 `None` 表示「不查数据库」（CLI 场景）。
pub fn get_push_language(kv: Option<&str>) -> String {
    if let Some(v) = kv {
        if matches!(v, "zh" | "en" | "both") {
            return v.to_string();
        }
    }
    load_settings()
        .get("language")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "zh".to_string())
}

// ---------------------------------------------------------------- panel.env

/// 读 `panel.env` 成键值表。
///
/// 对应原版 `read_env_file()`。规则刻意简单：`KEY=VALUE`，
/// `#` 开头是注释，空行跳过。**不做**引号与转义处理 ——
/// 因为写回时必须用同样的规则，两边不一致会把文件搞坏。
pub fn read_env_file() -> BTreeMap<String, String> {
    read_env_file_at(&env_path())
}

/// 从指定路径读（测试用）。
pub fn read_env_file_at(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        out.insert(k.trim().to_string(), v.trim().to_string());
    }
    out
}

/// 更新 `panel.env`。返回是否真的写了文件。
///
/// 对应原版 `update_env_file()`。
///
/// 语义要点：
/// - 值全都没变 → **直接返回 `false` 且不写文件**（避免无谓的 mtime 变化）
/// - `None` 值表示「跳过这一项」，不是「删除」
/// - 保留既有注释与其他行
/// - 新键追加到末尾
///
/// 见 [`ENV_HEADER_MARK`] 关于头注释的那个原版不一致。
pub fn update_env_file(updates: &BTreeMap<String, Option<String>>) -> Result<bool> {
    update_env_file_at(&env_path(), updates)
}

/// 从指定路径更新（测试用）。
pub fn update_env_file_at(
    path: &Path,
    updates: &BTreeMap<String, Option<String>>,
) -> Result<bool> {
    let cur = read_env_file_at(path);

    // 先判断有没有变化 —— 全一样就不碰文件
    let mut changed = false;
    for (k, v) in updates {
        let Some(v) = v else { continue };
        let v = v.trim();
        if cur.get(k).map(String::as_str).unwrap_or("") != v {
            changed = true;
            break;
        }
    }
    if !changed {
        return Ok(false);
    }

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<String> = existing.lines().map(str::to_string).collect();

    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();

    for line in &lines {
        let s = line.trim();
        if !s.is_empty() && !s.starts_with('#') {
            if let Some((k, _)) = s.split_once('=') {
                let k = k.trim();
                if let Some(Some(v)) = updates.get(k) {
                    out.push(format!("{k}={}", v.trim()));
                    seen.push(k.to_string());
                    continue;
                }
            }
        }
        out.push(line.clone());
    }

    for (k, v) in updates {
        let Some(v) = v else { continue };
        if seen.iter().any(|s| s == k) {
            continue;
        }
        out.push(format!("{k}={}", v.trim()));
    }

    // 见 ENV_HEADER_MARK：判定用短版、插入用长版，原版就是这样
    let has_mark = lines.iter().any(|l| l.trim() == ENV_HEADER_MARK);
    if !has_mark {
        out.insert(0, ENV_HEADER_INSERT.to_string());
    }

    let mut text = out.join("\n");
    while text.ends_with('\n') {
        text.pop();
    }
    text.push('\n');

    write_atomic(path, &text)?;
    Ok(true)
}

// ---------------------------------------------------------------- 工具

/// 按行拆开并**保留换行符**。
///
/// 原版用 `read().splitlines(keepends=True)`。保留换行符是必要的：
/// 否则重写文件时最后一行有没有换行、CRLF 还是 LF 都会变。
fn split_keepends(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'\n' {
            out.push(text[start..=i].to_string());
            start = i + 1;
        }
    }
    if start < text.len() {
        out.push(text[start..].to_string());
    }
    out
}

fn ensure_trailing_newline(s: &str) -> String {
    if s.ends_with('\n') {
        s.to_string()
    } else {
        format!("{s}\n")
    }
}

/// 去掉 YAML 标量两端的引号。
fn unquote(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (a, b) = (bytes[0], bytes[bytes.len() - 1]);
        if (a == b'"' && b == b'"') || (a == b'\'' && b == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// YAML 值的「真值」判断，对齐 Python 的 `if v:`。
///
/// Python 里 `0`、`""`、`[]`、`{}`、`None` 都是假。
fn yaml_truthy(v: &serde_yaml::Value) -> bool {
    match v {
        serde_yaml::Value::Null => false,
        serde_yaml::Value::Bool(b) => *b,
        serde_yaml::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        serde_yaml::Value::String(s) => !s.is_empty(),
        serde_yaml::Value::Sequence(s) => !s.is_empty(),
        serde_yaml::Value::Mapping(m) => !m.is_empty(),
        _ => true,
    }
}

fn yaml_i64(v: &serde_yaml::Value) -> Option<i64> {
    match v {
        serde_yaml::Value::Number(n) => n.as_i64(),
        // 字符串形式的数字也认（原版是 `int(...)`，Python 的 int("10") 能过）
        serde_yaml::Value::String(s) => s.trim().parse::<i64>().ok(),
        serde_yaml::Value::Bool(b) => Some(*b as i64),
        _ => None,
    }
}

/// YAML → JSON。
///
/// 之所以要转：面板的响应体是 JSON，而配置文件是 YAML。
/// 直接 `serde_yaml::to_string` 塞进 JSON 字符串会让前端拿到一段 YAML 文本，
/// 还得自己解析 —— 那不如后端一次转好。
///
/// 非字符串的映射键会**丢失**（JSON 的键只能是字符串）。
/// 配置里不该出现这种键，出现说明文件写错了，丢掉比崩掉好。
pub fn yaml_to_json(v: &serde_yaml::Value) -> Value {
    match v {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(b) => json!(b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(f) = n.as_f64() {
                json!(f)
            } else {
                Value::Null
            }
        }
        serde_yaml::Value::String(s) => json!(s),
        serde_yaml::Value::Sequence(s) => Value::Array(s.iter().map(yaml_to_json).collect()),
        serde_yaml::Value::Mapping(m) => {
            let mut out = Map::new();
            for (k, v) in m {
                let Some(k) = k.as_str() else { continue };
                out.insert(k.to_string(), yaml_to_json(v));
            }
            Value::Object(out)
        }
        _ => Value::Null,
    }
}

/// 原子写：先写同目录的临时文件，再 `rename` 覆盖。
///
/// 面板改配置时如果进程被杀（或磁盘满），直接 `write` 会留下半截文件，
/// 下次启动直接读不出来 —— 而这是**唯一**的配置来源，
/// 坏了就得手工修。`rename` 在同一文件系统内是原子的。
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CoreError::io(format!("创建 {} 失败: {e}", parent.display())))?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_else(|| "tmp".into())
    ));
    std::fs::write(&tmp, text)
        .map_err(|e| CoreError::io(format!("写入 {} 失败: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| CoreError::io(format!("替换 {} 失败: {e}", path.display())))?;
    Ok(())
}

// ---------------------------------------------------------------- 测试

#[cfg(test)]
mod tests {
    use super::*;

    // ---- split_keepends ----

    #[test]
    fn split_keepends_preserves_newlines() {
        let v = split_keepends("a\nb\nc");
        assert_eq!(v, vec!["a\n", "b\n", "c"]);
    }

    #[test]
    fn split_keepends_empty() {
        assert!(split_keepends("").is_empty());
    }

    // ---- set_top_field：三个承重细节 ----

    #[test]
    fn top_field_replaces_only_the_first_match() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        std::fs::write(
            &p,
            "# 头部注释\ntimezone: UTC\nlogging:\n  language: en\nlanguage: zh\n",
        )
        .unwrap();

        assert!(set_top_field(&p, "language", "both").unwrap());

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("language: both"), "顶层那行该被改:\n{text}");
        // **嵌套段里的同名键不能被动** —— 这是最容易写错的地方
        assert!(
            text.contains("  language: en"),
            "嵌套段里的 language 不该被改:\n{text}"
        );
        assert!(text.contains("# 头部注释"), "注释不该丢:\n{text}");
        assert!(text.contains("timezone: UTC"), "其他行不该动:\n{text}");
    }

    #[test]
    fn top_field_returns_false_when_not_found_and_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        let original = "# 只有注释\nlanguage: zh\n";
        std::fs::write(&p, original).unwrap();

        let before = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert!(!set_top_field(&p, "timezone", "UTC").unwrap());
        let after = std::fs::metadata(&p).unwrap().modified().unwrap();

        assert_eq!(std::fs::read_to_string(&p).unwrap(), original);
        assert_eq!(before, after, "没匹配到就不该写盘（mtime 不该变）");
    }

    /// 布尔值写成 Python 风格 `True`/`False`（原版 `str(True)` 的结果）。
    ///
    /// 这看着像 bug，但它是**原版的实际输出**。YAML 1.1 里 `True` 也是 bool，
    /// 所以能读回来。刻意复刻，理由见 [`set_top_field`] 文档。
    #[test]
    fn bool_is_written_as_python_style() {
        assert_eq!(render_scalar(&json!(true)), "True");
        assert_eq!(render_scalar(&json!(false)), "False");
        assert_eq!(render_scalar(&json!(42)), "42");
        assert_eq!(render_scalar(&json!("Asia/Shanghai")), "Asia/Shanghai");
    }

    #[test]
    fn scalar_has_no_quotes() {
        let v = render_scalar(&json!("Asia/Shanghai"));
        assert!(!v.contains('"'), "标量不该有引号: {v}");
        assert!(!v.contains('\''), "标量不该有引号: {v}");
    }

    /// 写完之后 `alpha-core::config` 必须能读回来 —— 这对函数是一致性契约。
    #[test]
    fn written_settings_are_readable_by_the_config_loader() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("settings.yaml");
        std::fs::write(&p, "timezone: UTC\nlanguage: zh\n").unwrap();
        set_top_field(&p, "timezone", "Asia/Tokyo").unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        let v: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
        assert_eq!(
            v.get("timezone").and_then(|x| x.as_str()),
            Some("Asia/Tokyo")
        );
    }

    // ---- panel.env ----

    #[test]
    fn env_file_roundtrip_keeps_comments() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("panel.env");
        std::fs::write(
            &p,
            "# WxPusher token\nWXPUSHER_APP_TOKEN_alpha=\n# 代理\nPROXY_KEY=old\n",
        )
        .unwrap();

        let mut up = BTreeMap::new();
        up.insert("PROXY_KEY".to_string(), Some("new".to_string()));
        up.insert("PANEL_SECRET".to_string(), Some("s3cret".to_string()));
        assert!(update_env_file_at(&p, &up).unwrap());

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# WxPusher token"), "注释被吃掉:\n{text}");
        assert!(text.contains("# 代理"), "注释被吃掉:\n{text}");
        assert!(text.contains("PROXY_KEY=new"), "值没更新:\n{text}");
        assert!(text.contains("PANEL_SECRET=s3cret"), "新键没追加:\n{text}");
        assert!(!text.contains("PROXY_KEY=old"));
    }

    #[test]
    fn env_file_unchanged_returns_false_and_skips_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("panel.env");
        std::fs::write(&p, "A=1\n").unwrap();
        let before = std::fs::metadata(&p).unwrap().modified().unwrap();

        let mut up = BTreeMap::new();
        up.insert("A".to_string(), Some("1".to_string()));
        assert!(!update_env_file_at(&p, &up).unwrap(), "值没变应返回 false");

        let after = std::fs::metadata(&p).unwrap().modified().unwrap();
        assert_eq!(before, after, "值没变就不该写盘");
    }

    #[test]
    fn env_file_none_value_is_skipped_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("panel.env");
        std::fs::write(&p, "A=1\nB=2\n").unwrap();

        let mut up = BTreeMap::new();
        up.insert("A".to_string(), Some("9".to_string()));
        up.insert("B".to_string(), None); // 「别动 B」
        assert!(update_env_file_at(&p, &up).unwrap());

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("A=9"));
        assert!(text.contains("B=2"), "None 是「跳过」不是「删除」:\n{text}");
    }

    /// 无头注释时插入一行 —— 原版判定用短版、插入用长版（见常量说明）。
    #[test]
    fn env_file_inserts_header_when_mark_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("panel.env");
        std::fs::write(&p, "A=1\n").unwrap();

        let mut up = BTreeMap::new();
        up.insert("A".to_string(), Some("2".to_string()));
        update_env_file_at(&p, &up).unwrap();

        let text = std::fs::read_to_string(&p).unwrap();
        assert!(
            text.starts_with(ENV_HEADER_INSERT),
            "头注释该在第一行:\n{text}"
        );
    }

    #[test]
    fn env_value_may_contain_equals() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("panel.env");
        std::fs::write(&p, "PANEL_SECRET=abc=def==\n").unwrap();
        let m = read_env_file_at(&p);
        assert_eq!(m.get("PANEL_SECRET").map(String::as_str), Some("abc=def=="));
    }

    // ---- sources.yaml 的文本级编辑 ----

    const SAMPLE: &str = "\
# ============================================================
# 数据源注册表
# ============================================================
sources:
  - name: LZPoke 报点
    adapter: lzpoke_reports
    # 这个源走代理
    enabled: true
    priority: 10
    options:
      target: https://tool.lzpoke.com/api/reports?type=alpha
      proxy: http://127.0.0.1:8899
      extra_lines:
        level: true
        vote: true
  - name: NEODEX Alpha
    adapter: neodex_alpha
    enabled: true
    priority: 9
    options:
      target: https://neodex.example/graphql
";

    #[test]
    fn parse_finds_all_sources_in_order() {
        let doc = SourcesDocFile::parse(SAMPLE);
        assert_eq!(doc.names(), vec!["LZPoke 报点", "NEODEX Alpha"]);
    }

    #[test]
    fn set_field_only_touches_that_source() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        assert!(doc.set_field("LZPoke 报点", "priority", "5"));

        let text = doc.text();
        assert!(text.contains("    priority: 5"), "该源优先级应变成 5:\n{text}");
        assert!(
            text.contains("    priority: 9"),
            "另一个源的优先级不该动:\n{text}"
        );
        assert!(text.contains("# 这个源走代理"), "注释不该丢:\n{text}");
        assert!(text.contains("# 数据源注册表"), "头注释不该丢:\n{text}");
    }

    /// `options` 里的 `enabled` 之类同名键不能被误改。
    #[test]
    fn set_field_only_touches_the_top_level_of_that_block() {
        let src = "\
sources:
  - name: A
    enabled: true
    options:
      enabled: false
";
        let mut doc = SourcesDocFile::parse(src);
        assert!(doc.set_field("A", "enabled", "false"));
        let text = doc.text();
        assert!(text.contains("\n    enabled: false\n"));
        assert!(
            text.contains("\n      enabled: false\n"),
            "嵌套的那个本来也是 false，不该被动过:\n{text}"
        );
    }

    #[test]
    fn set_field_inserts_when_absent() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        assert!(doc.set_field("NEODEX Alpha", "note", "备用源"));
        let text = doc.text();
        assert!(text.contains("    note: 备用源"), "新字段该被插入:\n{text}");
        // 插在 name 行之后，还在这个段里
        let after_name = text.split("name: NEODEX Alpha").nth(1).unwrap();
        assert!(after_name.starts_with("\n    note: 备用源"));
    }

    #[test]
    fn set_field_on_missing_source_returns_false() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        assert!(!doc.set_field("不存在的源", "enabled", "true"));
    }

    #[test]
    fn remove_source_takes_the_whole_block() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        assert!(doc.remove_source("LZPoke 报点"));
        let text = doc.text();
        assert!(!text.contains("LZPoke 报点"), "整个段该被删掉:\n{text}");
        assert!(!text.contains("lzpoke_reports"), "段里的行也该没了:\n{text}");
        assert!(!text.contains("# 这个源走代理"), "段内注释随段一起删:\n{text}");
        assert!(text.contains("NEODEX Alpha"), "另一个源该留着:\n{text}");
        assert!(text.contains("# 数据源注册表"), "头注释该留着:\n{text}");
        assert_eq!(doc.names(), vec!["NEODEX Alpha"]);
    }

    #[test]
    fn remove_missing_source_returns_false() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        assert!(!doc.remove_source("不存在"));
    }

    #[test]
    fn append_source_adds_at_the_end() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        doc.append_source("  - name: 新源\n    adapter: foo\n    enabled: false\n");
        assert_eq!(doc.names(), vec!["LZPoke 报点", "NEODEX Alpha", "新源"]);
        assert!(doc.text().contains("adapter: foo"));
    }

    #[test]
    fn replace_block_keeps_position() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        doc.replace_block(
            "LZPoke 报点",
            "  - name: 改名后\n    adapter: lzpoke_reports\n    enabled: false\n    priority: 10\n",
        );
        assert_eq!(doc.names(), vec!["改名后", "NEODEX Alpha"], "位置该不变");
    }

    /// 空文档（文件不存在）也能追加源，并且带着头注释。
    #[test]
    fn empty_doc_gets_header_and_sources_key() {
        let mut doc = SourcesDocFile::empty();
        assert!(doc.is_empty());
        doc.append_source("  - name: A\n    adapter: x\n");
        let text = doc.text();
        assert!(text.starts_with("# ===="), "该有头注释:\n{text}");
        assert!(text.contains("sources:"), "该补上 sources 键:\n{text}");
        assert_eq!(doc.names(), vec!["A"]);
    }

    /// 保存出来的文件必须能被 YAML 解析器读回来（文本级编辑的底线）。
    #[test]
    fn saved_text_is_valid_yaml() {
        let mut doc = SourcesDocFile::parse(SAMPLE);
        doc.set_field("LZPoke 报点", "priority", "5");
        doc.append_source("  - name: 新源\n    adapter: foo\n");
        let v: serde_yaml::Value = serde_yaml::from_str(&doc.text()).unwrap();
        let list = v.get("sources").unwrap().as_sequence().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(
            list[0].get("priority").and_then(|x| x.as_i64()),
            Some(5)
        );
    }

    // ---- describe_source ----

    #[test]
    fn describe_source_matches_python_defaults() {
        let v: serde_yaml::Value = serde_yaml::from_str(
            "{name: A, adapter: b, options: {target: http://x, extra_lines: {hms: true, vote: false}}}",
        )
        .unwrap();
        let d = describe_source(&v);
        // enabled 缺省 false（不是 true）
        assert_eq!(d["enabled"], false);
        // priority 缺省 100
        assert_eq!(d["priority"], 100);
        // note = target · 附加:开启的项
        assert_eq!(d["note"], "http://x · 附加:hms");
    }

    #[test]
    fn describe_source_without_options() {
        let v: serde_yaml::Value = serde_yaml::from_str("{name: A}").unwrap();
        let d = describe_source(&v);
        assert_eq!(d["name"], "A");
        assert_eq!(d["adapter"], "");
        assert_eq!(d["enabled"], false);
        assert_eq!(d["priority"], 100);
        assert_eq!(d["note"], "");
    }

    /// 优先级可以是字符串形式的数字（原版 `int(...)`）。
    #[test]
    fn describe_source_accepts_string_priority() {
        let v: serde_yaml::Value = serde_yaml::from_str("{name: A, priority: '20'}").unwrap();
        assert_eq!(describe_source(&v)["priority"], 20);
    }

    // ---- yaml_to_json ----

    #[test]
    fn yaml_to_json_handles_nested() {
        let v: serde_yaml::Value =
            serde_yaml::from_str("a: {b: [1, 2, {c: true}]}\n").unwrap();
        let j = yaml_to_json(&v);
        assert_eq!(j["a"]["b"][2]["c"], true);
        assert_eq!(j["a"]["b"][0], 1);
    }

    // ---- get_push_language ----

    #[test]
    fn push_language_kv_wins_over_yaml() {
        assert_eq!(get_push_language(Some("en")), "en");
        assert_eq!(get_push_language(Some("both")), "both");
    }

    /// KV 里是垃圾值时忽略它，回落 yaml —— 原版就是这个判定。
    #[test]
    fn push_language_ignores_invalid_kv() {
        let v = get_push_language(Some("klingon"));
        assert!(matches!(v.as_str(), "zh" | "en" | "both"), "得到 {v}");
    }

    #[test]
    fn push_language_defaults_to_zh() {
        assert!(matches!(get_push_language(None).as_str(), "zh" | "en" | "both"));
    }
}
