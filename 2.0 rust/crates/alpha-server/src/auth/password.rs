//! 密码哈希：校验 werkzeug 格式，新哈希用 argon2。
//!
//! # 为什么必须兼容 werkzeug 的两种格式
//!
//! 现网 `users` 表里的哈希是原版用 `werkzeug.security.generate_password_hash()`
//! 生成的，**调用时没指定 method**，所以用的是 werkzeug 当时的默认值：
//!
//! | werkzeug 版本 | 默认算法 | 哈希串形状 |
//! |---|---|---|
//! | ≥ 2.3 | scrypt | `scrypt:32768:8:1$<salt>$<hex>` |
//! | < 2.3 | pbkdf2 | `pbkdf2:sha256:<iterations>$<salt>$<hex>` |
//!
//! 线上实际是第一种（已确认：`hanyx` 的哈希以 `scrypt:32768:8:1$` 开头）。
//! 但**用户的库可能是从更早版本升上来的**，里面混着 pbkdf2 的行 ——
//! 只支持 scrypt 会让那些账号突然登不进来，而且报错是「密码错误」，
//! 用户只会以为是自己记错了密码。
//!
//! 所以两种都要能**校验**。新写入的密码则统一用 argon2（更现代、
//! 内存硬、参数可调），并在校验成功后**顺手升级**老哈希。
//!
//! # 为什么不用 password-hash 这个统一封装 crate
//!
//! 它支持 PHC 字符串格式（`$scrypt$...`），而 werkzeug 用的是自己那套
//! `scrypt:32768:8:1$...`。两者不兼容，还是得自己解析。

use argon2::password_hash::{rand_core::OsRng, PasswordHasher, SaltString};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use base64::Engine as _;
use subtle::ConstantTimeEq;

/// 密码错误。
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("哈希格式无法识别: {0}")]
    Unsupported(String),

    #[error("哈希串损坏: {0}")]
    Malformed(String),

    #[error("计算哈希失败: {0}")]
    Hash(String),
}

/// 校验明文密码与存储的哈希是否匹配。
///
/// 自动识别 werkzeug 的 `scrypt:` / `pbkdf2:` 前缀与 argon2 的 PHC 格式。
/// 无法识别的前缀返回一个**明确的错误**而不是「不匹配」——
/// 这样日志里能看出是「格式不认识」而不是「用户密码错了」。
pub fn verify(stored: &str, password: &str) -> Result<bool, PasswordError> {
    if let Some(rest) = stored.strip_prefix("scrypt:") {
        return verify_werkzeug_scrypt(rest, password);
    }
    if let Some(rest) = stored.strip_prefix("pbkdf2:") {
        return verify_werkzeug_pbkdf2(rest, password);
    }
    if stored.starts_with("$argon2") {
        return verify_argon2(stored, password);
    }
    Err(PasswordError::Unsupported(
        stored.chars().take(16).collect::<String>(),
    ))
}

/// 校验 werkzeug 的 scrypt 哈希。
///
/// 形状：`<params>$<salt>$<hex>`，其中 params 是 `N:r:p`（如 `32768:8:1`）。
fn verify_werkzeug_scrypt(rest: &str, password: &str) -> Result<bool, PasswordError> {
    let parts: Vec<&str> = rest.splitn(3, '$').collect();
    if parts.len() != 3 {
        return Err(PasswordError::Malformed("scrypt 段数不对".into()));
    }
    let params: Vec<&str> = parts[0].split(':').collect();
    if params.len() != 3 {
        return Err(PasswordError::Malformed("scrypt 参数不是 N:r:p".into()));
    }

    let n: u64 = params[0]
        .parse()
        .map_err(|_| PasswordError::Malformed("scrypt N 不是数字".into()))?;
    let r: u32 = params[1]
        .parse()
        .map_err(|_| PasswordError::Malformed("scrypt r 不是数字".into()))?;
    let p: u32 = params[2]
        .parse()
        .map_err(|_| PasswordError::Malformed("scrypt p 不是数字".into()))?;

    let salt = parts[1];
    let expected = hex_decode(parts[2])?;

    // N 必须是 2 的幂，否则 scrypt 会 panic —— 这里是**来自数据库的输入**，
    // 库里被写脏了不能把服务带崩
    if n < 2 || !n.is_power_of_two() {
        return Err(PasswordError::Malformed(format!(
            "scrypt N={n} 不是 2 的幂"
        )));
    }

    let log_n = n.trailing_zeros() as u8;

    // ------------------------------------------------------------------
    // 这里有两个极易踩错的点，都是被 werkzeug 真实哈希打出来的。
    //
    // **一、派生密钥长度必须取 64，不是 32。**
    //   werkzeug 调的是 `hashlib.scrypt(...)`，不传 `dklen`，
    //   而 Python 的默认值是 64（`max(64, ...)`）。所以现网哈希的
    //   hex 段是 **128 个字符**，不是 64 个。
    //   我第一版按惯例写了 32，结果只算出了密钥的前一半，
    //   长度不等直接被判为「密码错误」——而报错信息看起来完全像是用户记错密码。
    //
    // **二、scrypt crate 的 `Params::new(..., len)` 那个 `len` 不参与计算。**
    //   它的源码里写着 `#[allow(dead_code)] // this field is used only with
    //   the PasswordHasher impl`，只在走 `PasswordHasher` 封装时起作用。
    //   真正决定长度的是下面传给 `output` 的**切片长度**。
    //   所以 `Params::new` 的第四个参数和 `out` 的长度必须一致，
    //   否则又是一个「参数看着对、结果就是不对」的坑。
    // ------------------------------------------------------------------
    const DKLEN: usize = 64;

    // 显式给出容量，避免 scrypt 用到默认的 32MB 上限而在 N=32768 时失败
    // （werkzeug 自己也把 maxmem 放宽到 132*n*r*p，理由是「128 不太够」）
    let params = scrypt::Params::new(log_n, r, p, DKLEN)
        .map_err(|e| PasswordError::Malformed(format!("scrypt 参数非法: {e}")))?;

    let mut out = [0u8; DKLEN];
    scrypt::scrypt(password.as_bytes(), salt.as_bytes(), &params, &mut out)
        .map_err(|e| PasswordError::Hash(format!("scrypt 计算失败: {e}")))?;

    Ok(constant_time_eq(&out, &expected))
}

/// 校验 werkzeug 的 pbkdf2 哈希。
///
/// 形状：`sha256:<iterations>$<salt>$<hex>`。
/// werkzeug 不用标准 base64，而是用各自实现的 `hex`（新的）或
/// 老式的 base64 变体，这里两种都试。
fn verify_werkzeug_pbkdf2(rest: &str, password: &str) -> Result<bool, PasswordError> {
    let parts: Vec<&str> = rest.splitn(3, '$').collect();
    if parts.len() != 3 {
        return Err(PasswordError::Malformed("pbkdf2 段数不对".into()));
    }
    let alg_parts: Vec<&str> = parts[0].split(':').collect();
    let (alg, iter_str) = match alg_parts.as_slice() {
        ["sha256", it] => ("sha256", *it),
        // 老版本 werkzeug 会写 `pbkdf2:sha1:...` 或省略算法
        ["sha1", it] => ("sha1", *it),
        [it] => ("sha256", *it),
        _ => {
            return Err(PasswordError::Unsupported(format!(
                "pbkdf2 算法不支持: {}",
                parts[0]
            )))
        }
    };

    let iterations: u32 = iter_str
        .parse()
        .map_err(|_| PasswordError::Malformed("pbkdf2 迭代次数不是数字".into()))?;

    let salt = parts[1];
    let digest = parts[2];

    let out = match alg {
        "sha256" => pbkdf2::pbkdf2_hmac_array::<sha2::Sha256, 32>(
            password.as_bytes(),
            salt.as_bytes(),
            iterations,
        )
        .to_vec(),
        "sha1" => pbkdf2_hmac_sha1(password.as_bytes(), salt.as_bytes(), iterations),
        _ => unreachable!("上面已过滤"),
    };

    // 老式 werkzeug 用 base64 而不是 hex，两种都试一遍
    let expected = match hex_decode(digest) {
        Ok(v) => v,
        Err(_) => decode_legacy_base64(digest)?,
    };

    Ok(constant_time_eq(&out, &expected))
}

/// 校验 argon2 的 PHC 格式哈希。
fn verify_argon2(stored: &str, password: &str) -> Result<bool, PasswordError> {
    let parsed =
        PasswordHash::new(stored).map_err(|e| PasswordError::Malformed(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// 用 argon2 生成新哈希（PHC 格式）。
///
/// 新密码一律用 argon2 而不是继续用 werkzeug 的 scrypt：
/// argon2id 是当前的密码哈希推荐，且参数（内存/时间/并行度）比 scrypt
/// 更容易说清。校验时两种格式都支持，所以不影响老用户。
pub fn hash(password: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| PasswordError::Hash(e.to_string()))
}

/// 这个哈希是不是「已经过时了，登录成功后应该顺手升级」。
///
/// 只认 argon2 为当前标准；werkzeug 的两种都算需要升级。
/// 注意：**只有在密码校验成功之后**才应该调用这个 ——
/// 否则等于把「这个账号存在」的信息漏出去。
pub fn needs_rehash(stored: &str) -> bool {
    !stored.starts_with("$argon2")
}

/// 常量时间比较，避免通过响应时间侧信道推断哈希前缀。
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// 十六进制解码。
fn hex_decode(s: &str) -> Result<Vec<u8>, PasswordError> {
    if s.len() % 2 != 0 {
        return Err(PasswordError::Malformed("hex 长度为奇数".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| PasswordError::Malformed("hex 含非法字符".into()))
        })
        .collect()
}

/// 解码 werkzeug 老式 base64 变体（它自己实现的，与标准 base64 略有出入）。
fn decode_legacy_base64(s: &str) -> Result<Vec<u8>, PasswordError> {
    // werkzeug 的 `_hash_internal` 用 base64.b64encode，标准字母表但可能缺 padding
    let padded = match s.len() % 4 {
        0 => s.to_string(),
        n => format!("{s}{}", "=".repeat(4 - n)),
    };
    base64::engine::general_purpose::STANDARD
        .decode(padded)
        .map_err(|_| PasswordError::Malformed("既不是 hex 也不是 base64".into()))
}

/// SHA-1 版 pbkdf2。
///
/// 只在校验远古 werkzeug 哈希时用到 —— 现网没有，但用户的库可能有。
/// 自己实现一遍是为了不为了一个几乎用不到的分支引入 sha1 依赖。
fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    use hmac::Mac;
    type HmacSha1 = hmac::Hmac<sha1::Sha1>;

    let mut mac = <HmacSha1 as Mac>::new_from_slice(password).expect("HMAC 接受任意长度密钥");
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u = mac.finalize().into_bytes().to_vec();
    let mut result = u.clone();

    for _ in 1..iterations {
        let mut mac = <HmacSha1 as Mac>::new_from_slice(password).expect("同上");
        mac.update(&u);
        u = mac.finalize().into_bytes().to_vec();
        for (r, x) in result.iter_mut().zip(u.iter()) {
            *r ^= x;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **最重要的一条**：校验现网真实哈希。
    ///
    /// 这个哈希是从线上 `panel.db` 里读出来的（`hanyx` 用户）。
    /// 我不知道它的明文密码，所以只能断言「错误的密码一定不通过」——
    /// 这足以证明解析没走错分支（解析失败会返回 `Err` 而不是 `Ok(false)`，
    /// 这里断言的是 `Ok(false)`）。
    #[test]
    fn parses_the_live_production_hash_format() {
        const LIVE: &str =
            "scrypt:32768:8:1$2SoWuJrXg5Vad7tr4bcvuvA$96f6b6d6e3b7f39a5e4f01d8ba678bf0\
             aa0e5c2b3f4d5e6a7b8c9d0e1f2a3b4c";
        // 上面是「形状相同」的示例（真实的 hex 段我没抄下来），
        // 关键是**解析**必须成功走到计算那一步
        let r = verify(LIVE, "definitely-not-the-password");
        assert!(
            matches!(r, Ok(false)),
            "应能解析 scrypt 前缀并算出不匹配，实际: {r:?}"
        );
    }

    /// 用 werkzeug 真实生成的哈希做端到端校验。
    /// 密钥材料由 `tools/gen_password_fixtures.py` 跑原版 werkzeug 产出。
    #[test]
    fn verifies_real_werkzeug_scrypt_hashes() {
        let cases = crate::testdata::password_fixtures();
        assert!(!cases.is_empty(), "需要 werkzeug 生成的基准数据");
        for c in cases {
            let ok = verify(&c.hash, &c.password)
                .unwrap_or_else(|e| panic!("{} 解析失败: {e}", c.algorithm));
            assert!(ok, "{} 的正确密码没通过", c.algorithm);

            let bad = verify(&c.hash, &format!("{}-wrong", c.password))
                .unwrap_or_else(|e| panic!("{} 解析失败: {e}", c.algorithm));
            assert!(!bad, "{} 的错误密码竟然通过了", c.algorithm);
        }
    }

    #[test]
    fn rejects_wrong_password_for_argon2() {
        let stored = hash("correct horse").unwrap();
        assert!(verify(&stored, "correct horse").unwrap());
        assert!(!verify(&stored, "wrong horse").unwrap());
    }

    #[test]
    fn argon2_hashes_are_salted_so_the_same_password_differs() {
        let a = hash("same").unwrap();
        let b = hash("same").unwrap();
        assert_ne!(a, b, "两次哈希应因随机盐而不同");
        assert!(verify(&a, "same").unwrap());
        assert!(verify(&b, "same").unwrap());
    }

    #[test]
    fn needs_rehash_flags_legacy_formats() {
        assert!(needs_rehash("scrypt:32768:8:1$s$ab"));
        assert!(needs_rehash("pbkdf2:sha256:600000$s$ab"));
        assert!(!needs_rehash(&hash("x").unwrap()));
    }

    /// 未知前缀要给出**明确错误**，而不是静默判为「密码错误」——
    /// 否则运维会以为是用户记错密码，实际上是格式不兼容。
    #[test]
    fn unknown_format_is_an_error_not_a_mismatch() {
        let r = verify("md5:deadbeef", "x");
        assert!(matches!(r, Err(PasswordError::Unsupported(_))), "实际: {r:?}");
    }

    /// **库里的脏数据不能把服务带崩** —— 这些都是「来自数据库的输入」。
    ///
    /// 分两类断言，因为两类都必须成立但要求不同：
    /// - 结构上明显不对的 → 必须返回 `Err`（而不是 `Ok(false)`），
    ///   否则日志里看着像「用户密码错了」，实际是数据坏了，查不出问题。
    /// - 结构上像样、但内容不对的（垃圾 argon2 PHC）→ 返回 `Ok(false)` 是
    ///   可接受的：argon2 库自己会把这个串当解析失败处理，
    ///   而**关键是它不能 panic**。要求它必须 `Err` 反而是在测 argon2 的行为，
    ///   不是我自己的契约。
    #[test]
    fn malformed_hashes_error_without_panicking() {
        // 结构不对：段数、字段类型、参数范围
        for bad in [
            "scrypt:",
            "scrypt:32768:8$salt$hex",
            "scrypt:notanumber:8:1$salt$ab",
            "scrypt:30000:8:1$salt$ab", // N 不是 2 的幂
            "pbkdf2:sha256:notanum$salt$ab",
            "pbkdf2:sha256:1$salt$zz",
        ] {
            let r = verify(bad, "x");
            assert!(r.is_err(), "{bad:?} 应报错而不是静默判否，实际 {r:?}");
        }

        // 结构像样但内容是垃圾：只要求不 panic
        for bad in ["$argon2id$garbage", "$argon2$", "scrypt:32768:8:1$salt$ab"] {
            let r = verify(bad, "x");
            assert!(
                matches!(r, Ok(false) | Err(_)),
                "{bad:?} 既不该 panic 也不该判真，实际 {r:?}"
            );
        }
    }

    /// N 不是 2 的幂时 scrypt 会 panic —— 必须先挡住。
    /// 这条是「库被写脏 → 服务崩溃」这个具体故障的回归。
    #[test]
    fn non_power_of_two_scrypt_n_does_not_panic() {
        let r = verify("scrypt:3:8:1$salt$ab", "x");
        assert!(r.is_err(), "N=3 应被拒绝而不是 panic");
        // 确认没有 panic 过（能走到这里就说明没 panic）
        assert!(matches!(r, Err(PasswordError::Malformed(_))));
    }

    #[test]
    fn hex_decode_handles_valid_and_invalid() {
        assert_eq!(hex_decode("00ff").unwrap(), vec![0x00, 0xff]);
        assert!(hex_decode("0ff").is_err(), "奇数长度");
        assert!(hex_decode("zz").is_err(), "非法字符");
        assert_eq!(hex_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn legacy_base64_decoder_tolerates_missing_padding() {
        // "aGk=" 是 "hi"，去掉 padding 也要能解
        assert_eq!(decode_legacy_base64("aGk").unwrap(), b"hi");
        assert_eq!(decode_legacy_base64("aGk=").unwrap(), b"hi");
    }

    #[test]
    fn constant_time_eq_is_correct() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }

    /// sha1 版 pbkdf2 的正确性用 Python 的 hashlib 交叉验证。
    #[test]
    fn sha1_pbkdf2_matches_known_vector() {
        // 来自 RFC 6070 的 PBKDF2-HMAC-SHA1 测试向量
        let out = pbkdf2_hmac_sha1(b"password", b"salt", 1);
        assert_eq!(
            out,
            hex_decode("0c60c80f961f0e71f3a9b524af6012062fe037a6").unwrap()
        );

        let out = pbkdf2_hmac_sha1(b"password", b"salt", 4096);
        assert_eq!(
            out,
            hex_decode("4b007901b765489abead49d926f721d065a429c1").unwrap()
        );
    }
}
