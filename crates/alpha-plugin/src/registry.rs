//! 插件目录扫描与加载。
//!
//! # 目录布局
//!
//! ```text
//! data/plugins/
//!   dispatchers/        ← 决策器
//!     default_dispatcher.rhai
//!     example_dispatcher.rhai
//!   evaluators/         ← 评估器
//!     script_team.rhai
//! ```
//!
//! 与原版 `panel/dispatchers/`、`panel/evaluators/` 一样的组织方式，
//! 只是扩展名从 `.py` 换成 `.rhai`。
//!
//! # 为什么要扫描而不是只从数据库读
//!
//! 数据库里的 `filename` 列指向磁盘上的文件。两边会不一致：
//!
//! - 用户手工把 `.rhai` 丢进目录（没走上传接口）
//! - 用户手工删了文件（数据库里还留着记录）
//! - 上传接口写完文件、写数据库前进程崩了
//!
//! 所以 [`PluginRegistry::scan`] 以**磁盘为准**，返回「目录里实际有什么」，
//! 由上层（`alpha-server`）去跟数据库对账。这样任何一边手工改动都能被发现，
//! 而不是表现出来「面板里有个插件，点了没反应」。
//!
//! # 热更新
//!
//! 按 `(路径, 修改时间)` 判断是否需要重新编译。面板轮询是每 60 秒一次，
//! 每次都重新编译全部插件纯属浪费 —— 而编译恰恰是这层最贵的操作。
//! 缓存后，只有真的改过文件才会付编译成本。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use crate::error::{PluginError, PluginResult};
use crate::sandbox::{Plugin, PluginKind};

/// 允许上传的扩展名。
///
/// 只有 `.rhai`。原版什么都能传（`.py` 即可），Rust 版收紧到一种 ——
/// 白名单比黑名单可靠得多，也省得去拦 `../../etc/passwd` 这类文件名。
pub const UPLOAD_EXTENSIONS: &[&str] = &["rhai"];

/// 磁盘上的一个插件文件。
#[derive(Debug, Clone)]
pub struct PluginFile {
    /// 文件名（不含目录），对应数据库 `filename` 列
    pub filename: String,
    pub kind: PluginKind,
    pub path: PathBuf,
    /// 文件大小（字节）
    pub size: u64,
    /// 最后修改时间
    pub modified: Option<SystemTime>,
}

impl PluginFile {
    /// 用脚本元信息或文件名兜底出的显示名。
    pub fn display_name(&self, plugin: &Plugin) -> String {
        let n = plugin.manifest().name.trim();
        if n.is_empty() {
            // 去掉扩展名当名字，比显示 `foo.rhai` 好看
            self.filename
                .rsplit_once('.')
                .map(|(stem, _)| stem.to_string())
                .unwrap_or_else(|| self.filename.clone())
        } else {
            n.to_string()
        }
    }
}

/// 一个已加载（或加载失败）的插件条目。
pub struct Loaded {
    /// 源文件信息
    pub file: PluginFile,
    /// 编译结果：`Ok` 可用，`Err` 是给用户看的报错
    pub plugin: Result<Arc<Plugin>, String>,
    /// 编译时用的修改时间（用于判断缓存是否失效）
    cache_key: Option<SystemTime>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("filename", &self.file.filename)
            .field("kind", &self.file.kind)
            .field("ok", &self.plugin.is_ok())
            .finish_non_exhaustive()
    }
}

impl Loaded {
    pub fn is_ok(&self) -> bool {
        self.plugin.is_ok()
    }

    /// 取插件；加载失败时把错误信息原样抛出。
    pub fn get(&self) -> PluginResult<&Arc<Plugin>> {
        self.plugin
            .as_ref()
            .map_err(|e| PluginError::Compile(e.clone()))
    }
}

/// 插件注册表：扫描目录 + 编译缓存。
///
/// 内部用 `RwLock`：读（每次推送取插件）远多于写（只有文件变了才写）。
#[derive(Default)]
pub struct PluginRegistry {
    /// 根目录（`data/plugins`）
    root: PathBuf,
    /// 已加载的插件，按 `filename` 索引
    entries: RwLock<HashMap<String, Arc<Loaded>>>,
}

impl std::fmt::Debug for PluginRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginRegistry")
            .field("root", &self.root)
            .field("count", &self.entries.read().map(|e| e.len()).unwrap_or(0))
            .finish_non_exhaustive()
    }
}

impl PluginRegistry {
    /// 新建注册表并创建目录结构。
    pub fn new(root: impl AsRef<Path>) -> PluginResult<Self> {
        let root = root.as_ref().to_path_buf();
        for kind in [PluginKind::Dispatcher, PluginKind::Evaluator] {
            let dir = root.join(dir_of(kind));
            std::fs::create_dir_all(&dir).map_err(|e| PluginError::Io {
                path: dir.display().to_string(),
                source: e,
            })?;
        }
        Ok(Self {
            root,
            entries: RwLock::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 某个类别的目录。
    pub fn dir(&self, kind: PluginKind) -> PathBuf {
        self.root.join(dir_of(kind))
    }

    /// 某个插件的完整路径。
    pub fn path_of(&self, kind: PluginKind, filename: &str) -> PathBuf {
        self.dir(kind).join(filename)
    }

    /// 扫描某个类别的目录，返回磁盘上的文件列表。
    ///
    /// 只认 `.rhai`，其它扩展名（包括原版的 `.py`）会被忽略 ——
    /// 面板上会有个提示告诉用户「旧插件需要改写」。
    pub fn scan(&self, kind: PluginKind) -> PluginResult<Vec<PluginFile>> {
        let dir = self.dir(kind);
        let rd = std::fs::read_dir(&dir).map_err(|e| PluginError::Io {
            path: dir.display().to_string(),
            source: e,
        })?;

        let mut out = Vec::new();
        for entry in rd {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let filename = match path.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue, // 非 UTF-8 文件名，跳过
            };
            if !is_allowed_filename(&filename) {
                continue;
            }
            let meta = entry.metadata().ok();
            out.push(PluginFile {
                filename,
                kind,
                path,
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta.and_then(|m| m.modified().ok()),
            });
        }

        // 排序让面板列表稳定（否则每次刷新顺序都可能变）
        out.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(out)
    }

    /// 扫描并加载某个类别的全部插件，返回加载结果。
    ///
    /// 单个插件加载失败**不影响**其它的 —— 用户传了个写坏的脚本，
    /// 不该让面板上其它插件一起消失。
    pub fn load_all(&self, kind: PluginKind) -> PluginResult<Vec<Arc<Loaded>>> {
        let files = self.scan(kind)?;
        let mut out = Vec::with_capacity(files.len());
        for f in files {
            out.push(self.load_one(f)?);
        }
        Ok(out)
    }

    /// 加载单个文件（带缓存）。
    pub fn load_one(&self, file: PluginFile) -> PluginResult<Arc<Loaded>> {
        // 缓存命中：文件没改过就直接复用
        if let Some(cached) = self.cached(&file) {
            return Ok(cached);
        }

        let result = Plugin::load_file(&file.path, file.kind).map(Arc::new);
        let plugin = match result {
            Ok(p) => Ok(p),
            Err(e) => {
                tracing::warn!(
                    "插件 {} 加载失败: {e}",
                    file.path.display()
                );
                Err(e.to_string())
            }
        };

        let loaded = Arc::new(Loaded {
            cache_key: file.modified,
            file: file.clone(),
            plugin,
        });

        if let Ok(mut map) = self.entries.write() {
            map.insert(file.filename.clone(), loaded.clone());
        }
        Ok(loaded)
    }

    /// 按文件名取已缓存的插件；未缓存或已过期则返回 `None`。
    fn cached(&self, file: &PluginFile) -> Option<Arc<Loaded>> {
        let map = self.entries.read().ok()?;
        let e = map.get(&file.filename)?;
        // 修改时间一致才算命中
        if e.cache_key == file.modified && e.file.path == file.path {
            Some(e.clone())
        } else {
            None
        }
    }

    /// 取已缓存的插件（不触发编译）。
    pub fn get(&self, filename: &str) -> Option<Arc<Loaded>> {
        self.entries.read().ok()?.get(filename).cloned()
    }

    /// 按文件名加载一个插件（**会**触发编译，走 `modified` 缓存）。
    ///
    /// 与 [`get`](Self::get) 的区别：「我想拿到这个文件对应的插件」
    /// 而不是「它是不是已经在缓存里了」。文件不存在时返回 `Ok(None)`。
    ///
    /// 路由层直接用这个 —— 它只持有数据库里的 `filename` 字符串，
    /// 没必要为了拿一个 `PluginFile` 去 `scan` 整个目录。
    pub fn load_by_filename(
        &self,
        kind: PluginKind,
        filename: &str,
    ) -> PluginResult<Option<Arc<Loaded>>> {
        let path = self.path_of(kind, filename);
        if !path.exists() {
            return Ok(None);
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let file = PluginFile {
            filename: filename.to_string(),
            kind,
            path,
            size,
            modified,
        };
        self.load_one(file).map(Some)
    }

    /// 把内建插件写入磁盘（已存在则不覆盖）。
    ///
    /// 不覆盖是有意的：用户可能已经按自己的需要改过内建插件的源码，
    /// 升级时把它们冲掉是数据丢失。
    pub fn materialize_builtins(&self) -> PluginResult<Vec<PathBuf>> {
        let mut written = Vec::new();
        for b in crate::builtins::BUILTINS {
            let path = self.path_of(b.kind, b.filename);
            if path.exists() {
                continue;
            }
            std::fs::write(&path, b.source).map_err(|e| PluginError::Io {
                path: path.display().to_string(),
                source: e,
            })?;
            written.push(path);
        }
        Ok(written)
    }

    /// 清掉缓存（强制下次重新编译）。
    pub fn invalidate(&self) {
        if let Ok(mut m) = self.entries.write() {
            m.clear();
        }
    }

    /// 校验收到的上传文件名并返回规范化结果。
    ///
    /// # 为什么这么小心
    ///
    /// 文件名来自 HTTP 请求，是**不可信输入**。原版直接
    /// `os.path.join(EVAL_DIR, filename)`，一个 `../../panel/panel.db`
    /// 就能指到库里任何位置 —— 配合「上传即写入」，等于任意文件覆盖。
    ///
    /// 这里的规则：
    /// - 不允许路径分隔符（`/`、`\`）
    /// - 不允许 `..`
    /// - 不允许以 `.` 开头（隐藏文件 / `.`、`..`）
    /// - 必须是指定扩展名
    pub fn validate_filename(name: &str) -> PluginResult<String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(PluginError::Rejected("文件名不能为空".into()));
        }
        if name.contains('/') || name.contains('\\') {
            return Err(PluginError::Rejected(format!(
                "文件名不能包含路径分隔符: {name}"
            )));
        }
        if name.contains("..") {
            return Err(PluginError::Rejected(format!("文件名不能包含 `..`: {name}")));
        }
        if name.starts_with('.') {
            return Err(PluginError::Rejected(format!(
                "文件名不能以 `.` 开头: {name}"
            )));
        }
        // Windows 保留字符与空字节——上传上来的名字可能带这些
        if name.contains('\0') || name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*')) {
            return Err(PluginError::Rejected(format!("文件名含非法字符: {name}")));
        }
        if !is_allowed_filename(name) {
            return Err(PluginError::Rejected(format!(
                "只支持 {} 扩展名: {name}",
                UPLOAD_EXTENSIONS.join(" / ")
            )));
        }
        // 长度限制：主流文件系统 255 字节，留点余量给前缀
        if name.len() > 200 {
            return Err(PluginError::Rejected(format!(
                "文件名过长（{} 字节，上限 200）: {name}",
                name.len()
            )));
        }
        Ok(name.to_string())
    }
}

/// 类别对应的子目录名。
/// 插件类型对应的子目录名（`dispatchers` / `evaluators`）。
pub fn dir_of(kind: PluginKind) -> &'static str {
    match kind {
        PluginKind::Dispatcher => "dispatchers",
        PluginKind::Evaluator => "evaluators",
    }
}

/// 扩展名是否在白名单内（大小写不敏感）。
fn is_allowed_filename(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return false;
    };
    UPLOAD_EXTENSIONS
        .iter()
        .any(|a| ext.eq_ignore_ascii_case(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> (tempfile::TempDir, PluginRegistry) {
        let dir = tempfile::tempdir().unwrap();
        let r = PluginRegistry::new(dir.path()).unwrap();
        (dir, r)
    }

    #[test]
    fn creates_subdirectories() {
        let (_d, r) = reg();
        assert!(r.dir(PluginKind::Dispatcher).is_dir());
        assert!(r.dir(PluginKind::Evaluator).is_dir());
    }

    /// 写进磁盘的内建插件必须能被扫描到并加载成功。
    #[test]
    fn materializes_and_loads_builtins() {
        let (_d, r) = reg();
        let written = r.materialize_builtins().unwrap();
        assert_eq!(written.len(), 2, "应写入两个内建插件");

        let disp = r.load_all(PluginKind::Dispatcher).unwrap();
        assert_eq!(disp.len(), 1);
        assert!(disp[0].is_ok(), "内建决策器应加载成功: {:?}", disp[0].plugin);
        assert_eq!(disp[0].file.filename, "default_dispatcher.rhai");

        let ev = r.load_all(PluginKind::Evaluator).unwrap();
        assert_eq!(ev.len(), 1);
        assert!(ev[0].is_ok());
        assert_eq!(ev[0].file.filename, "script_team.rhai");
    }

    /// 再次调用不能覆盖用户改过的内建插件 —— 那是数据丢失。
    #[test]
    fn materialize_does_not_overwrite_user_edits() {
        let (_d, r) = reg();
        r.materialize_builtins().unwrap();

        let path = r.path_of(PluginKind::Evaluator, "script_team.rhai");
        std::fs::write(&path, "// 用户改过的\nfn evaluate(b, c) { () }").unwrap();

        let again = r.materialize_builtins().unwrap();
        assert!(again.is_empty(), "不该重复写入");
        let now = std::fs::read_to_string(&path).unwrap();
        assert!(now.contains("用户改过的"), "用户的修改被覆盖了");
    }

    #[test]
    fn scan_ignores_non_rhai_files() {
        let (_d, r) = reg();
        let dir = r.dir(PluginKind::Evaluator);
        std::fs::write(dir.join("good.rhai"), "fn evaluate(b,c){ () }").unwrap();
        std::fs::write(dir.join("legacy.py"), "def evaluate(b,c): pass").unwrap();
        std::fs::write(dir.join("notes.txt"), "hello").unwrap();

        let files = r.scan(PluginKind::Evaluator).unwrap();
        assert_eq!(files.len(), 1, "只应认 .rhai: {files:?}");
        assert_eq!(files[0].filename, "good.rhai");
    }

    #[test]
    fn scan_is_sorted_for_stable_ui() {
        let (_d, r) = reg();
        let dir = r.dir(PluginKind::Evaluator);
        for n in ["c.rhai", "a.rhai", "b.rhai"] {
            std::fs::write(dir.join(n), "fn evaluate(b,c){ () }").unwrap();
        }
        let names: Vec<String> = r
            .scan(PluginKind::Evaluator)
            .unwrap()
            .into_iter()
            .map(|f| f.filename)
            .collect();
        assert_eq!(names, vec!["a.rhai", "b.rhai", "c.rhai"]);
    }

    /// 一个插件写坏了，不能连累其它插件 ——
    /// 面板上应该只标记那一个为错误状态。
    #[test]
    fn broken_plugin_does_not_break_others() {
        let (_d, r) = reg();
        let dir = r.dir(PluginKind::Evaluator);
        std::fs::write(dir.join("a_ok.rhai"), "fn evaluate(b,c){ () }").unwrap();
        std::fs::write(dir.join("b_broken.rhai"), "fn evaluate(b,c { ").unwrap();
        std::fs::write(dir.join("c_ok.rhai"), "fn evaluate(b,c){ () }").unwrap();

        let all = r.load_all(PluginKind::Evaluator).unwrap();
        assert_eq!(all.len(), 3);
        let ok: Vec<bool> = all.iter().map(|l| l.is_ok()).collect();
        assert_eq!(ok, vec![true, false, true], "只有中间那个该失败");

        // 失败的那个要带可读的报错
        let bad = all.iter().find(|l| !l.is_ok()).unwrap();
        assert!(bad.get().is_err());
    }

    /// 编译缓存要真的生效：文件没改就不重新编译。
    #[test]
    fn caches_compiled_plugins() {
        let (_d, r) = reg();
        let dir = r.dir(PluginKind::Evaluator);
        let p = dir.join("cached.rhai");
        std::fs::write(&p, "fn evaluate(b,c){ () }").unwrap();

        let first = r.load_all(PluginKind::Evaluator).unwrap();
        let second = r.load_all(PluginKind::Evaluator).unwrap();
        assert!(
            Arc::ptr_eq(&first[0], &second[0]),
            "未改动时应命中缓存（复用同一个 Arc）"
        );
    }

    /// 文件改了要重新编译 —— 否则用户上传新版本后看不到效果。
    #[test]
    fn invalidates_cache_when_file_changes() {
        let (_d, r) = reg();
        let dir = r.dir(PluginKind::Evaluator);
        let p = dir.join("changing.rhai");
        std::fs::write(&p, "fn evaluate(b,c){ () }").unwrap();
        let first = r.load_all(PluginKind::Evaluator).unwrap();

        // 改内容并确保 mtime 变化（有些文件系统时间戳精度低）
        std:: thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&p, "// @name 新版\nfn evaluate(b,c){ () }").unwrap();

        let second = r.load_all(PluginKind::Evaluator).unwrap();
        assert!(
            !Arc::ptr_eq(&first[0], &second[0]),
            "文件改过应重新编译"
        );
        assert_eq!(second[0].get().unwrap().manifest().name, "新版");
    }

    // ------------------------------------------------ 文件名校验

    /// 路径穿越必须被拦住 —— 这是原版最严重的一个洞。
    #[test]
    fn rejects_path_traversal() {
        for bad in [
            "../../panel.db",
            "../evil.rhai",
            "a/../../b.rhai",
            "sub/dir.rhai",
            "..\\windows.rhai",
            "/etc/passwd.rhai",
        ] {
            assert!(
                PluginRegistry::validate_filename(bad).is_err(),
                "`{bad}` 应被拒绝"
            );
        }
    }

    #[test]
    fn rejects_wrong_extension() {
        for bad in ["evil.py", "a.sh", "noext", "a.rhai.py", "a.rhaix"] {
            assert!(
                PluginRegistry::validate_filename(bad).is_err(),
                "`{bad}` 应被拒绝"
            );
        }
    }

    #[test]
    fn rejects_hidden_and_empty_names() {
        assert!(PluginRegistry::validate_filename("").is_err());
        assert!(PluginRegistry::validate_filename("   ").is_err());
        assert!(PluginRegistry::validate_filename(".hidden.rhai").is_err());
        assert!(PluginRegistry::validate_filename("..rhai").is_err());
    }

    #[test]
    fn rejects_special_characters() {
        for bad in ["a<b.rhai", "a>b.rhai", "a:b.rhai", "a|b.rhai", "a?b.rhai", "a*b.rhai"] {
            assert!(
                PluginRegistry::validate_filename(bad).is_err(),
                "`{bad}` 应被拒绝"
            );
        }
    }

    #[test]
    fn rejects_overlong_names() {
        let long = format!("{}.rhai", "a".repeat(300));
        assert!(PluginRegistry::validate_filename(&long).is_err());
    }

    #[test]
    fn accepts_reasonable_names() {
        for good in [
            "my_evaluator.rhai",
            "my-evaluator.rhai",
            "我的评估器.rhai",
            "eval_v2.rhai",
            "UPPER.RHAI",
        ] {
            let got = PluginRegistry::validate_filename(good);
            assert!(got.is_ok(), "`{good}` 应被接受: {:?}", got.err());
        }
    }

    #[test]
    fn trims_whitespace_around_name() {
        assert_eq!(
            PluginRegistry::validate_filename("  a.rhai  ").unwrap(),
            "a.rhai"
        );
    }

    /// 文件名带中文也要能真的落到磁盘并加载 ——
    /// 校验通过但文件系统侧失败是很尴尬的失败模式。
    #[test]
    fn unicode_filename_roundtrips_through_disk() {
        let (_d, r) = reg();
        let name = PluginRegistry::validate_filename("我的评估器.rhai").unwrap();
        let path = r.path_of(PluginKind::Evaluator, &name);
        std::fs::write(&path, "// @name 我的评估器\nfn evaluate(b,c){ () }").unwrap();

        let loaded = r.load_all(PluginKind::Evaluator).unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(loaded[0].is_ok());
        assert_eq!(loaded[0].get().unwrap().manifest().name, "我的评估器");
        assert_eq!(loaded[0].file.display_name(loaded[0].get().unwrap()), "我的评估器");
    }

    /// 没有元信息时用文件名兜底当显示名。
    #[test]
    fn display_name_falls_back_to_filename() {
        let (_d, r) = reg();
        let path = r.path_of(PluginKind::Evaluator, "no_meta.rhai");
        std::fs::write(&path, "fn evaluate(b,c){ () }").unwrap();

        let l = &r.load_all(PluginKind::Evaluator).unwrap()[0];
        assert_eq!(l.file.display_name(l.get().unwrap()), "no_meta");
    }
}
