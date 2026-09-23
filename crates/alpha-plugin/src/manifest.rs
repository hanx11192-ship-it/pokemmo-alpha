//! 插件元信息。
//!
//! # 为什么不用单独的 manifest 文件
//!
//! 原版把 `NAME` / `DESCRIPTION` 写成 Python 模块级变量，加载后反射读取。
//! 换到 Rhai 有两条路：
//!
//! 1. **让脚本导出 `const NAME = "..."`**，Rhai 支持 `const`；
//! 2. **从脚本头部的注释里读**。
//!
//! 这里两条都支持，但优先用注释：
//!
//! - 元信息是**给人看的**（面板列表里显示的名字和说明），放在文件开头的
//!   注释块里，用户不打开面板也能一眼看到这个脚本是干什么的；
//! - `const` 是脚本运行期的东西，读它得先执行脚本 —— 一个语法有问题的
//!   脚本就连「它叫什么」都读不出来，面板上只能显示一行文件名。
//! - 注释解析失败时无成本降级，不会让插件加载失败。

use rhai::AST;

/// 插件元信息。
///
/// 所有字段默认是空串（而不是 `None`）：面板的表单、数据库的 `NOT NULL`
/// 列、脚本的 `// @xxx` 都是「没有就空着」的语义，用 `String` 比
/// `Option<String>` 少一层解包，调用方不必到处 `unwrap_or_default()`。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    /// 面板显示名
    pub name: String,
    /// 一句话说明
    pub description: String,
    /// 作者（可选）
    pub author: String,
    /// 版本（可选）
    pub version: String,
}

impl Manifest {
    /// 从源码 + 已编译的 AST 里提取元信息。
    ///
    /// 优先级：文件头注释 > AST 里的 `const` > 空（由调用方用文件名兜底）。
    pub fn parse(source: &str, ast: &AST) -> Self {
        let mut m = Self::parse_header_comments(source);

        // 注释里没给，就看看脚本有没有定义 const（Rhai 的常量会进 AST 的变量表）
        if m.name.is_empty() {
            m.name = const_string(ast, "NAME").unwrap_or_default();
        }
        if m.description.is_empty() {
            m.description = const_string(ast, "DESCRIPTION").unwrap_or_default();
        }
        if m.version.is_empty() {
            m.version = const_string(ast, "VERSION").unwrap_or_default();
        }
        if m.author.is_empty() {
            m.author = const_string(ast, "AUTHOR").unwrap_or_default();
        }
        m
    }

    /// 解析文件头部注释块里的 `@key value` 行。
    ///
    /// 只扫**第一个非注释行之前**的区域 —— 否则脚本正文里出现的
    /// `@name` 字样（比如字符串常量）会被误当成元信息。
    ///
    /// # 只认 `//`，不认 `#`
    ///
    /// Rhai 里 `#` 是保留符号（`#{...}` 是 map 字面量），
    /// `# 注释` **无法通过编译**。所以这里不认 `#` 开头的行：
    ///
    /// - 认了也没用 —— 带 `#` 注释的脚本根本编译不过，走不到读元信息这一步；
    /// - 反而有害 —— `#{ name: "x" }` 里的内容会被误读成元信息，
    ///   尤其是当 map 字面量出现在文件开头时。
    fn parse_header_comments(source: &str) -> Self {
        let mut m = Self::default();
        for raw in source.lines() {
            let line = raw.trim();

            // 空行不断开头（允许注释块前有空行），但一旦遇到代码就停
            if line.is_empty() {
                continue;
            }

            let content = if let Some(c) = line.strip_prefix("///") {
                c.trim()
            } else if let Some(c) = line.strip_prefix("//") {
                c.trim()
            } else {
                break; // 遇到代码（或 `#{...}` 这样的 map 字面量），头部结束
            };

            // 元信息行形如 `@name 我的决策器`
            for (key, slot) in [
                ("@name", &mut m.name),
                ("@description", &mut m.description),
                ("@author", &mut m.author),
                ("@version", &mut m.version),
            ] {
                if let Some(v) = content.strip_prefix(key) {
                    // 避免 `@namex` 被当成 `@name`
                    if v.starts_with(char::is_whitespace) || v.starts_with(':') {
                        let v = v.trim_start_matches(|c: char| c.is_whitespace() || c == ':');
                        if slot.is_empty() {
                            *slot = v.trim().to_string();
                        }
                    }
                }
            }
        }
        m
    }
}

/// 从 AST 里取一个字符串常量的值。
///
/// 只认**字面量**常量（`const NAME = "xxx";`）—— `iter_literal_variables`
/// 只返回能静态求值的那些。`const NAME = "a" + "b";` 这种拿不到，
/// 属于可接受的限制：元信息本来就该写成字面量。
fn const_string(ast: &AST, name: &str) -> Option<String> {
    ast.iter_literal_variables(true, false)
        .find(|(n, _, _)| *n == name)
        .and_then(|(_, _, v)| v.try_cast::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &str) -> Manifest {
        let engine = rhai::Engine::new();
        let ast = engine.compile(src).unwrap();
        Manifest::parse(src, &ast)
    }

    #[test]
    fn reads_all_metadata_fields() {
        let m = parse(
            r#"
            // @name 我的决策器
            // @description 在官方报告后追加一行提示
            // @author hanyx
            // @version 1.2

            fn dispatch(boss, ctx) { "x" }
            "#,
        );
        assert_eq!(m.name, "我的决策器");
        assert_eq!(m.description, "在官方报告后追加一行提示");
        assert_eq!(m.author, "hanyx");
        assert_eq!(m.version, "1.2");
    }

    /// 用户从 Python 迁过来时习惯写 `#` 注释，但 Rhai 里 `#` 是保留符号 ——
    /// 这种脚本**编译不过**，所以元信息读取阶段根本看不到它。
    /// 这里钉住这个事实，免得日后有人「顺手加个 `#` 支持」反而引入误解析。
    #[test]
    fn hash_comments_are_not_valid_rhai() {
        let engine = rhai::Engine::new();
        let src = "# @name 哈希注释\nfn dispatch(b,c){ \"x\" }";
        assert!(
            engine.compile(src).is_err(),
            "`#` 注释在 Rhai 里应当编译失败（它是保留符号）"
        );
    }

    /// map 字面量开头的 `#{` 不能被误读成注释 ——
    /// 这是「不认 `#` 注释」的直接收益。
    #[test]
    fn map_literal_at_file_start_is_not_metadata() {
        let m = parse("let cfg = #{ name: \"张伟\" };\nfn dispatch(b,c){ \"x\" }");
        assert!(
            m.name.is_empty(),
            "map 字面量里的 name 被误读成元信息了: {m:?}"
        );
    }

    /// 代码里出现的 `@name` 不能被当成元信息 ——
    /// 只扫文件头部，遇到代码就停。
    #[test]
    fn ignores_metadata_like_text_in_code() {
        let m = parse(
            r#"
            fn dispatch(boss, ctx) {
                let s = "// @name 假的";
                s
            }
            "#,
        );
        assert!(m.name.is_empty(), "不应从代码体内读到元信息: {m:?}");
    }

    /// 没有元信息时不能报错，只能给空值（调用方用文件名兜底）。
    #[test]
    fn missing_metadata_is_not_an_error() {
        let m = parse("fn dispatch(boss, ctx) { \"x\" }");
        assert_eq!(m, Manifest::default());
    }

    /// `@namex` 不该被误认成 `@name`。
    #[test]
    fn does_not_match_key_prefix() {
        let m = parse("// @namex 错误\nfn dispatch(b,c){ \"x\" }");
        assert!(m.name.is_empty(), "前缀误匹配: {m:?}");
    }

    /// 冒号写法也要认（`@name: xxx`）。
    #[test]
    fn accepts_colon_separator() {
        let m = parse("// @name: 带冒号\nfn dispatch(b,c){ \"x\" }");
        assert_eq!(m.name, "带冒号");
    }

    /// 后面的同名字段不该覆盖前面的（第一个生效，符合「就近原则」直觉）。
    #[test]
    fn first_occurrence_wins() {
        let m = parse("// @name 第一个\n// @name 第二个\nfn dispatch(b,c){ \"x\" }");
        assert_eq!(m.name, "第一个");
    }

    /// 中文与 emoji 都不能把解析搞乱。
    #[test]
    fn handles_unicode() {
        let m = parse("// @name 带 emoji 🎯 的名字\n// @description 说明——含破折号\nfn d(b,c){ \"x\" }");
        assert_eq!(m.name, "带 emoji 🎯 的名字");
        assert_eq!(m.description, "说明——含破折号");
    }
}
