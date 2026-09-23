//! 前端资源的定位与加载。
//!
//! # 背景：后端重写了，前端没重写
//!
//! 用户明确要求前端保持原样（「前端就没必要重构了，现在用着还行」），
//! 所以这里托管的是原版那套前端：`style.css` + `i18n.js` + `app.js`，
//! 一字未改。唯一的新增是 `csrf-shim.js` —— 见 [`crate::routes::spa`]
//! 与那文件自身的注释。
//!
//! # 为什么用「启动时读进内存」而不是每次请求读文件
//!
//! `web/static/` 一共 148K，入口页几 KB。全读进内存之后：
//!
//! - 每次请求不用碰磁盘（面板本身是低 QPS、但前端资源会被反复拉）
//! - **部署时不用担心「二进制换了、web/ 没换」** —— 入口页的内容
//!   在第一秒就固定了，跑起来之后挪走 `web/` 目录也不影响
//!
//! 代价是「改了 HTML 要重启」。对面板这种量级的服务，这个代价可以忽略，
//! 而且重启本来也就是 5 秒钟的事。
//!
//! # 静态目录怎么找
//!
//! 判定顺序与 `alpha_core::config::project_root()` 对齐，但**这里是
//! 独立的**：前端资源是 `alpha-server` 自己的东西，不该让 core 去知道。
//!
//! 1. 环境变量 `ALPHA_WEB_ROOT`（部署脚本会设）
//! 2. 从可执行文件位置向上找含 `web/index.html` 的目录
//!    （`target/release/alpha-server` 往上三级就是仓库根）
//! 3. 当前工作目录下的 `web/`
//!
//! 找不到**不报错、不 panic**：面板的 API 还能用，只是界面出不来。
//! 启动时打一条 `error` 日志说明清楚，并把降级页返回给用户。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 入口页内容（读一次，之后复用）。
///
/// `None` 表示**读不到**（不是「还没读」）—— 用 `OnceLock<Option<String>>`
/// 而不是 `Option<OnceLock<String>>`，是为了让「找不到文件」这个结论
/// 也只计算一次，别每次请求都去 stat 一遍磁盘。
static INDEX_HTML: OnceLock<Option<String>> = OnceLock::new();

/// 前端根目录（含 `index.html` 与 `static/`）。找不到返回 `None`。
pub fn web_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("ALPHA_WEB_ROOT") {
        let p = PathBuf::from(p);
        if p.join("index.html").exists() {
            return Some(p);
        }
        // 显式配了但不对 —— 这是配置错误，要说出来，
        // 否则会静默退到下面几个「猜」出来的路径上，让人以为配置生效了
        tracing::error!(
            path = %p.display(),
            "ALPHA_WEB_ROOT 指向的目录里没有 index.html，将尝试自动定位"
        );
    }

    // 从可执行文件位置往上找。
    // `target/release/alpha-server` → `target/release` → `target` → 仓库根
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf);
        while let Some(dir) = cur {
            if dir.join("web").join("index.html").exists() {
                return Some(dir.join("web"));
            }
            cur = dir.parent().map(Path::to_path_buf);
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if cwd.join("web").join("index.html").exists() {
        return Some(cwd.join("web"));
    }

    None
}

/// 静态资源目录（`web/static/`）。
///
/// `None` 时 [`ServeDir`] 不该被挂上 —— 挂一个不存在的目录会让
/// `/static/*` 全部 404，而 404 的响应体是空的，排查起来很费劲。
/// 调用方拿 `None` 时应该走 [`crate::routes::spa::static_not_found`]。
///
/// [`ServeDir`]: tower_http::services::ServeDir
pub fn static_dir() -> Option<PathBuf> {
    let dir = web_root()?.join("static");
    dir.is_dir().then_some(dir)
}

/// 入口页 HTML。读不到返回 `None`（调用方负责降级）。
pub fn index_html() -> Option<String> {
    INDEX_HTML
        .get_or_init(|| {
            let root = web_root()?;
            let path = root.join("index.html");
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    tracing::info!(path = %path.display(), bytes = text.len(), "前端入口页已加载");
                    Some(text)
                }
                Err(e) => {
                    tracing::error!(path = %path.display(), error = %e, "前端入口页读取失败");
                    None
                }
            }
        })
        .clone()
}

/// 读不到前端资源时返回给浏览器的页面。
///
/// 刻意做成**能自证问题**的：白屏是最难排查的一种故障 ——
/// 用户只会说「打不开」，运维得去翻日志才知道是资源没搬。
/// 这页直接把「期望路径」和「怎么指定」写在脸上。
pub fn missing_assets_page() -> String {
    let hint = web_root_hint();
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>Pokemmo Alpha — 前端资源缺失</title>
<style>
  body {{ font: 14px/1.7 -apple-system, "Segoe UI", "PingFang SC", sans-serif;
         max-width: 720px; margin: 64px auto; padding: 0 24px; color: #24292f; }}
  h1 {{ font-size: 20px; margin: 0 0 4px; }}
  code, pre {{ font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
               font-size: 13px; background: #f6f8fa; border-radius: 6px; }}
  code {{ padding: 2px 6px; }}
  pre {{ padding: 12px 16px; overflow-x: auto; }}
  .warn {{ color: #b35900; }}
</style>
<h1>前端资源缺失</h1>
<p>后端已正常启动 —— <strong>接口是可用的</strong>，但界面的静态文件没找到。</p>
<p>已探测的路径：</p>
<pre>{hint}</pre>
<p>修法：把仓库里的 <code>web/</code> 目录放到二进制旁边，或者直接指定：</p>
<pre>ALPHA_WEB_ROOT=/opt/pokemmo-alpha/web ./alpha-server</pre>
<p class="warn">面板的 API（<code>/api/*</code>）不受影响，可以先通过接口确认服务本身正常。</p>
"#
    )
}

/// 探测了哪些路径 —— 用于降级页。
fn web_root_hint() -> String {
    let mut lines = vec!["ALPHA_WEB_ROOT=<未设置>".to_string()];
    if let Ok(exe) = std::env::current_exe() {
        lines.push(format!("可执行文件: {}", exe.display()));
        let mut cur = exe.parent().map(Path::to_path_buf);
        while let Some(dir) = cur {
            lines.push(format!("  尝试: {}", dir.join("web").display()));
            cur = dir.parent().map(Path::to_path_buf);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    lines.push(format!("当前工作目录: {}", cwd.display()));
    lines.push(format!("  尝试: {}", cwd.join("web").display()));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 仓库自带的 `web/` 目录必须是完整的。
    ///
    /// 这条测试的价值在于：**部署脚本正是靠「把 web/ 拷到二进制旁边」
    /// 来工作的**。如果哪天有人清理仓库时顺手删了某个文件，
    /// 部署出去的面板就会白屏 —— 而那种问题在 CI 里不会有任何征兆。
    #[test]
    fn the_repository_ships_a_complete_frontend() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("web");

        assert!(
            root.join("index.html").is_file(),
            "缺少 web/index.html —— 部署后会白屏"
        );

        // 原版前端的四个 js + 样式 + 图标，一个都不能少
        for f in [
            "static/app.js",
            "static/i18n.js",
            "static/debug.js",
            "static/dispatchers.js",
            "static/style.css",
            "static/logo.png",
            "static/logo_32.png",
            "static/logo_64.png",
        ] {
            assert!(root.join(f).is_file(), "缺少 web/{f}");
        }

        assert!(
            root.join("static/csrf-shim.js").is_file(),
            "缺少 CSRF 垫片 —— 部署后所有写操作都会 400"
        );
    }

    /// **垫片必须排在 app.js 之前。**
    ///
    /// app.js 是立即执行的 IIFE，加载那一刻就发 bootstrap 请求。
    /// 垫片排在它后面的话，最开始那几发请求漏掉 CSRF 头 ——
    /// 症状是「刚打开面板时偶发报错、刷新一下又好了」，
    /// 属于最难复现的那类 bug。
    #[test]
    fn csrf_shim_loads_before_the_app() {
        let html = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("web/index.html");
        let text = std::fs::read_to_string(&html).unwrap();

        // 匹配带引号的完整 src 属性，而不是裸文件名 —— 垫片文件里
        // 有一句注释写着「必须排在 app.js 之前」，用裸文件名匹配的话
        // `find("app.js")` 会先命中那句注释，测试就永远失败。
        let shim = text
            .find(r#"src="/static/csrf-shim.js""#)
            .expect("index.html 里没有垫片引用");
        let app = text
            .find(r#"src="/static/app.js"#)
            .expect("index.html 里没有 app.js 引用");

        assert!(
            shim < app,
            "csrf-shim.js 必须排在 app.js 之前，否则启动时的请求不带 CSRF 头"
        );
    }

    /// 入口页里的静态资源路径必须都是 `/static/...`。
    ///
    /// 原版模板用的是 Flask 的 `url_for('static', ...)`，而这里换成了
    /// 写死的路径 —— 如果哪天有人改回模板语法，或者把目录结构挪了，
    /// 这条能挡住。
    #[test]
    fn index_references_static_assets_by_absolute_path() {
        let html = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("web/index.html");
        let text = std::fs::read_to_string(&html).unwrap();

        assert!(
            !text.contains("url_for"),
            "index.html 里还留着 Flask 模板语法 —— 我们是静态托管，不会渲染它"
        );
        assert!(text.contains("/static/style.css"), "样式表路径不对");
        assert!(text.contains("/static/app.js"), "主脚本路径不对");
        assert!(text.contains("/static/i18n.js"), "i18n 路径不对");
    }

    /// 降级页必须能自证问题（包含期望路径与修复开关）。
    #[test]
    fn the_fallback_page_tells_you_how_to_fix_it() {
        let page = missing_assets_page();
        assert!(page.contains("ALPHA_WEB_ROOT"), "没说怎么指定路径");
        assert!(page.contains("<pre>"), "没把探测路径列出来");
        assert!(page.starts_with("<!doctype html>"), "不是完整的 HTML 文档");
        // 要明确告诉用户「后端是好的」，否则会误以为整个服务挂了
        assert!(page.contains("接口是可用的"));
    }
}
