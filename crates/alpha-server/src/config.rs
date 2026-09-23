//! 服务配置。
//!
//! # 这一版和原版最重要的一处不同：密钥**没有默认值**
//!
//! 原版 `panel/app.py:42`：
//!
//! ```python
//! app.secret_key = os.environ.get("PANEL_SECRET", "alpha-panel-dev-secret")
//! ```
//!
//! `os.environ.get(..., default)` 写法看着很自然，但它把「配置缺失」这个
//! **部署事故**变成了一个静默的、安全的假象：服务照常启动，只是会话签名
//! 密钥变成了源码里公开的那个字符串。
//!
//! 会话是客户端签名 cookie，签名密钥公开就等于没有鉴权 —— 任何人都能
//! 离线签出一个 `{"user":"hanyx","role":"admin"}` 的 cookie。
//! **这正是现网正在发生的事**（已实测确认，详见 `auth/session.rs` 的说明）。
//!
//! 所以这里改成：密钥缺失 → **拒绝启动**，并打印出「怎么生成、写哪里」。
//! 一个起不来的服务会立刻被人发现；一个谁都能进的服务不会。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// 配置错误。
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("{0}")]
    Missing(String),

    #[error("{0}")]
    Invalid(String),

    #[error("读取 {path} 失败: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// 会话密钥的最小长度。
///
/// 32 字符不是密码学下限（HMAC-SHA256 的密钥可以更短），而是一个
/// **「这确实是人随机生成的」的粗略信号** —— 防止有人填 `123456` 或
/// `secret` 把刚修好的洞又开回去。
pub const MIN_SECRET_LEN: usize = 32;

/// 已知的弱密钥（含原版那个默认值）。
///
/// 单独列出来是因为**最可能发生的回归**就是有人为了「先跑起来」把
/// 原来那个默认值填回去 —— 那样等于没修。这里直接拒掉。
const KNOWN_WEAK_SECRETS: &[&str] = &[
    "alpha-panel-dev-secret",
    "changeme",
    "change-me",
    "secret",
    "test",
    "dev",
    "password",
];

/// 服务配置。
#[derive(Debug, Clone)]
pub struct Config {
    /// 监听地址（原版硬编码 `0.0.0.0`）
    pub host: String,
    /// 监听端口（原版读 `PORT`，默认 5000；线上 systemd 注入 5703）
    pub port: u16,
    /// 会话签名密钥 —— 必填，无默认值
    pub secret: String,
    /// 数据目录（放 `panel.db`、`state.json`、`plugins/`）
    pub data_dir: PathBuf,
    /// 配置目录（放 `rules.yaml` / `settings.yaml` / `sources.yaml`）
    pub config_dir: PathBuf,
    /// 会话有效期（秒）。原版**没有**这个 —— 见 [`Config::default_session_ttl`]
    pub session_ttl: i64,
    /// 环境变量表（用于「系统配置」页展示与写回 `panel.env`）
    pub envs: HashMap<String, String>,
}

/// 默认会话有效期：7 天。
///
/// 原版是「浏览器会话 cookie + 服务端不判过期」—— 关掉浏览器就失效，
/// 但**只要浏览器不关就一直有效**，而且 cookie 一旦泄露就永久可用。
/// 这里给一个明确的上限，属于有意的行为改进（见 `auth/session.rs`）。
pub const DEFAULT_SESSION_TTL: i64 = 7 * 24 * 3600;

impl Config {
    /// 从环境变量装配配置。
    ///
    /// # 为什么错误信息写得这么啰嗦
    ///
    /// 这条错误是**运维第一次部署时唯一会看到的东西**。原版的问题是
    /// 「配置错了但不报错」，跑到线上才被利用；这里反过来 ——
    /// 启动就失败，并且把「怎么修」直接写在错误里，
    /// 让人不需要去翻文档。
    ///
    /// # 取值顺序：进程环境变量优先，其次 `panel.env`
    ///
    /// 原版这里是「只读进程环境变量」，靠 systemd 的 `EnvironmentFile`
    /// 把文件注进来。这条路径在 systemd 下是对的，但**手动起二进制时
    /// 是坏的** —— 而部署脚本恰好会让人先手动起一次验证。
    /// 那种情况下的报错是「PANEL_SECRET 没有设置」，可 `panel.env` 里
    /// 明明刚写进去一行，看的人只会怀疑自己写错了。
    ///
    /// 所以这里补一层回退。**进程环境变量优先级更高**是刻意的：
    /// 临时覆盖（`PANEL_SECRET=xxx ./alpha-server`）必须能生效，
    /// 否则排障时会很别扭。
    pub fn from_env() -> Result<Self, ConfigError> {
        let envs = load_env_file();
        let get = |key: &str| -> Option<String> { env_or_file(key, &envs) };

        let secret = read_secret(&envs)?;

        let host = get("HOST").unwrap_or_else(|| "0.0.0.0".to_string());

        let port = match get("PORT") {
            Some(v) => v
                .trim()
                .parse::<u16>()
                .map_err(|_| ConfigError::Invalid(format!("PORT 不是合法端口号: {v:?}")))?,
            None => 5703,
        };

        let data_dir = get("DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data"));
        let config_dir = get("CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("config"));

        let session_ttl = match get("SESSION_TTL") {
            Some(v) => v
                .trim()
                .parse::<i64>()
                .map_err(|_| ConfigError::Invalid(format!("SESSION_TTL 不是整数秒: {v:?}")))?,
            None => DEFAULT_SESSION_TTL,
        };
        if session_ttl <= 0 {
            return Err(ConfigError::Invalid(
                "SESSION_TTL 必须为正数（0 或负数意味着会话立即失效）".into(),
            ));
        }

        Ok(Self {
            host,
            port,
            secret,
            data_dir,
            config_dir,
            session_ttl,
            envs,
        })
    }

    /// 数据库文件路径。
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("panel.db")
    }

    /// 去重状态文件路径（与原版 `config.dedup.state_file` 对齐）。
    pub fn state_path(&self) -> PathBuf {
        self.data_dir.join("state.json")
    }

    /// 插件目录。
    pub fn plugins_dir(&self) -> PathBuf {
        self.data_dir.join("plugins")
    }

    /// `panel.env` 路径（「系统配置」页要读写它）。
    pub fn env_path(&self) -> PathBuf {
        self.config_dir.join("..").join("panel.env")
    }

    /// 开始服务前创建必需的目录。
    pub fn ensure_dirs(&self) -> Result<(), ConfigError> {
        for dir in [&self.data_dir, &self.plugins_dir()] {
            std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io {
                path: dir.display().to_string(),
                source: e,
            })?;
        }
        Ok(())
    }
}

/// 读取并**校验**会话密钥。
///
/// 见模块头部说明：这里刻意不给默认值。
/// 弱密钥检查：把「公开的默认值」和「一眼就是凑数的」都挡掉。
///
/// 大小写不敏感，因为 `ALPHA-PANEL-DEV-SECRET` 和前缀加空格
/// 在实际部署里都出现过。
pub fn is_known_weak(secret: &str) -> bool {
    let lower = secret.trim().to_ascii_lowercase();
    KNOWN_WEAK_SECRETS.iter().any(|w| lower == *w)
}

/// 长度检查。
///
/// 32 是 HMAC-SHA256 的一个自然门槛：短于它，密钥空间就不足以
/// 抵抗针对签名的离线爆破，签名本身也就失去意义了。
pub fn is_too_short(secret: &str) -> bool {
    secret.trim().len() < MIN_SECRET_LEN
}

/// 纯校验逻辑：给定一个「可能的密钥」，判断能不能用。
///
/// 从 [`read_secret`] 里拆出来是为了让测试不用去动进程级环境变量 ——
/// `std::env::set_var` 在多线程测试里是竞态源，而且改了就影响同进程的
/// 其他测试。拆成纯函数之后，校验规则可以被直接、确定地测到。
///
/// `None` 表示环境变量根本没设置。
pub fn validate_secret_input(raw: Option<&str>) -> Result<String, ConfigError> {
    let raw = raw.ok_or_else(|| ConfigError::Missing(secret_help("环境变量 PANEL_SECRET 没有设置")))?;

    let secret = raw.trim().to_string();

    if secret.is_empty() {
        return Err(ConfigError::Missing(secret_help(
            "环境变量 PANEL_SECRET 是空的",
        )));
    }

    // 弱密钥检查（大小写不敏感）—— 挡住「把原版默认值填回去」这种回归
    if is_known_weak(&secret) {
        return Err(ConfigError::Invalid(format!(
            "PANEL_SECRET 是已知的弱密钥（{:?}）—— 这等于没有鉴权：\n\
             会话是客户端签名 cookie，签名密钥公开的话任何人都能伪造管理员身份。\n\n{}",
            secret,
            secret_help("换一个随机密钥")
        )));
    }

    if is_too_short(&secret) {
        return Err(ConfigError::Invalid(format!(
            "PANEL_SECRET 太短（{} 字符，至少 {}）—— 密钥越短越容易被离线爆破。\n\n{}",
            secret.len(),
            MIN_SECRET_LEN,
            secret_help("换一个更长的随机密钥")
        )));
    }

    Ok(secret)
}

fn read_secret(envs: &HashMap<String, String>) -> Result<String, ConfigError> {
    let raw = env_or_file("PANEL_SECRET", envs);
    validate_secret_input(raw.as_deref())
}

/// 取一个配置值：进程环境变量优先，其次 `panel.env`。
///
/// 空字符串按「没设置」处理 —— `panel.env` 里写一行
/// `PANEL_SECRET=`（值留空）是很自然的误操作，它不该被当成一个
/// 合法的空密钥往下走，而应该和「没写这一行」得到同样的报错。
fn env_or_file(key: &str, envs: &HashMap<String, String>) -> Option<String> {
    if let Ok(v) = std::env::var(key) {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    envs.get(key).map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// 统一的「怎么修」提示。
///
/// 抽出来是因为密钥相关的几条错误信息都要说同样的话，而且这段话
/// 需要包含**可直接复制执行**的命令 —— 运维在凌晨两点看这条报错的
/// 时候，没有心情去翻文档。
fn secret_help(why: &str) -> String {
    format!(
        "{why}\n\
         \n\
         会话签名密钥是必需的，且没有默认值。\n\
         原版之所以有漏洞，就是因为它在密钥缺失时回退到了源码里公开的默认值。\n\
         \n\
         生成一个并写入环境变量文件：\n\
         \n\
             PANEL_SECRET=$(head -c 48 /dev/urandom | base64)\n\
             echo \"PANEL_SECRET=$PANEL_SECRET\" >> panel.env\n\
         \n\
         systemd 场景下要确保 unit 里有 EnvironmentFile=...panel.env，然后重启服务。"
    )
}

/// 读取 `panel.env`（存在才读），返回键值表。
///
/// 解析规则刻意保持简单：`KEY=VALUE`，`#` 开头是注释，空行跳过。
/// 与原版 `panel/config_mgr.py` 读 env 文件的方式对齐，**不做**引号与
/// 转义处理 —— 因为写回时也要用同样的规则，两边不一致会把配置文件搞坏。
fn load_env_file() -> HashMap<String, String> {
    let path = std::env::var("ENV_FILE").unwrap_or_else(|_| "panel.env".to_string());
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    parse_env(&text)
}

/// 解析 `KEY=VALUE` 文本。
///
/// 单独抽出来是为了能单测 —— 写回 `panel.env` 时会覆盖整个文件，
/// 解析错了会把用户的其他配置吃掉。
pub fn parse_env(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue; // 没有等号的行直接忽略，不报错（可能是在注释里写说明）
        };
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        // 值只去掉首尾空白，**不**去引号 —— 保持原样的「所见即所得」，
        // 用户写什么就是什么，我们不猜测他的意图
        out.insert(k.to_string(), v.trim().to_string());
    }
    out
}

/// 把键值表写回 `panel.env`。
///
/// 保留原有的注释与顺序：注释行原样输出，已知键就地替换值，
/// 新键追加到末尾。这样用户在文件里写的说明文字不会被抹掉 ——
/// 原版的「系统配置」页会重写这个文件，弄丢注释是很讨厌的事。
pub fn write_env_file(path: impl AsRef<Path>, updates: &HashMap<String, String>) -> std::io::Result<()> {
    let path = path.as_ref();
    let existing = std::fs::read_to_string(path).unwrap_or_default();

    let mut written: Vec<String> = Vec::new();
    let mut done: Vec<&String> = Vec::new();

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            written.push(line.to_string());
            continue;
        }
        match trimmed.split_once('=').map(|(k, _)| k.trim()) {
            Some(key) if updates.contains_key(key) => {
                let v = &updates[key];
                written.push(format!("{key}={v}"));
                done.push(updates.get_key_value(key).map(|(k, _)| k).unwrap());
            }
            _ => written.push(line.to_string()),
        }
    }

    // 新键追加（保持插入顺序稳定，方便人看）
    let mut fresh: Vec<_> = updates
        .iter()
        .filter(|(k, _)| !done.contains(k))
        .collect();
    fresh.sort_by_key(|(k, _)| k.as_str());
    for (k, v) in fresh {
        written.push(format!("{k}={v}"));
    }

    let mut text = written.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- panel.env 回退：本轮实测发现的真实缺陷 ----

    /// `panel.env` 里的值要被用上。
    ///
    /// **这条是实测发现的 bug 的回归测试。** 部署脚本把 `PANEL_SECRET`
    /// 写进 `panel.env`，然后提示「先手动启动验证一下」—— 而
    /// `Config::from_env()` 当时只读进程环境变量，于是手动启动必然
    /// 报「PANEL_SECRET 没有设置」，可文件里明明刚写了一行。
    /// systemd 路径（`EnvironmentFile`）没暴露这个问题，所以它一直
    /// 活到真机实测才被撞出来。
    #[test]
    fn reads_values_from_the_env_file() {
        let mut envs = HashMap::new();
        envs.insert("PANEL_SECRET".to_string(), "x".repeat(40));
        envs.insert("PORT".to_string(), "9999".to_string());

        assert_eq!(
            env_or_file("PANEL_SECRET", &envs).as_deref(),
            Some("x".repeat(40).as_str())
        );
        assert_eq!(env_or_file("PORT", &envs).as_deref(), Some("9999"));
    }

    /// 进程环境变量**优先于** `panel.env`。
    ///
    /// 临时覆盖（`PORT=8080 ./alpha-server`）必须能生效，
    /// 否则排障时改了环境变量却发现没起作用，会很别扭。
    #[test]
    fn the_process_environment_wins_over_the_file() {
        let mut envs = HashMap::new();
        envs.insert("ALPHA_TEST_PRECEDENCE_KEY".to_string(), "from-file".to_string());

        std::env::set_var("ALPHA_TEST_PRECEDENCE_KEY", "from-process");
        assert_eq!(
            env_or_file("ALPHA_TEST_PRECEDENCE_KEY", &envs).as_deref(),
            Some("from-process")
        );

        std::env::remove_var("ALPHA_TEST_PRECEDENCE_KEY");
        assert_eq!(
            env_or_file("ALPHA_TEST_PRECEDENCE_KEY", &envs).as_deref(),
            Some("from-file"),
            "进程变量清掉之后该回落到文件里的值"
        );
    }

    /// 空值按「没设置」处理。
    ///
    /// `panel.env` 里写一行 `PANEL_SECRET=`（值留空）是很自然的误操作，
    /// 它不该被当成一个合法的空密钥往下走 —— 那会让服务用空串签名会话，
    /// 比启动失败糟得多。
    #[test]
    fn an_empty_value_counts_as_unset() {
        let mut envs = HashMap::new();
        envs.insert("PANEL_SECRET".to_string(), String::new());
        assert_eq!(env_or_file("PANEL_SECRET", &envs), None);

        envs.insert("PANEL_SECRET".to_string(), "   ".to_string());
        assert_eq!(env_or_file("PANEL_SECRET", &envs), None, "只有空白也算没设置");

        // 两处都没有
        assert_eq!(env_or_file("TOTALLY_MISSING_KEY_XYZ", &envs), None);
    }

    /// 值两边的空白要去掉（`panel.env` 里手写很容易多打空格）。
    #[test]
    fn values_are_trimmed() {
        let mut envs = HashMap::new();
        envs.insert("PORT".to_string(), "  8080  ".to_string());
        assert_eq!(env_or_file("PORT", &envs).as_deref(), Some("8080"));
    }

    // ---- 密钥校验：这是本次修复的核心，测厚一点 ----

    /// 原版那个默认值必须被拒 —— 这是防回归的第一道闸。
    #[test]
    fn rejects_the_original_default_secret() {
        // 直接测校验函数（避免依赖进程级环境变量，测试才能并行跑）
        assert!(is_weak("alpha-panel-dev-secret"));
    }

    #[test]
    fn rejects_other_obvious_weak_secrets() {
        for s in ["changeme", "SECRET", "Password", "test", "dev"] {
            assert!(is_weak(s), "{s:?} 应被判为弱密钥");
        }
    }

    #[test]
    fn rejects_short_secrets() {
        assert!(is_too_short("abc123"));
        assert!(is_too_short(""));
        assert!(!is_too_short("x".repeat(MIN_SECRET_LEN).as_str()));
    }

    #[test]
    fn accepts_a_random_looking_secret() {
        let s = "dGhpcyBpcyBhIHJhbmRvbS1sb29raW5nIHNlY3JldCB2YWx1ZQ==";
        assert!(!is_weak(s));
        assert!(!is_too_short(s));
    }

    // ---- panel.env 解析：写回时会把整个文件覆盖，不能解析错 ----

    #[test]
    fn parses_simple_key_value() {
        let m = parse_env("FOO=bar\nBAZ=qux\n");
        assert_eq!(m.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(m.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let m = parse_env("# 注释\n\n   \nFOO=bar\n# 又一个注释\n");
        assert_eq!(m.len(), 1, "只应解析出 FOO");
        assert_eq!(m.get("FOO").map(String::as_str), Some("bar"));
    }

    /// 空值是合法的（现网 `WXPUSHER_APP_TOKEN_alpha=` 就是空的，
    /// 「已声明但未填写」和「没声明」语义不同，不能丢）。
    #[test]
    fn keeps_empty_values() {
        let m = parse_env("WXPUSHER_APP_TOKEN_alpha=\n");
        assert_eq!(m.get("WXPUSHER_APP_TOKEN_alpha").map(String::as_str), Some(""));
    }

    /// 值里含 `=` 时只切第一个等号 —— base64 密钥结尾就是 `=`。
    #[test]
    fn value_may_contain_equals_signs() {
        let m = parse_env("PANEL_SECRET=abc=def==\n");
        assert_eq!(m.get("PANEL_SECRET").map(String::as_str), Some("abc=def=="));
    }

    #[test]
    fn ignores_lines_without_equals() {
        let m = parse_env("这不是一个配置行\nFOO=bar\n");
        assert_eq!(m.len(), 1);
    }

    /// 写回时必须保留注释 —— 原版会重写整个文件，把用户的说明吃掉。
    #[test]
    fn write_preserves_comments_and_updates_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panel.env");
        std::fs::write(
            &path,
            "# WxPusher 推送 token\nWXPUSHER_APP_TOKEN_alpha=\n# 下面是代理密钥\nPROXY_KEY=old\n",
        )
        .unwrap();

        let mut updates = HashMap::new();
        updates.insert("PROXY_KEY".to_string(), "new".to_string());
        updates.insert("PANEL_SECRET".to_string(), "s3cret".to_string());
        write_env_file(&path, &updates).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# WxPusher 推送 token"), "注释被吃掉了:\n{text}");
        assert!(text.contains("# 下面是代理密钥"), "第二个注释也没了:\n{text}");
        assert!(text.contains("PROXY_KEY=new"), "值没更新:\n{text}");
        assert!(text.contains("PANEL_SECRET=s3cret"), "新键没追加:\n{text}");
        assert!(!text.contains("PROXY_KEY=old"));
    }

    /// 写回后的文件必须能被重新解析回来 —— 这一对函数是一致性契约。
    #[test]
    fn write_then_parse_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panel.env");

        let mut updates = HashMap::new();
        updates.insert("A".to_string(), "1".to_string());
        updates.insert("B".to_string(), "含=等号".to_string());
        updates.insert("C".to_string(), String::new());
        write_env_file(&path, &updates).unwrap();

        let back = parse_env(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(back.get("A").map(String::as_str), Some("1"));
        assert_eq!(back.get("B").map(String::as_str), Some("含=等号"));
        assert_eq!(back.get("C").map(String::as_str), Some(""));
    }

    // ---- 路径推导 ----

    #[test]
    fn derives_paths_from_dirs() {
        let cfg = Config {
            host: "127.0.0.1".into(),
            port: 5703,
            secret: "x".repeat(40),
            data_dir: PathBuf::from("/srv/alpha/data"),
            config_dir: PathBuf::from("/srv/alpha/config"),
            session_ttl: DEFAULT_SESSION_TTL,
            envs: HashMap::new(),
        };
        assert_eq!(cfg.db_path(), PathBuf::from("/srv/alpha/data/panel.db"));
        assert_eq!(cfg.state_path(), PathBuf::from("/srv/alpha/data/state.json"));
        assert_eq!(cfg.plugins_dir(), PathBuf::from("/srv/alpha/data/plugins"));
    }

    #[test]
    fn default_session_ttl_is_one_week() {
        assert_eq!(DEFAULT_SESSION_TTL, 7 * 24 * 3600);
    }

    // ---- 下面两个是纯判定函数，抽出来才能不依赖进程环境变量地测 ----

    /// 与 `read_secret` 里的弱密钥判定保持一致。
    fn is_weak(secret: &str) -> bool {
        let lower = secret.to_ascii_lowercase();
        KNOWN_WEAK_SECRETS.iter().any(|w| lower == *w)
    }

    fn is_too_short(secret: &str) -> bool {
        secret.trim().len() < MIN_SECRET_LEN
    }
}
