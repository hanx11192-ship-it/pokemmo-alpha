//! 全局去重（跨源，按「头目指纹 + 时段」）。
//!
//! 游戏机制（用户给定）：
//! 一天分 4 个时段，每时段必定刷一只固定存在 75 分钟的头目
//! （早头 08:00–14:00 / 午头 14:00–20:00 / 晚头 20:00–次日 02:00 / 凌晨头 02:00–08:00）。
//!
//! 去重策略：
//! 去重键 = 头目内容指纹（图鉴号）+ 时段标识（北京时间 6h 边界切成 4 段）。
//! - 同一时段、同一只头目（同指纹）→ 键相同 → 只推一次
//! - 跨时段 → 键不同 → 必重推（契合「每时段必报一只」）
//! - 跨源一致：不同源报同一只头目，指纹相同、时段相同 → 键相同 → 只推一次
//!
//! **指纹算法必须与原版逐字一致**（`md5(str(pokedex_id))[:16]`），
//! 否则 `data/state.json` 无法平滑迁移，升级后会重复推送一次。
//!
//! 对应原版 `src/core/dedup.py`。

use std::collections::HashSet;
use std::path::Path;

use md5::{Digest, Md5};

use crate::error::Result;
use crate::models::BossData;
use crate::time::{now_beijing, parse_iso, slot_key_of, to_beijing};

/// 默认保留条数（settings.dedup.keep 缺省）。
pub const DEFAULT_KEEP: usize = 500;

/// 头目内容指纹：跨源一致。图鉴 id 优先于名称（更稳）。
///
/// 仅以「图鉴号（或名称）」为指纹主体；时段由 `slot_key_of` 叠加。
/// 游戏机制保证每时段只有一只头目、固定存活 75 分钟且不跨时段，
/// 因此「图鉴号 + 时段」即可唯一锁定一只头目，无需纳入 特性/地点/技能
/// （这些字段跨源格式不一，纳入会导致同一只头目被多源当成不同指纹而重复推送）。
pub fn boss_fingerprint(boss: Option<&BossData>) -> String {
    let Some(b) = boss else {
        return String::new();
    };
    let key = match b.pokedex_id {
        Some(id) => id.to_string(),
        None => {
            if b.name.is_empty() {
                return String::new();
            }
            b.name.clone()
        }
    };
    if key.is_empty() {
        return String::new();
    }
    let digest = Md5::digest(key.as_bytes());
    format!("fp:{}", &hex_lower(&digest)[..16])
}

/// 头目所属时段标识（北京时间 6h 边界）。
///
/// 源没给报点时间时，用轮询当下时刻兜底（与原版一致）。
pub fn slot_of(boss: Option<&BossData>) -> String {
    let dt = boss
        .and_then(|b| {
            if b.reported_at.is_empty() {
                None
            } else {
                parse_iso(&b.reported_at)
            }
        })
        .map(to_beijing)
        .unwrap_or_else(now_beijing);
    slot_key_of(dt)
}

/// 跨源去重键：头目指纹 + 时段标识（不含更细的时间粒度，避免裂桶）。
pub fn global_dedup_key(boss: Option<&BossData>) -> String {
    let fp = boss_fingerprint(boss);
    if fp.is_empty() {
        return String::new();
    }
    format!("{}@{}", fp, slot_of(boss))
}

/// 用头目内容算指纹（适配器兜底用，与原版 `BaseSource._content_fingerprint` 一致）。
pub fn content_fingerprint(boss: Option<&BossData>) -> String {
    let Some(b) = boss else {
        return String::new();
    };
    let raw = format!(
        "{}|{}|{}|{}|{}",
        b.name,
        b.ability,
        b.moves.join(","),
        b.location,
        b.period
    );
    if raw.trim_matches('|').is_empty() {
        return String::new();
    }
    let digest = Md5::digest(raw.as_bytes());
    format!("fp:{}", &hex_lower(&digest)[..16])
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// 去重状态文件（扁平的已处理键列表）。
///
/// 兼容旧版 `{source: [keys]}` 或 `list[dict]` 格式：直接丢弃，从头开始。
#[derive(Debug, Clone)]
pub struct DedupStore {
    path: std::path::PathBuf,
    keep: usize,
}

impl DedupStore {
    pub fn new(path: impl Into<std::path::PathBuf>, keep: usize) -> Self {
        Self {
            path: path.into(),
            keep: if keep == 0 { DEFAULT_KEEP } else { keep },
        }
    }

    /// 从全局配置构造。
    pub fn from_config() -> Result<Self> {
        let cfg = crate::config::get_config()?;
        Ok(Self::new(cfg.state_path(), cfg.settings.dedup.keep))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 读取已处理键列表。文件不存在 / 损坏 / 旧格式 → 返回空列表。
    pub fn load(&self) -> Vec<String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(_) => return Vec::new(),
        };
        let v: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };
        // 兼容旧版 {source: [keys]} 格式：直接丢弃
        let arr = match v.as_array() {
            Some(a) => a,
            None => return Vec::new(),
        };
        arr.iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect()
    }

    /// 原子保存（先写 .tmp 再 rename，避免写一半崩掉留下坏文件）。
    pub fn save(&self, data: &[String]) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(data)?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// 该键是否已处理。
    pub fn is_processed(&self, key: &str) -> bool {
        if key.is_empty() {
            return false;
        }
        let set: HashSet<String> = self.load().into_iter().collect();
        set.contains(key)
    }

    /// 标记已处理。**只应在推送成功之后调用**。
    pub fn mark(&self, key: &str) -> Result<()> {
        if key.is_empty() {
            return Ok(());
        }
        let mut state = self.load();
        if !state.iter().any(|k| k == key) {
            state.push(key.to_string());
        }
        if state.len() > self.keep {
            let drop_n = state.len() - self.keep;
            state.drain(0..drop_n);
        }
        self.save(&state)
    }

    /// 清空去重记录（调试用）。
    pub fn reset(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::BossData;

    fn boss(pid: Option<i64>, name: &str, reported_at: &str) -> BossData {
        BossData {
            name: name.to_string(),
            ability: "恶作剧之心".to_string(),
            moves: vec!["冲浪".to_string()],
            pokedex_id: pid,
            reported_at: reported_at.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn fingerprint_uses_pokedex_id() {
        // md5("462") = ?
        let b = boss(Some(462), "自爆磁怪", "");
        let fp = boss_fingerprint(Some(&b));
        assert!(fp.starts_with("fp:"));
        assert_eq!(fp.len(), 3 + 16);
        // 与 Python 的 hashlib.md5("462".encode()).hexdigest()[:16] 一致
        let expect = format!("fp:{}", &hex_lower(&Md5::digest(b"462"))[..16]);
        assert_eq!(fp, expect);
    }

    #[test]
    fn fingerprint_falls_back_to_name() {
        let b = boss(None, "自爆磁怪", "");
        let fp = boss_fingerprint(Some(&b));
        let expect = format!("fp:{}", &hex_lower(&Md5::digest("自爆磁怪".as_bytes()))[..16]);
        assert_eq!(fp, expect);
    }

    #[test]
    fn empty_fingerprint_for_empty_boss() {
        assert_eq!(boss_fingerprint(None), "");
        let b = BossData::default();
        assert_eq!(boss_fingerprint(Some(&b)), "");
    }

    #[test]
    fn dedup_key_shape() {
        // 2026-09-11T04:50:53Z = 北京 12:50 → 早头 → 20260911-08
        let b = boss(Some(342), "铁螯龙虾", "2026-09-11T04:50:53.351Z");
        let k = global_dedup_key(Some(&b));
        assert!(k.ends_with("@20260911-08"), "got {k}");
        // 同图鉴号不同报点时间但同一时段 → 同一个键
        let b2 = boss(Some(342), "铁螯龙虾", "2026-09-11T05:30:00Z");
        assert_eq!(k, global_dedup_key(Some(&b2)));
        // 跨时段 → 不同键
        let b3 = boss(Some(342), "铁螯龙虾", "2026-09-11T07:00:00Z"); // 北京 15:00 午头
        assert_ne!(k, global_dedup_key(Some(&b3)));
    }

    #[test]
    fn content_fingerprint_deterministic() {
        let b = boss(Some(1), "A", "");
        let a = content_fingerprint(Some(&b));
        let c = content_fingerprint(Some(&b));
        assert_eq!(a, c);
        assert!(a.starts_with("fp:"));
    }

    #[test]
    fn store_roundtrip() {
        let dir = std::env::temp_dir().join(format!("alpha-dedup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("state.json");
        let store = DedupStore::new(&path, 5);

        assert!(!store.is_processed("k1"));
        store.mark("k1").unwrap();
        assert!(store.is_processed("k1"));
        assert_eq!(store.load(), vec!["k1".to_string()]);

        // 幂等
        store.mark("k1").unwrap();
        assert_eq!(store.load().len(), 1);

        // 超出 keep 时丢弃最旧的
        for i in 0..6 {
            store.mark(&format!("k{i}")).unwrap();
        }
        assert_eq!(store.load().len(), 5);
        assert!(!store.is_processed("k1"), "最旧的应被丢弃");

        // 旧格式（dict）应被丢弃
        std::fs::write(&path, r#"{"src":["a","b"]}"#).unwrap();
        assert_eq!(store.load().len(), 0);

        // 损坏文件也应容错
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(store.load().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
