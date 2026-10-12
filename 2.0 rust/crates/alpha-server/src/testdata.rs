//! 测试基准数据：由原版 Python 跑出来，供 Rust 侧做差分回归。
//!
//! 这些 JSON 是**原版行为的快照**，不是手写的期望值。手写期望值只能证明
//! 「我以为原版怎么做的」，而差分测试证明的是「Rust 版和原版行为一致」。
//! 生成脚本都在 `tools/` 下，注释里写了怎么重跑。
//!
//! 放在 `src/` 而不是 `tests/` 是因为多个测试文件（单元测试 + 集成测试）
//! 都要用，`tests/` 下的模块没法共享。

use serde::Deserialize;

/// 一条 werkzeug 密码哈希基准。
///
/// 由 `tools/gen_password_fixtures.py` 调用**原版 werkzeug** 生成。
#[derive(Debug, Clone, Deserialize)]
pub struct PasswordFixture {
    /// 人类可读的算法名，如 `scrypt` / `pbkdf2-sha256`，只用于报错信息。
    pub algorithm: String,
    /// 传给 `generate_password_hash` 的 method 字符串。
    #[allow(dead_code)]
    pub method: String,
    /// 明文密码。
    pub password: String,
    /// werkzeug 生成的哈希串。
    pub hash: String,
    /// 哈希前缀（`$` 前那段），用来断言走的是哪个解析分支。
    pub prefix: String,
}

#[derive(Debug, Deserialize)]
struct PasswordFixtureFile {
    werkzeug_version: String,
    cases: Vec<PasswordFixture>,
}

/// 读取 `tests/fixtures/password_fixtures.json`，随被测二进制一起编译进去。
///
/// 用 `include_str!` 而不是运行时读文件：这样 `cargo test` 在哪个目录跑
/// 都能找到数据，也不会有「测试通过是因为文件恰好存在」这种假阳性。
pub fn password_fixtures() -> Vec<PasswordFixture> {
    let raw = include_str!("../tests/fixtures/password_fixtures.json");
    let file: PasswordFixtureFile =
        serde_json::from_str(raw).expect("password_fixtures.json 格式不对，重跑 tools/gen_password_fixtures.py");
    file.cases
}

/// 生成这些基准时用的 werkzeug 版本。
///
/// 断言这个不是想锁定版本，而是让「基准是哪一代 werkzeug 产出的」
/// 有据可查 —— 将来 werkzeug 改了哈希格式，看日志就知道该重跑脚本了。
pub fn werkzeug_version() -> String {
    let raw = include_str!("../tests/fixtures/password_fixtures.json");
    let file: PasswordFixtureFile =
        serde_json::from_str(raw).expect("password_fixtures.json 格式不对");
    file.werkzeug_version
}
