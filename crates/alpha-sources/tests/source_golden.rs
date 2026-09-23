//! 源层回归测试：把 Rust 适配器的输出与原版 Python 的输出逐字比对。
//!
//! 基准数据由 `tools/gen_source_golden.py` 从原版 Python 实现生成，
//! 存放在 `tests/fixtures/source_samples.json`。
//!
//! 这里覆盖三块容易悄悄偏移的逻辑：
//! 1. **性别解析** —— 各源的写法千奇百怪，`公1母7` 这类边界值最容易算错
//! 2. **地点译中** —— 249 条对照表 + `Route N` 规则 + 兜底
//! 3. **报点选优 / 归一化** —— 投票机制、英文优先解析、图鉴补特性

use std::path::{Path, PathBuf};

use alpha_core::config::Config;
use alpha_core::pokedex::get_pokedex;
use alpha_sources::base::{parse_male_ratio_str, DataSource};
use alpha_sources::lzpoke_reports::LzpokeReportsSource;
use alpha_sources::pokemmotools_landing::{hm_zh, region_zh, translate_location};
use chrono::Timelike;
use serde::Deserialize;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn load_fixture() -> serde_json::Value {
    let raw = std::fs::read_to_string(fixtures_dir().join("source_samples.json"))
        .expect("缺少 source_samples.json，请先运行 tools/gen_source_golden.py");
    serde_json::from_str(&raw).expect("source_samples.json 不是合法 JSON")
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// 用仓库内置的 `config/` 构造配置（测试不依赖用户本地环境变量）。
fn test_config() -> Config {
    std::env::set_var("ALPHA_ROOT", workspace_root());
    Config::load().expect("加载仓库内置配置失败")
}

// ------------------------------------------------------------ 性别解析

#[test]
fn male_ratio_matches_python() {
    let fx = load_fixture();
    let cases = fx["parse_male_ratio"]
        .as_object()
        .expect("parse_male_ratio 基准缺失");

    let mut checked = 0;
    let mut failures = Vec::new();

    for (input, expected) in cases {
        let expected = expected.as_f64();
        let actual = parse_male_ratio_str(input);
        checked += 1;
        let same = match (expected, actual) {
            (None, None) => true,
            (Some(a), Some(b)) => (a - b).abs() < 1e-9,
            _ => false,
        };
        if !same {
            failures.push(format!("  输入 {input:?}: 期望 {expected:?} / 实际 {actual:?}"));
        }
    }

    assert!(
        failures.is_empty(),
        "{}/{} 个性别解析不一致:\n{}",
        failures.len(),
        checked,
        failures.join("\n")
    );
    eprintln!("✓ {checked} 个性别解析与原版一致");
}

// ------------------------------------------------------------ 地点译中

#[test]
fn translate_location_matches_python() {
    let fx = load_fixture();
    let cases = fx["translate_location"]
        .as_object()
        .expect("translate_location 基准缺失");

    let mut failures = Vec::new();
    let mut checked = 0;

    for (key, expected) in cases {
        let (region, location) = key.split_once('|').expect("基准 key 应为 `region|location`");
        let region = if region == "None" { None } else { Some(region) };
        let location = if location == "None" { None } else { Some(location) };

        let actual = translate_location(region, location);
        let expected = expected.as_str().map(|s| s.to_string());
        checked += 1;
        if actual != expected {
            failures.push(format!(
                "  {key}: 期望 {expected:?} / 实际 {actual:?}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{}/{} 个地点翻译不一致:\n{}",
        failures.len(),
        checked,
        failures.join("\n")
    );
    eprintln!("✓ {checked} 个地点翻译与原版一致");
}

#[test]
fn region_and_hm_tables_match_python() {
    let fx = load_fixture();

    let regions = fx["region_zh"].as_object().unwrap();
    let mut failures = Vec::new();
    for (en, zh) in regions {
        let actual = region_zh(en);
        if actual != zh.as_str().unwrap() {
            failures.push(format!("  地区 {en}: 期望 {:?} / 实际 {actual:?}", zh));
        }
    }
    assert!(failures.is_empty(), "地区表不一致:\n{}", failures.join("\n"));

    let hms = fx["hm_zh"].as_object().unwrap();
    let mut failures = Vec::new();
    for (en, zh) in hms {
        let actual = hm_zh(en);
        if actual != *zh.as_str().unwrap() {
            failures.push(format!("  秘传技 {en}: 期望 {:?} / 实际 {actual:?}", zh));
        }
    }
    assert!(failures.is_empty(), "秘传技表不一致:\n{}", failures.join("\n"));

    eprintln!("✓ 地区表 {} 条 / 秘传技表 {} 条与原版一致", regions.len(), hms.len());
}

// ------------------------------------------------------ LZPoke 报点源

#[derive(Debug, Deserialize)]
struct GoldenBoss {
    name: String,
    ability: String,
    moves: Vec<String>,
    period: String,
    location: String,
    location_en: String,
    reporter: String,
    male: Option<f64>,
    egg_groups: Vec<String>,
    pokedex_id: Option<i64>,
    ability_id: Option<i64>,
    move_ids: Vec<Option<i64>>,
    source: String,
    extra_lines: Vec<GoldenLine>,
}

#[derive(Debug, Deserialize)]
struct GoldenLine {
    zh: String,
    en: String,
}

#[tokio::test]
async fn lzpoke_reports_adapter_matches_python() {
    let fx = load_fixture();
    let expected: GoldenBoss = serde_json::from_value(fx["lzpoke_reports"]["boss"].clone())
        .expect("lzpoke 基准 boss 反序列化失败");
    let expected_dedup = fx["lzpoke_reports"]["dedup_key"].as_str().unwrap().to_string();
    let expected_slot_en = fx["lzpoke_reports"]["slot_name_en"].as_str().unwrap().to_string();

    let cfg = test_config();
    let options: alpha_core::config::SourceOptions = serde_yaml::from_str(
        r#"
target: https://tool.lzpoke.com/api/reports?type=alpha
extra_lines:
  vote: true
"#,
    )
    .unwrap();

    let sample = serde_json::json!({
        "reports": [
            {
                "type": "alpha",
                "windowStart": "2026-09-11T00:00:00.000Z",
                "windowEnd": "2026-09-11T06:00:00.000Z",
                "monsterId": 342,
                "level": 100,
                "moves": ["蟹钳锤", "咬碎", "近身战", "龙之舞"],
                "nameEn": "Crawdaunt",
                "location": "丰缘 · 弃船",
                "locationEn": "Abandoned Ship",
                "regionEn": "Hoenn",
                "region": "丰缘",
                "id": "ce99402e-0000-0000-0000-000000000001",
                "createdAt": "2026-09-11T04:50:53.351Z",
                "reporterName": "lmxm",
                "voteWeight": 1,
                "burstKey": "alpha|2026-09-11|342|abandoned ship",
                "voteScore": 3,
                "voteCount": 3,
                "threshold": 5,
                "confirmed": false
            },
            {
                "type": "alpha",
                "windowStart": "2026-09-11T00:00:00.000Z",
                "monsterId": 100,
                "moves": ["十万伏特"],
                "nameEn": "Voltorb",
                "location": "关都 · 道路 10",
                "locationEn": "Kanto · Route 10",
                "id": "ce99402e-0000-0000-0000-000000000002",
                "createdAt": "2026-09-11T05:10:00.000Z",
                "reporterName": "someone",
                "voteScore": 7,
                "voteCount": 7,
                "threshold": 5,
                "confirmed": true
            }
        ]
    });

    let src = LzpokeReportsSource::new(options, &cfg).unwrap().with_sample(sample);
    let res = src.fetch().await;

    assert!(res.is_hit(), "应命中，实际: {:?} / {}", res.status, res.message);

    // 选优：已确认优先于票分高 —— 第二条 confirmed=true 应胜出
    assert_eq!(res.dedup_key, expected_dedup, "去重标识(取 burstKey/id)不一致");

    let boss = res.boss.as_ref().unwrap();
    let mut failures = Vec::new();
    let mut check = |field: &str, a: String, e: String| {
        if a != e {
            failures.push(format!("  {field}: 期望 {e:?} / 实际 {a:?}"));
        }
    };

    check("name", boss.name.clone(), expected.name.clone());
    check("ability", boss.ability.clone(), expected.ability.clone());
    check("location", boss.location.clone(), expected.location.clone());
    check("location_en", boss.location_en.clone(), expected.location_en.clone());
    // period 里含「约止于 HH:MM」，由 `now + 75min` 算出 —— 跨分钟跑就会差 1，
    // 所以只比对时段名部分，不断言分钟。
    check(
        "period_slot",
        boss.period.split('(').next().unwrap_or("").to_string(),
        expected.period.split('(').next().unwrap_or("").to_string(),
    );
    check(
        "reporter",
        boss.reporter.clone().unwrap_or_default(),
        expected.reporter.clone(),
    );
    check("source", boss.source.clone(), expected.source.clone());
    check("moves", format!("{:?}", boss.moves), format!("{:?}", expected.moves));
    check(
        "move_ids",
        format!("{:?}", boss.move_ids),
        format!("{:?}", expected.move_ids),
    );
    check(
        "egg_groups",
        format!("{:?}", boss.egg_groups),
        format!("{:?}", expected.egg_groups),
    );
    check("pokedex_id", format!("{:?}", boss.pokedex_id), format!("{:?}", expected.pokedex_id));
    check("ability_id", format!("{:?}", boss.ability_id), format!("{:?}", expected.ability_id));
    check(
        "extra_lines",
        format!(
            "{:?}",
            boss.extra_lines.iter().map(|l| (&l.zh, &l.en)).collect::<Vec<_>>()
        ),
        format!(
            "{:?}",
            expected.extra_lines.iter().map(|l| (&l.zh, &l.en)).collect::<Vec<_>>()
        ),
    );
    // 霹雳电球无性别：两侧都应为 None（图鉴 gender_rate 为 null）
    check(
        "male_percent",
        format!("{:?}", boss.gender.male_percent),
        format!("{:?}", expected.male),
    );

    assert!(
        failures.is_empty(),
        "LZPoke 适配器输出与原版不一致:\n{}",
        failures.join("\n")
    );

    assert_eq!(res.slot_name_en, expected_slot_en, "时段英文名不一致");

    // 本源不给「剩余消失时间」，时段必须按「命中时刻 + 75 分钟」推算。
    // 断言格式与大致范围，否则上面跳过分钟就等于没测这条规则。
    let expire = boss
        .period
        .rsplit_once("约止于")
        .and_then(|(_, rest)| rest.trim_end_matches(')').split_once(':'))
        .and_then(|(h, m)| Some((h.parse::<i64>().ok()?, m.parse::<i64>().ok()?)))
        .expect("period 应形如 `早头(约止于13:15)`");
    let now_bj = alpha_core::time::now_beijing();
    let expected_minutes = (now_bj.hour() as i64 * 60 + now_bj.minute() as i64 + 75) % (24 * 60);
    let actual_minutes = expire.0 * 60 + expire.1;
    assert!(
        (actual_minutes - expected_minutes).rem_euclid(24 * 60) <= 1,
        "时段失效时刻应为「现在 + 75 分钟」（允许 1 分钟误差）：期望约 {expected_minutes} 分钟，实际 {actual_minutes} 分钟"
    );

    eprintln!(
        "✓ LZPoke 适配器与原版一致：{} / {} / {:?}",
        boss.name, boss.location, boss.moves
    );
}

// ------------------------------------------------- 时段与地区边界补充

#[test]
fn slot_name_en_covers_all_four_slots() {
    use alpha_core::time::slot_name_en;
    assert_eq!(slot_name_en("凌晨头"), "Dawn Alpha");
    assert_eq!(slot_name_en("早头"), "Morning Alpha");
    assert_eq!(slot_name_en("午头"), "Noon Alpha");
    assert_eq!(slot_name_en("晚头"), "Night Alpha");
}

#[test]
fn known_adapters_are_all_registered() {
    let cfg = test_config();
    let pokedex = get_pokedex().unwrap();

    // 配置里启用的源，只要适配器名在已知列表里，就应该能构造出来
    for scfg in cfg.enabled_sources() {
        let known = alpha_sources::known_adapters().contains(&scfg.adapter.as_str());
        assert!(
            known,
            "配置里的适配器 `{}` 未在 KNOWN_ADAPTERS 中登记",
            scfg.adapter
        );
        let built = alpha_sources::create_source(&scfg.adapter, scfg.options.clone(), &cfg);
        assert!(built.is_ok(), "适配器 {} 构造失败: {:?}", scfg.adapter, built.err());
    }
    let _ = pokedex;
    eprintln!("✓ 所有已启用源的适配器均可构造");
}

#[test]
fn unknown_adapter_reports_helpful_error() {
    let cfg = test_config();
    let err = alpha_sources::create_source(
        "definitely_not_a_source",
        Default::default(),
        &cfg,
    )
    .err()
    .expect("未知适配器应当报错");
    let msg = err.to_string();
    assert!(msg.contains("未知的数据源适配器"), "{msg}");
    assert!(msg.contains("lzpoke_reports"), "报错里应列出已支持的适配器: {msg}");
}
