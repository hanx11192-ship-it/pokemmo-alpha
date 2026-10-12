//! 会话签发与校验。
//!
//! # 为什么不直接用原版那套「Flask 签名 cookie」
//!
//! 原版把 `{user, role}` 交给 `itsdangerous` 签名后整个塞进 cookie。
//! 问题是**签名密钥有公开默认值**，且 systemd 没注入环境变量覆盖它，
//! 于是任何人都能手搓一个 `{"user":"hanyx","role":"admin"}` 直接登进后台。
//! 我已在现网验证过（详情见仓库根目录的审计记录）。
//!
//! 本版保留「签名 cookie」这个形态（无状态、不查库、水平扩展友好），
//! 但把密钥变成**必填且校验强度**的启动参数，密钥缺失直接拒绝启动。
//! 同时补上原版没有的两样东西：
//!
//! 1. **过期时间**。原版 cookie 签发后永久有效，关了浏览器再打开还在，
//!    离职员工的浏览器只要没清 cookie 就一直是管理员。这里带 `exp`。
//! 2. **角色不放进 cookie 的信任边界之外**。原版 cookie 里直接带 `role`，
//!    改一下就能提权 —— 虽然改不了（有签名），但把权限塞进客户端数据
//!    终究是坏习惯。这里仍带 `role` 但**只当缓存**，真正的权限判断
//!    走数据库里的 `users.role`（见 `routes`）。

use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// 会话 cookie 名。沿用原版的名字 —— 换名字会让老用户的浏览器里
/// 留着一个无用的旧 cookie，虽然无害，但没必要制造混乱。
pub const COOKIE_NAME: &str = "session";

/// 默认会话有效期：7 天。
///
/// 选 7 天是个折中：管理员是运维自己，每天都要看面板，
/// 一天一登太烦；再长的话，一台落灰的平板电脑能一直登着。
/// 真正的强约束应该靠「不共用设备」，cookie 有效期只能兜底。
pub const DEFAULT_TTL: i64 = 7 * 24 * 3600;

/// 会话里的用户信息。
///
/// 字段名用短的（`u`/`r`）是为了让 cookie 体积小一点 —— 每个请求都要带。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// 用户名。
    pub u: String,
    /// 角色，`admin` 或普通角色。**仅作缓存**，鉴权时以数据库为准。
    pub r: String,
    /// 签发时间（Unix 秒）。
    pub iat: i64,
    /// 过期时间（Unix 秒）。
    pub exp: i64,
}

impl Session {
    /// 新建一个会话。
    pub fn new(user: impl Into<String>, role: impl Into<String>, now: i64, ttl: i64) -> Self {
        Self {
            u: user.into(),
            r: role.into(),
            iat: now,
            exp: now + ttl,
        }
    }

    /// 是否已过期。
    ///
    /// 注意这里用的是**服务器本地时间**，而会话是无状态的：
    /// 多实例部署时钟不同步会导致行为不一致。面板是单机部署，
    /// 这条不构成实际问题，但如果将来水平扩展，得换成 NTP。
    pub fn is_expired(&self, now: i64) -> bool {
        now >= self.exp
    }

    pub fn is_admin(&self) -> bool {
        self.r == "admin"
    }
}

/// 会话编解码错误。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("cookie 格式不对")]
    Format,

    #[error("签名不匹配")]
    BadSignature,

    #[error("会话已过期")]
    Expired,
}

/// 会话签名器。持有密钥，负责签发与校验。
///
/// 密钥来自 [`crate::config`]，那边已经保证它够长、不是已知弱密钥。
#[derive(Clone)]
pub struct SessionSigner {
    key: Vec<u8>,
    ttl: i64,
}

impl std::fmt::Debug for SessionSigner {
    /// 手写 `Debug` 以免密钥被意外打进日志。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionSigner")
            .field("key", &"<已隐藏>")
            .field("ttl", &self.ttl)
            .finish()
    }
}

/// cookie 里的 payload 分隔符。用 `.` 是 JWT 的惯例，看着眼熟。
const SEP: char = '.';

impl SessionSigner {
    pub fn new(key: impl Into<Vec<u8>>, ttl: i64) -> Self {
        Self {
            key: key.into(),
            ttl,
        }
    }

    pub fn ttl(&self) -> i64 {
        self.ttl
    }

    /// 签发：`base64url(json).base64url(hmac)`。
    ///
    /// 用 URL-safe 的 base64 且**不带 padding**：cookie 值里出现 `=`
    /// 虽然合法，但某些反向代理和客户端的 cookie 解析会对它做意外处理，
    /// 去掉 padding 省一类玄学问题。
    pub fn sign(&self, session: &Session) -> String {
        let json = serde_json::to_vec(session).expect("Session 只有简单字段，不可能序列化失败");
        let payload = b64(&json);
        let sig = self.mac(payload.as_bytes());
        format!("{payload}{SEP}{}", b64(&sig))
    }

    /// 校验并解出会话内容。
    ///
    /// # 为什么显式拒绝三段式 token
    ///
    /// Flask 的 `itsdangerous` 用的是 `payload.时间戳.签名`。如果我们只按
    /// 「第一段是 payload、其余是签名」去解析，一条**原版的** token
    /// 会走进「签名不匹配」这个分支 —— 结果是安全的，但错误原因会误导人：
    /// 看上去像「有人拿错密钥伪造」，实际只是「格式不兼容的旧 cookie」。
    /// 这两种情况在排查时该走完全不同的路，所以这里给出明确的 `Format`。
    ///
    /// 我就是在写安全回归测试时发现这个问题的：拿线上一条真实的伪造 token
    /// 打过来，得到的是 `BadSignature` 还是 `Format` 被我写成了断言，
    /// 结果暴露出解析器压根没考虑过三段式。
    pub fn verify(&self, token: &str, now: i64) -> Result<Session, SessionError> {
        // 先单独挡掉三段式（原版 itsdangerous 格式）
        if token.matches(SEP).count() > 1 {
            return Err(SessionError::Format);
        }

        let (payload, sig_b64) = token.split_once(SEP).ok_or(SessionError::Format)?;

        // 签名段必须是能解出来的 base64。解不出来说明这压根不是我们的格式，
        // 不该报「签名不匹配」—— 那会让人以为是密钥问题。
        let sig = unb64(sig_b64).ok_or(SessionError::Format)?;
        let expected = self.mac(payload.as_bytes());

        // 常量时间比较：普通 `==` 会在第一个不同字节处返回，
        // 攻击者能通过响应时间逐字节猜出正确签名。
        if sig.len() != expected.len() || !bool::from(sig.ct_eq(&expected)) {
            return Err(SessionError::BadSignature);
        }

        let json = unb64(payload).ok_or(SessionError::Format)?;
        let session: Session =
            serde_json::from_slice(&json).map_err(|_| SessionError::Format)?;

        if session.is_expired(now) {
            return Err(SessionError::Expired);
        }

        Ok(session)
    }

    fn mac(&self, data: &[u8]) -> Vec<u8> {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.key)
            .expect("HMAC 接受任意长度密钥");
        mac.update(data);
        mac.finalize().into_bytes().to_vec()
    }
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

fn unb64(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "this-is-a-test-key-at-least-32-bytes-long";
    const NOW: i64 = 1_700_000_000;

    fn signer() -> SessionSigner {
        SessionSigner::new(KEY, DEFAULT_TTL)
    }

    #[test]
    fn round_trips() {
        let s = Session::new("hanyx", "admin", NOW, DEFAULT_TTL);
        let token = signer().sign(&s);
        let got = signer().verify(&token, NOW).unwrap();
        assert_eq!(got, s);
    }

    /// **回归测试：伪造的 admin cookie 必须被拒绝。**
    ///
    /// 这就是现网正在被利用的漏洞。原版用公开默认密钥签名，
    /// 我本地就能造出一个 admin 会话并成功访问线上 `/api/me`。
    /// 这条测试保证：用错误的密钥签出来的 token，怎么都进不来。
    #[test]
    fn forged_session_with_a_different_key_is_rejected() {
        // 模拟攻击者：知道公开默认密钥，用它签一个 admin 会话
        let attacker = SessionSigner::new("alpha-panel-dev-secret", DEFAULT_TTL);
        let forged = attacker.sign(&Session::new("hanyx", "admin", NOW, DEFAULT_TTL));

        // 服务端用的是真密钥，必须拒绝
        let r = signer().verify(&forged, NOW);
        assert_eq!(r, Err(SessionError::BadSignature), "伪造会话竟然通过了");
    }

    /// 篡改 payload 但保留原签名 —— 签名校验必须发现。
    #[test]
    fn tampered_payload_is_rejected() {
        let real = signer().sign(&Session::new("guest", "viewer", NOW, DEFAULT_TTL));
        let (_, sig) = real.split_once(SEP).unwrap();

        // 攻击者把用户改成 admin，签名照抄
        let evil = serde_json::to_vec(&Session::new("hanyx", "admin", NOW, DEFAULT_TTL)).unwrap();
        let tampered = format!("{}{SEP}{sig}", b64(&evil));

        assert_eq!(
            signer().verify(&tampered, NOW),
            Err(SessionError::BadSignature)
        );
    }

    /// 原版会话永不过期。这里必须过期。
    #[test]
    fn expired_session_is_rejected() {
        let s = Session::new("hanyx", "admin", NOW, 3600);
        let token = signer().sign(&s);

        // 有效期最后一秒还能用
        assert!(signer().verify(&token, NOW + 3599).is_ok());
        // 到点就失效
        assert_eq!(
            signer().verify(&token, NOW + 3600),
            Err(SessionError::Expired)
        );
        assert_eq!(
            signer().verify(&token, NOW + 999_999),
            Err(SessionError::Expired)
        );
    }

    /// 过期会话和签名错误要能区分 —— 前端据此决定「静默重登」还是
    /// 「提示登录已过期」，运维排查时也能看出是哪种。
    #[test]
    fn expiry_is_distinguishable_from_a_bad_signature() {
        let token = signer().sign(&Session::new("u", "admin", NOW, 60));
        assert_eq!(signer().verify(&token, NOW + 61), Err(SessionError::Expired));
        assert_eq!(
            signer().verify(&token, NOW),
            Ok(Session::new("u", "admin", NOW, 60))
        );
    }

    #[test]
    fn malformed_tokens_error_instead_of_panicking() {
        for bad in [
            "",
            ".",
            "no-separator",
            "a.b.c",
            "!!!.???",
            "aGVhZGVy.",   // 有 payload 无签名
            ".c2ln",       // 有签名无 payload
        ] {
            let r = signer().verify(bad, NOW);
            assert!(
                matches!(r, Err(SessionError::Format | SessionError::BadSignature)),
                "{bad:?} 应被拒绝，实际 {r:?}"
            );
        }
    }

    /// 空密钥是灾难（HMAC 密钥为空时，谁都能签）。
    /// `config` 那边已经挡住空密钥，这里确认签名器本身也不会退化。
    #[test]
    fn empty_key_still_produces_a_distinct_signature() {
        let a = SessionSigner::new("", DEFAULT_TTL);
        let b = SessionSigner::new("x", DEFAULT_TTL);
        let s = Session::new("u", "admin", NOW, 60);

        assert_ne!(a.sign(&s), b.sign(&s));
        // 空密钥签的东西只有空密钥能验
        assert!(a.verify(&a.sign(&s), NOW).is_ok());
        assert_eq!(
            b.verify(&a.sign(&s), NOW),
            Err(SessionError::BadSignature)
        );
    }

    #[test]
    fn cookie_name_is_unchanged_from_the_original() {
        assert_eq!(COOKIE_NAME, "session");
    }

    /// 密钥不能出现在 `Debug` 输出里 —— 不然一条 `tracing::debug!` 就泄了。
    #[test]
    fn debug_does_not_leak_the_key() {
        let text = format!("{:?}", SessionSigner::new("super-secret-key-value", 60));
        assert!(!text.contains("super-secret-key-value"), "密钥泄漏到 Debug 了: {text}");
        assert!(text.contains("已隐藏"));
    }

    /// 签名与 payload 之间用 `.` 分隔，且 base64 不带 padding。
    /// 带 `=` 在部分反代上会被截断或改写。
    #[test]
    fn token_shape_is_proxy_safe() {
        let token = signer().sign(&Session::new("hanyx", "admin", NOW, DEFAULT_TTL));
        assert_eq!(token.matches(SEP).count(), 1, "分隔符应只有一个");
        assert!(!token.contains('='), "不应有 base64 padding: {token}");
        assert!(!token.contains('+') && !token.contains('/'), "应是 URL-safe 字母表");
    }

    /// 换个密钥（比如运维轮换了 `PANEL_SECRET`）旧会话立即全部失效 ——
    /// 这是轮换密钥时的预期行为，也是应急踢掉所有会话的手段。
    #[test]
    fn rotating_the_key_invalidates_every_session() {
        let old = SessionSigner::new("old-key-old-key-old-key-old-key!!", DEFAULT_TTL);
        let new = SessionSigner::new("new-key-new-key-new-key-new-key!!", DEFAULT_TTL);
        let token = old.sign(&Session::new("hanyx", "admin", NOW, DEFAULT_TTL));

        assert!(old.verify(&token, NOW).is_ok());
        assert_eq!(new.verify(&token, NOW), Err(SessionError::BadSignature));
    }

    #[test]
    fn is_admin_checks_the_role() {
        assert!(Session::new("u", "admin", NOW, 60).is_admin());
        assert!(!Session::new("u", "viewer", NOW, 60).is_admin());
        assert!(!Session::new("u", "", NOW, 60).is_admin());
    }
}
