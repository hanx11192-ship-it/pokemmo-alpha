//! 给脚本用的通用小工具（补 Rhai 标准库的缺口）。
//!
//! # 为什么需要这个模块
//!
//! Rhai 的内置包并不包含 `Array::join()` —— 我原本以为有（Python 的
//! `"/".join(names)` 太顺手了），结果脚本一跑就报
//! `Function not found: join (array, &str)`。它也没有 `format!` 之外的
//! 数值格式化拼接工具。
//!
//! 有两个选择：
//!
//! 1. 在脚本里手写字符串累加循环 —— 每个插件都要写一遍，而且容易写出
//!    「末尾多一个分隔符」这种 bug；
//! 2. 在这里用 Rust 实现一次，注册给所有脚本用。
//!
//! 选 2。这些函数**全是纯计算**：只读入参、返回新值，不碰文件 / 网络 /
//! 进程 / 环境变量，所以注册它们不扩大沙箱的攻击面（见 `sandbox.rs`
//! 的权限表）。
//!
//! # 加了新函数要做什么
//!
//! [`SCRIPT_STD_FUNCTIONS`] 里的名字会被 `sandbox_exposes_no_io` 之类的
//! 测试间接约束 —— 加函数时想清楚「这个函数给脚本多开了什么口子」，
//! 有 IO 的一律不要加。

use rhai::{Array, Dynamic, Engine};

/// 把一个数组拼成字符串，用 `sep` 分隔。
///
/// 原版 Python 的 `"/".join(names)` 对应。每个元素用 `to_string()` 语义
/// 转成文本，所以数字数组也能直接拼。
///
/// 空数组返回空串（而不是 `"[]"`）—— 与 Python `join` 一致。
/// 这一点很重要：`factors.push(\`翻车技×${n}:${names.join("/")}\`)` 里
/// 若拼出 `"[]"` 会直接出现在用户的报文里。
fn join_array(arr: Array, sep: &str) -> String {
    arr.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(sep)
}

/// 把数组拼成字符串，用默认分隔符 `, `。
///
/// 提供这个重载是为了 `names.join()` 也能用 —— 脚本里少写一个参数
/// 是很自然的事，报「参数不匹配」对写脚本的人不友好。
fn join_array_default(arr: Array) -> String {
    join_array(arr, ", ")
}

/// 取数组里第一个「等于给定值」的下标，没有返回 `-1`。
///
/// 给脚本判断「这个技能在列表里的位置」用；`index_of` 在 Rhai 里
/// 只有字符串版本，数组没有。
fn array_index_of(arr: Array, target: Dynamic) -> i64 {
    arr.iter()
        .position(|v| dynamic_eq(v, &target))
        .map_or(-1, |i| i as i64)
}

/// 判断两个 [`Dynamic`] 是否「相等」。
///
/// # 为什么要自己写
///
/// `rhai::Dynamic` 没有实现 `PartialEq`（因为它内部是 `Union` 枚举，
/// 还可能装 `Any`），而脚本里的 `==` 是引擎在运行时按两边类型分派到
/// 具体函数的。这里是个 Rust 侧的辅助函数，拿不到那个分派，
/// 所以退一步：**按类型比**。
///
/// 覆盖脚本里真正会用到的那几种标量足够 —— 字符串、整数、浮点、布尔、
/// 单位。其它类型（数组 / Map / 自定义类型）一律判不等，
/// 而不是猜测着递归比较：宁可「查不到」，也不要给出错误的「查到了」。
///
/// 跨数值类型（`1` vs `1.0`）也判不等，这与 Rhai 的严格比较语义一致。
fn dynamic_eq(a: &Dynamic, b: &Dynamic) -> bool {
    match (a.type_id(), b.type_id()) {
        (x, y) if x != y => false,
        _ => {
            // 类型相同，逐个试具体标量
            if a.is_unit() {
                return true;
            }
            if let (Ok(x), Ok(y)) = (a.as_int(), b.as_int()) {
                return x == y;
            }
            if let (Ok(x), Ok(y)) = (a.as_float(), b.as_float()) {
                return x == y;
            }
            if let (Ok(x), Ok(y)) = (a.as_bool(), b.as_bool()) {
                return x == y;
            }
            if let (Some(x), Some(y)) = (
                a.clone().try_cast::<String>(),
                b.clone().try_cast::<String>(),
            ) {
                return x == y;
            }
            false
        }
    }
}

/// 按 id 列表批量取名字，拼成 `名字/名字` —— 纯语法糖，省掉脚本里的循环。
///
/// 主要给「因子明细」这类需要把一组技能名拼起来的地方用。
fn join_names(names: Array, sep: &str) -> String {
    join_array(names, sep)
}

/// 这些函数会被注册到所有插件引擎上。
///
/// 目前只暴露给**内建/上传脚本**使用，不做权限区分 ——
/// 它们都不涉及 IO，没有区分的必要。
pub const SCRIPT_STD_FUNCTIONS: &[&str] = &[
    "join",
    "array_index_of",
    "join_names",
];

/// 把通用小工具注册到引擎上。
///
/// 由 [`crate::sandbox`] 的 `build_engine` 调用，所以**所有**插件
/// （内建与上传）都能用到同一套函数。
pub(crate) fn register(engine: &mut Engine) {
    // `join(array, sep)` —— 对应 Python 的 `sep.join(arr)`
    engine.register_fn("join", join_array);
    // `join(array)` —— 默认 `", "`
    engine.register_fn("join", join_array_default);
    // 语义与 Python `list.index()` 接近，但查不到不抛异常而是返回 -1，
    // 因为脚本里 `if ... index_of(...) >= 0` 比 try/catch 好写
    engine.register_fn("array_index_of", array_index_of);
    // 与 join 同义，只是名字更贴近「拼技能名」这个用途，便于阅读
    engine.register_fn("join_names", join_names);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::{Context, Plugin, PluginKind};
    use crate::test_support::boss;

    /// 跑一段脚本，返回 `dispatch` 的结果。
    fn run(src: &str) -> String {
        let p = Plugin::compile_inline(src, PluginKind::Dispatcher).expect("脚本应能编译");
        p.dispatch(&boss("x", &[]), &Context::zh())
            .expect("脚本应能执行")
    }

    /// 这是触发本模块存在的那条报错，钉住它别再回来。
    #[test]
    fn join_works_because_rhai_has_no_builtin_join() {
        let out = run(r#"fn dispatch(boss, ctx) { ["a", "b", "c"].join("/") }"#);
        assert_eq!(out, "a/b/c");
    }

    #[test]
    fn join_with_default_separator() {
        let out = run(r#"fn dispatch(boss, ctx) { ["a", "b"].join() }"#);
        assert_eq!(out, "a, b");
    }

    /// 空数组拼出来必须是空串。若拼成 `"[]"`，这个字符串会直接
    /// 出现在用户收到的报文里（`翻车技×0:[]` 这种），很难看。
    #[test]
    fn join_of_empty_array_is_empty_string() {
        let out = run(r#"fn dispatch(boss, ctx) { let e = []; "[" + e.join("/") + "]" }"#);
        assert_eq!(out, "[]", "空数组应拼成空串，外层括号是测试自己加的");
    }

    /// 单元素不该多出分隔符 —— 手写循环最容易在这里出错。
    #[test]
    fn join_of_single_element_has_no_separator() {
        let out = run(r#"fn dispatch(boss, ctx) { ["只有我"].join("/") }"#);
        assert_eq!(out, "只有我");
    }

    #[test]
    fn join_handles_numbers() {
        let out = run(r#"fn dispatch(boss, ctx) { [1, 2, 3].join("-") }"#);
        assert_eq!(out, "1-2-3");
    }

    #[test]
    fn array_index_of_finds_and_misses() {
        let out = run(
            r#"fn dispatch(boss, ctx) {
                let a = ["x", "y", "z"];
                `${a.array_index_of("y")},${a.array_index_of("nope")}`
            }"#,
        );
        assert_eq!(out, "1,-1");
    }

    #[test]
    fn join_names_is_equivalent_to_join() {
        let out = run(r#"fn dispatch(boss, ctx) { ["挑衅", "近身战"].join_names("+") }"#);
        assert_eq!(out, "挑衅+近身战");
    }

    /// 这些函数是纯计算 —— 不注册它们不该影响其它函数的可用性，
    /// 反过来注册它们也不该让 IO 函数变得可调用。
    #[test]
    fn registering_helpers_does_not_open_io() {
        for forbidden in ["system", "read_file", "write_file"] {
            let src = format!(r#"fn dispatch(boss, ctx) {{ {forbidden}("x") }}"#);
            let Ok(p) = Plugin::compile_inline(&src, PluginKind::Dispatcher) else {
                continue;
            };
            assert!(
                p.dispatch(&boss("x", &[]), &Context::zh()).is_err(),
                "`{forbidden}` 在注册了脚本工具后变得可调用了 —— 沙箱漏了"
            );
        }
    }

    /// 清单与实际注册的函数要一一对上，避免「文档说有一个函数、
    /// 实际没注册」这种只在用户手里才暴露的差异。
    #[test]
    fn function_list_matches_what_is_registered() {
        assert!(SCRIPT_STD_FUNCTIONS.contains(&"join"));
        assert!(SCRIPT_STD_FUNCTIONS.contains(&"array_index_of"));
        assert!(SCRIPT_STD_FUNCTIONS.contains(&"join_names"));

        // 清单里每个名字都必须真的能调用（用最简调用签名探一下）。
        // 字符串类的直接返回，`array_index_of` 返回整数要显式转字符串 ——
        // 给整数加 `.to_string()` 是无效的（本来就能当字符串拼）。
        let probes = [
            (r#"["a"].join("/")"#, "a"),
            (r#"["a"].array_index_of("a").to_string()"#, "0"),
            (r#"["a"].join_names("/")"#, "a"),
        ];
        for (call, want) in probes {
            let src = format!(r#"fn dispatch(boss, ctx) {{ {call} }}"#);
            assert_eq!(run(&src), want, "`{call}` 的结果不对");
        }
    }
}
