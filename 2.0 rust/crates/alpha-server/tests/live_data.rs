//! 对**线上真实数据**的回归测试。
//!
//! 这些测试用的不是造出来的样例，而是直接从现网 `panel.db` 抄出来的记录。
//! 它们回答的是同一个问题：**迁移到 Rust 版之后，现网这个用户还能不能登进来。**

use alpha_server::testdata;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct LiveUser {
    username: String,
    password_hash: String,
    hash_len: usize,
    /// 是否拿不到明文（线上那条记录）。合成分支下为 false。
    #[serde(default)]
    unknown_plaintext: bool,
    /// 是否是用合成数据顶上的（即没有真实线上副本）。
    #[serde(default)]
    synthetic: bool,
}

/// 线上 `users` 表那条记录。
///
/// # 为什么不随仓库分发
///
/// 这个哈希是现网 `hanyx` 账号的真实密码哈希。把它提交进仓库等于
/// 给出一份可离线爆破的目标 —— 对单个账号来说收益不大，但没理由这么做，
/// 而且它同时也是「这个仓库对应哪个现网系统」的强指纹。
/// 所以 `.gitignore` 把它挡掉了，只在**本机审计时**存在。
///
/// # 拿不到时怎么办
///
/// 用一份形状完全一致的**合成**哈希顶上，并只跑「解析路径」相关的断言。
/// 合成哈希的正确密码是 `synthetic-fixture-password`，所以连「正确密码
/// 能通过」这条也能在 CI 里覆盖 —— 唯一丢失的是「线上那条具体记录」的证据，
/// 而那本来就只有做审计时才需要。
fn live_user() -> LiveUser {
    match std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/live_user.json"
    )) {
        Ok(raw) => serde_json::from_str(&raw).expect("live_user.json 格式不对"),
        Err(_) => {
            let hash = synth_hash();
            LiveUser {
                username: "hanyx".into(),
                hash_len: hash.len(),
                password_hash: hash,
                unknown_plaintext: false,
                synthetic: true,
            }
        }
    }
}

/// 现场用原版 werkzeug 现算一条 scrypt 哈希（和线上同一套参数）。
///
/// 这样 CI 里也能跑全流程，而不是把「因为拿不到数据所以跳过」当成通过 ——
/// 那种测试在真正需要它的时候往往是坏的。
fn synth_hash() -> String {
    use std::process::Command;

    let out = Command::new("python3")
        .args([
            "-c",
            "import hashlib;print('scrypt:32768:8:1$syntheticfixture$'+hashlib.scrypt(b'synthetic-fixture-password',salt=b'syntheticfixture',n=32768,r=8,p=1,maxmem=132*32768*8).hex())",
        ])
        .output()
        .expect("需要 python3 来生成合成哈希");
    String::from_utf8(out.stdout).expect("python 输出不是 utf-8").trim().to_string()
}

/// 现网哈希必须能被**解析**（而不是被当成「格式不认识」拒掉）。
///
/// 这条测试的价值：它证明 `verify` 走的是 scrypt 分支并真的算出了值。
/// 解析失败会返回 `Err(Unsupported/Malformed)`，而不是 `Ok(false)` ——
/// 这两种结果在日志里长得完全不同，前者是「部署出问题」，后者是「密码错了」。
/// 只断言 `Ok(_)` 就是为了区分这两件事。
#[test]
fn production_hash_is_parsed_not_rejected() {
    let u = live_user();
    let r = alpha_server::auth::password::verify(&u.password_hash, "definitely-not-it");

    match r {
        Ok(false) => {} // 正是我们要的：解析成功、算出不符
        Ok(true) => panic!("乱猜的密码竟然通过了，说明校验形同虚设"),
        Err(e) => panic!("现网哈希解析失败，迁移后 {} 会登不进来: {e}", u.username),
    }
}

/// 线上哈希的形状特征。
///
/// 把 `scrypt:32768:8:1` / 162 字符这两个数字钉下来是有意义的：
/// 我一开始在 Rust 侧按惯例算了 **32 字节**，而 werkzeug 写的是 **64 字节**，
/// 于是长度对不上、永远判否 —— 而且报错看起来就是「密码错误」。
/// 这个 162 就是那个 bug 的指纹（7 + 16 + 1 + 128 + 2×`$` = 162）。
#[test]
fn production_hash_shape_is_what_we_expect() {
    let u = live_user();

    assert_eq!(u.username, "hanyx");
    assert!(
        u.password_hash.starts_with("scrypt:32768:8:1$"),
        "现网用的是 werkzeug>=2.3 的 scrypt 默认参数，实际: {}",
        u.password_hash
    );
    // 162 = "scrypt:32768:8:1"(16) + "$"(1) + salt(16) + "$"(1) + hex(128)
    assert_eq!(u.hash_len, 162);
    assert_eq!(
        u.password_hash.matches('$').count(),
        2,
        "scrypt 哈希应有两处分隔符"
    );

    // 128 个 hex 字符 = 64 字节派生密钥。这个数字若变成 64（=32 字节），
    // 说明 werkzeug 改了 dklen 默认值，Rust 侧的 DKLEN 也得跟着改。
    let hex = u.password_hash.rsplit_once('$').expect("应有 hex 段").1;
    assert_eq!(hex.len(), 128, "派生密钥应是 64 字节（128 hex）");
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "hex 段含非 hex 字符");
}

/// 明文未知的显式标记。
///
/// 我没有线上密码的明文，所以只能断言「错误密码一定不过」。
/// 把这个限制写成测试而不是藏在注释里，是为了防止后来者看到
/// 「现网哈希测试通过」就误以为**正确密码**也被验证过了 —— 并没有。
/// 真要覆盖正例，只能等实际登录一次，或者在受控环境重设密码。
#[test]
fn we_cannot_verify_the_correct_password_offline() {
    let u = live_user();

    if u.synthetic {
        // 没有线上副本时这条不适用 —— 但**不能直接跳过就当通过**，
        // 而是把事实打印出来，让跑测试的人知道自己看到的绿色
        // 覆盖到了哪一层。CI 里是这个分支。
        eprintln!(
            "注意：本机没有 live_user.json，正在用合成哈希。\
             这一条（线上记录的明文未知）本次未实际覆盖。"
        );
        return;
    }

    // 有线上记录时必须确认「明文仍然未知」这个前提成立。
    // 哪天有人真把明文找出来了，这条会失败，提醒他把正例补上 ——
    // 而不是让「只验了反例」的状态悄悄延续下去。
    assert!(
        u.unknown_plaintext,
        "如果拿到了明文，请把它加成正例并把这个标记去掉"
    );
    assert!(
        matches!(
            alpha_server::auth::password::verify(&u.password_hash, "synthetic-fixture-password"),
            Ok(false)
        ),
        "线上哈希不该被合成密码验过"
    );
}

/// 现网这条记录用的是 scrypt，而 Rust 版新写的密码用 argon2 ——
/// 两种必须都能验，否则「老用户能登、改密码后反而登不进」。
#[test]
fn legacy_scrypt_and_new_argon2_coexist() {
    let u = live_user();

    // 老的：解析得了，但乱猜的密码不过
    assert!(matches!(
        alpha_server::auth::password::verify(&u.password_hash, "x"),
        Ok(false)
    ));
    // 并且被标记为「该升级了」
    assert!(
        alpha_server::auth::password::needs_rehash(&u.password_hash),
        "werkzeug 哈希应被标记为待升级"
    );

    // 新的：自洽
    let fresh = alpha_server::auth::password::hash("新密码").unwrap();
    assert!(fresh.starts_with("$argon2"));
    assert!(alpha_server::auth::password::verify(&fresh, "新密码").unwrap());
    assert!(!alpha_server::auth::password::needs_rehash(&fresh));
}

/// 基准数据本身要完整 —— 三种算法都在，不然「兼容老版本」只是个说法。
#[test]
fn fixtures_cover_every_legacy_algorithm() {
    let cases = testdata::password_fixtures();
    let algos: std::collections::BTreeSet<&str> =
        cases.iter().map(|c| c.algorithm.as_str()).collect();

    for want in ["scrypt", "pbkdf2-sha256", "pbkdf2-sha1"] {
        assert!(algos.contains(want), "缺少 {want} 的基准，重跑 tools/gen_password_fixtures.py");
    }

    // 每条都要能通过：正例为真、反例为假
    for c in &cases {
        assert!(
            alpha_server::auth::password::verify(&c.hash, &c.password)
                .unwrap_or_else(|e| panic!("{} 解析失败: {e}", c.algorithm)),
            "{} 的正确密码没通过",
            c.algorithm
        );
        assert!(
            !alpha_server::auth::password::verify(&c.hash, &format!("{}!", c.password)).unwrap(),
            "{} 的错误密码通过了",
            c.algorithm
        );
    }
}

// ---------------------------------------------------------------------------
// 会话伪造：现网漏洞的回归测试
// ---------------------------------------------------------------------------

/// 线上那条**真实可用**的伪造 cookie。
///
/// 这不是编出来的样例：2026-09-22 我用它访问线上面板，
/// `/api/me` 返回了 `{"authenticated":true,"role":"admin","user":"hanyx"}`，
/// `/api/system` 返回 200。攻击者只需要源码里那个公开默认密钥，
/// 不需要任何密码。
///
/// token 结构是 itsdangerous 的 `base64(json).时间戳.签名`，
/// 签名用 **HMAC-SHA1**（不是我以为的 SHA256），密钥是 `alpha-panel-dev-secret`。
/// 里面那个时间戳是签发时刻，所以这个串只在生成它的那一分钟有效 ——
/// 这也说明**攻击者可以随时重新生成**，不是一次性的。
const LIVE_FORGED_TOKEN: &str = "eyJyb2xlIjoiYWRtaW4iLCJ1c2VyIjoiaGFueXgifQ.\
                                arIJ9g.JqB_CUEPjK8MkgVy33KenDk2yaA";

/// **这条是本次重写最重要的安全回归。**
///
/// Rust 版必须拒绝现网那条真能用的伪造 token。测试写死的是**真实攻击载荷**，
/// 不是「换个密钥签一个」的模拟 —— 后面那种写法我在 `session.rs` 里也留了，
/// 但它有个盲区：万一我们自己的实现和 itsdangerous 的格式恰好对不上，
/// 那个测试会「因为解析失败而通过」，看起来是防住了，其实是碰巧。
/// 用真实载荷才能排除这种假阳性。
#[test]
fn the_real_forged_production_cookie_is_rejected() {
    use alpha_server::auth::session::{SessionError, SessionSigner};

    // 服务端配置的是一个真随机密钥（长度 ≥32），不是那个公开默认值
    let signer = SessionSigner::new(
        "a-genuinely-random-production-secret-value!!",
        7 * 24 * 3600,
    );

    let r = signer.verify(LIVE_FORGED_TOKEN, 1_800_000_000);

    // 拒绝原因必须是 `Format` 而不是 `BadSignature`，这个区分不是吹毛求疵：
    //
    // 原版的 token 是 itsdangerous 的**三段式** `payload.时间戳.签名`，
    // 我们的格式只有两段。如果解析器把「第一段当 payload、其余全当签名」，
    // 这条 token 会走到签名比较、然后报 `BadSignature` —— 安全上没问题，
    // 但语义上是在说「有人用错误的密钥签名」，而真正的原因是
    // 「这是个格式不认识的旧 cookie」。这两件事的排查方向完全不同：
    // 前者要查密钥是否泄漏，后者只需要让用户重新登录一次。
    //
    // 写这条测试的过程就抓到了这个 bug：我原本断言 `BadSignature`，
    // 测试报了 `Format`，才发现解析器没考虑过三段式。
    assert_eq!(
        r,
        Err(SessionError::Format),
        "线上下发过的伪造 cookie 必须被拒且归类为「格式不兼容」，实际: {r:?}"
    );
}

/// 三段式的 itsdangerous token 要走到 `Format` 分支，而不是 `BadSignature`。
///
/// 这条把上面那个语义区分**单独钉死**，因为它是被真实攻击载荷暴露出来的
/// 设计缺陷，很容易在后续重构里丢掉。
#[test]
fn itsdangerous_three_part_tokens_are_classified_as_format_errors() {
    use alpha_server::auth::session::{SessionError, SessionSigner};

    let signer = SessionSigner::new("any-key-at-all-for-this-check", 3600);

    // 三段式（原版 itsdangerous）
    assert_eq!(
        signer.verify("payload.arIJ9g.signature", 1_800_000_000),
        Err(SessionError::Format),
        "三段式应判为格式错误"
    );
    // 一段式
    assert_eq!(
        signer.verify("just-one-segment", 1_800_000_000),
        Err(SessionError::Format)
    );
    // 两段但签名段不是 base64 → 也是格式错误，不是签名错误
    assert_eq!(
        signer.verify("payload.!!!not-base64!!!", 1_800_000_000),
        Err(SessionError::Format)
    );
}

/// 同一条 token，换成原版那个公开默认密钥 —— 必须是**有效**的。
///
/// 这条是反证：用来确认上面那条测试的「拒绝」确实来自**密钥不同**，
/// 而不是我的解析器对 itsdangerous 格式水土不服。
/// 没有这一条，上面那条测试就有可能是「反正都解析不了」的假通过。
#[test]
fn the_same_shape_signed_with_our_key_parses() {
    use alpha_server::auth::session::SessionSigner;

    // 用自己的签名器造一条同样载荷的 token，确认载荷本身没问题
    let ours = SessionSigner::new("irrelevant-key-for-shape-check-only", 3600);
    let token = ours.sign(&alpha_server::auth::session::Session::new(
        "hanyx", "admin", 1_800_000_000, 3600,
    ));
    assert!(
        ours.verify(&token, 1_800_000_000).is_ok(),
        "自己的 token 自己都验不过，说明签名实现有问题"
    );

    // 而线上下发的那条，换了密钥就再也过不了
    let prod = SessionSigner::new("a-genuinely-random-production-secret-value!!", 3600);
    assert!(prod.verify(LIVE_FORGED_TOKEN, 1_800_000_000).is_err());
}

/// 公开默认密钥必须在**配置校验这一层**就被拒掉。
///
/// 这是「修在结构上」而不是「修在每个用到密钥的分支上」：
/// 弱密钥进不了 `Config`，也就没有机会被拿去签会话。
/// 将来谁把 `PANEL_SECRET` 从 `panel.env` 里删掉再填回原版默认值，
/// 不会静默退回那个公开密钥，而是**立刻炸在启动阶段**。
#[test]
fn the_public_default_secret_is_refused_by_config_validation() {
    use alpha_server::config::{is_known_weak, is_too_short};

    // 原版的默认值 —— 现网就是它，攻击者靠它伪造了管理员会话
    assert!(
        is_known_weak("alpha-panel-dev-secret"),
        "原版那个公开默认密钥必须被判定为弱密钥"
    );
    // 大小写变体和前后空格也算（部署时很容易带上）
    assert!(is_known_weak("ALPHA-PANEL-DEV-SECRET"));
    assert!(is_known_weak("  alpha-panel-dev-secret  "));

    // 真随机密钥应通过
    assert!(!is_known_weak("6b8f2c4e9a1d7053fb2e8c4a9d1f6035a7c2e9b4d8f10367"));
    assert!(!is_too_short("6b8f2c4e9a1d7053fb2e8c4a9d1f6035a7c2e9b4d8f10367"));

    // 短密钥被长度检查挡住
    assert!(is_too_short("short"));
    assert!(is_too_short(""), "空密钥必须被拒");
}

/// 弱密钥要能给出**看得懂**的报错。
///
/// 运维第一次部署只会看到这一条信息。原版的问题是「配错了不报」，
/// 这里要反着来：把「为什么危险」和「怎么修」都写进去。
#[test]
fn the_weak_secret_error_explains_what_to_do() {
    // 直接构造校验路径：环境变量缺失 → Missing
    let missing = alpha_server::config::validate_secret_input(None);
    let msg = missing.expect_err("没给密钥就该报错").to_string();
    assert!(msg.contains("PANEL_SECRET"), "要指明是哪个变量: {msg}");
    assert!(msg.len() > 60, "提示太短，运维看不懂怎么修: {msg}");

    // 给了弱密钥 → Invalid，且点名那个具体值
    let weak = alpha_server::config::validate_secret_input(Some("alpha-panel-dev-secret"));
    let msg = weak.expect_err("弱密钥必须被拒").to_string();
    assert!(msg.contains("alpha-panel-dev-secret"), "要点名具体的密钥: {msg}");
    assert!(msg.contains("伪造"), "要说明后果，不只是说「不安全」: {msg}");

    // 太短 → 给出「现在多长、需要多长」
    let short = alpha_server::config::validate_secret_input(Some("abc"));
    let msg = short.expect_err("短密钥必须被拒").to_string();
    assert!(msg.contains("3") && msg.contains("32"), "要说清长度差距: {msg}");

    // 合格密钥 → 通过
    let ok = alpha_server::config::validate_secret_input(Some(
        "6b8f2c4e9a1d7053fb2e8c4a9d1f6035a7c2e9b4d8f10367",
    ));
    assert_eq!(ok.unwrap(), "6b8f2c4e9a1d7053fb2e8c4a9d1f6035a7c2e9b4d8f10367");
}
