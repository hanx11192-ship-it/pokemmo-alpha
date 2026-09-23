// ═══════════════════════════════════════════════════════════════════════════
//  脚本队评估器 / Script-Team Evaluator
// ═══════════════════════════════════════════════════════════════════════════
//
// @name 脚本队评估器
// @description 评估脚本队（呆壳兽核心）能否无随机性推进：0-18 评分 + 评价词，-1 为小丑头
// @version 2.0
//
// ── 这是什么 ──────────────────────────────────────────────────────────────
//
// 对「脚本队」打法能否无随机性地顺利推进，给头目打一个难度评分（0 下限、
// 18 上限，另有 -1 的小丑头特判）。判据核心是：头目能不能打断我方节奏核心
// 「呆壳兽」。
//
//     呆壳兽是队伍的中转/睡眠发动机，它能否稳定出手决定整个脚本能否跑通：
//       - 密探斗篷：免疫睡眠 / 麻痹 / 冰冻 这类「追加异常」（不会因异常放不出招）
//       - 迟钝特性：免疫挑衅 / 着迷（不会被封变化技、不会被着迷锁招）
//       - 不会混乱（无直接/间接混乱来源），避免概率性放错招
//     所以「睡/麻/冰 + 挑衅 + 着迷 + 混乱」这些常规威胁对脚本队无效，不计分。
//
// ── 评分模型 ──────────────────────────────────────────────────────────────
// （先判性别，再判小丑头，再加基础项，再加梦话类，再加特判，最后 clamp）
//
//   ── 性别过滤（最高优先级，单性别/无性别且不在白名单 → 不评估）──
//     白名单：艾路雷朵（Gallade）
//
//   ── 小丑头（次优先，命中即固定 -1，直接返回）──
//     技能集合恰好 = {挥指功, 珍藏}        —— 任意精灵
//
//   ── 基础项（逐个累加 +1）──
//     · 翻车技：干扰/异常类，会让呆壳兽放不出招或放错招 → 每个 +1
//     · 离场技：飞翔/潜水/挖洞/弹跳/吼叫/吹飞/龙尾/巴投 拖回合 → 每个 +1
//     · 防睡技：守住/神秘守护/薄雾场地，头目提前防睡 → 每个 +1
//     · 先手技：音速拳/子弹拳/电光一闪/神速/佯攻/真空波/冰砾/水流喷射/突袭
//               抢节奏 → 每个 +1
//
//   ── 梦话类（同类只吃一次 +2）──
//     · 带梦话 + 有翻车技 → +2
//     · 带梦话 + 有离场技 → +2
//     · 梦话 + 先手技 → 额外 +1（先手技本身已 +1，此处叠加）
//     · 梦话 +（治愈铃声 或 芳香治疗）→ 独立 +2
//       （前两项梦话加成若已触发，这里只 +2，不叠加成 +4）
//
//   ── 特判（在基础分之上额外叠加）──
//     · 巨牙鲨 +（挑衅+近身战）或（挑衅+咬碎）                    → +2
//     · 斗笠菇 +（音速拳+种子炸弹 / 种子机关枪 / 打草结）          → +2
//
//   ── 分档 ──
//     -1           小丑头
//     0            白给头
//     (0, 3]       简单头
//     (3, 6]       看脸头
//     > 6          还有高手？！？
//
// ── 评语说明 ──────────────────────────────────────────────────────────────
//
// 评语与原版内置 `script_team.py` 有两处**有意的**差别（现网运行的是这个版本）：
//     无威胁   → 白给头
//     不做评价 → 还有高手？！？
// 评分逻辑完全一致，只有这两句文案不同，所以合并成一个插件、只改文案。
//
// ── 可插拔 ────────────────────────────────────────────────────────────────
//
// 匹配一律走技能 id（`resolve_move_id`），民间叫法靠别名表兜底，
// 因此上游给英文/机翻/别名都能正确命中同一条规则。
// ═══════════════════════════════════════════════════════════════════════════

// ── 常数表 ────────────────────────────────────────────────────────────────
//
// ⚠️ 这里全部写成 `private fn` 而**不是**顶层 `const` / `let`。
//
// 原因：Rhai 里顶层 `let` / `const` 只是「模块初始化时执行一次的赋值」，
// 它们**不在任何函数的可见作用域里** —— 函数体里引用 `CLOWN_SET` 会直接
// 报 `Variable not found: CLOWN_SET`。原版 Python 是模块级全局变量，
// 脚本函数随便读，所以这里是翻译时最容易踩的坑。
//
// 写成 `private fn` 的额外好处：每个调用点各拿一份新数组，
// 脚本就算 push 进去也不会污染下一次调用（模块状态在轮询场景下极危险）。

private fn score_min()     { -1 }
private fn score_max()     { 18 }

// 翻车技：会让呆壳兽放不出招 / 放错招 / 拖回合的干扰与异常类
private fn crash_moves() {
    [
        "电磁波", "哈欠", "恶魔之吻", "虚张声势", "催眠术", "催眠粉", "麻痹粉",
        "大蛇瞪眼", "掷泥", "魔法反射", "唱歌", "蘑菇孢子", "奇异之光", "超音波",
        "吹捧", "摇晃舞", "可爱之吻", "幻象光线", "爆裂拳", "迷昏拳",
        "再来一次", "吵闹",
    ]
}

// 离场 / 拖回合技：头目离场或强制换人，打乱脚本节奏
private fn pivot_away_moves() {
    ["飞翔", "潜水", "挖洞", "跳跃", "吼叫", "吹飞", "龙尾", "巴投"]
}

// 防睡技：头目提前预防被睡眠
private fn sleep_guard_moves() {
    ["守住", "神秘守护", "薄雾场地"]
}

// 先手技（先制技）：与 rules.yaml 的 priority 分组保持同一份名单。
// 头目带先手技即 +1/个（抢节奏）；若同时带梦话，再额外 +1（靠梦话随机抽先制）。
private fn priority_moves() {
    [
        "音速拳", "子弹拳", "电光一闪", "神速", "佯攻",
        "真空波", "冰砾", "水流喷射", "突袭",
    ]
}

private fn move_sleep_talk() { "梦话" }

private fn moves_self_heal_sleep() { ["治愈铃声", "芳香治疗"] }

// 小丑头组合：技能集合恰好等于这两招
private fn clown_set() { ["挥指功", "珍藏"] }

// 性别白名单：单性别/无性别的头目里**仍然要评**的精灵。
// 目前只有艾路雷朵（Gallade，只有公的，但脚本队有对应打法）。
private fn gender_whitelist() { ["艾路雷朵"] }

// 特判组合：每组内技能「全部携带」才算命中；组间「任一」命中即触发
private fn special_cases() {
    [
        #{ pokemon: "巨牙鲨", score: 2, combos: [["挑衅", "近身战"], ["挑衅", "咬碎"]] },
        #{ pokemon: "斗笠菇", score: 2, combos: [
            ["音速拳", "种子炸弹"], ["音速拳", "种子机关枪"], ["音速拳", "打草结"]
        ] },
    ]
}

// ── 匹配工具 ──────────────────────────────────────────────────────────────

// 把技能名列表解析成 id 集合 + 无法解析的残余名（原名兜底）。
// 返回 #{ ids: [...], leftovers: [...] }
fn resolve_names(pokedex, names) {
    let ids = [];
    let leftovers = [];
    for n in names {
        let mid = resolve_move_id(n);
        if mid == () {
            leftovers.push(n);
        } else {
            ids.push(mid);
        }
    }
    #{ ids: ids, leftovers: leftovers }
}

// ── 关于「因子明细里的顺序」的一处**有意差异** ────────────────────────────
//
// 原版 `_resolve()` 返回的是 `set`，而 Python 的整数 set 按**哈希桶顺序**
// 迭代（初始 8 桶、装载率 3/5 扩容、`hash(int) == int`）。这个顺序：
//
//   · 不等于名单书写顺序；
//   · 不等于 id 升序；
//   · 还会随「这一组实际解析出几个 id」而变（因为扩容时机不同）。
//
// 实测：带「唱歌+电磁波+催眠术」的头目，原版输出
//     `翻车技×3:唱歌/电磁波/催眠术`
// 而按本文件的名单顺序输出
//     `翻车技×3:电磁波/唱歌/催眠术`
//
// 这里**选择名单书写顺序**，即上面看到的这种差异是故意保留的：
//
//   1. 它只影响 `detail`（因子明细）这一个调试字段 ——
//      `score`、`label` 完全一致，**推送报文那一行不受任何影响**
//      （报文只用 `format_line` 拼的「评分N + 评语」）；
//   2. 名单顺序是「人写下来的顺序」，可解释、跨语言可复现；
//      Python 的桶顺序是 CPython 的实现细节，换个 Python 版本就可能变；
//   3. 为了复刻它要写一套 CPython set 的表布局模拟（hash/掩码/扰动/
//      扩容时机），约百行极难维护的代码，收益只是调试页字符串一样。
//
// 回归测试对 `detail` 做**规范化比较**（按 `/` 与 `；` 切开后排序再比），
// 其余字段仍是逐字严格比对。若要改成完全逐字一致，见
// `tests/plugin_golden.rs` 里 `detail_ordering_is_normalized` 的说明。

// 头目是否携带该组里任一技能（id 优先，字符串兜底）。
fn has_any(boss, ids, leftovers) {
    for mid in ids {
        if boss_move_has_id(boss, mid) {
            return true;
        }
    }
    for n in leftovers {
        if boss.moves.contains(n) {
            return true;
        }
    }
    false
}

// 头目是否「同时携带」names 里全部技能。
fn has_all(boss, names) {
    for n in names {
        let mid = resolve_move_id(n);
        if mid == () {
            if !boss.moves.contains(n) { return false; }
        } else {
            if !boss_move_has_id(boss, mid) { return false; }
        }
    }
    true
}

// 头目是否携带某个技能 id。
// move_ids 里可能有 ()（源没给 id），所以要逐个判类型。
fn boss_move_has_id(boss, mid) {
    for x in boss.move_ids {
        if x != () && x == mid {
            return true;
        }
    }
    false
}

// 头目技能集合是否「恰好等于」names（数量与内容都一致）。
fn move_set_equal(boss, names) {
    let target = [];
    for n in names {
        let mid = resolve_move_id(n);
        target.push(if mid == () { n } else { mid });
    }

    let cur = [];
    let n = boss.moves.len();
    // move_ids 与 moves 可能长度不一致（构造方违约/手改数据）——
    // 先判长度再索引，否则 move_ids[i] 直接越界炸掉整个评估。
    // 不等长时退化用中文名集合比较（target 也退回原名）。
    if boss.move_ids.len() != n {
        for nm in boss.moves {
            cur.push(nm);
        }
        target = names;
    } else {
        for i in 0..n {
            let item = boss.move_ids[i];
            cur.push(if item == () { boss.moves[i] } else { item });
        }
    }

    // 集合相等 = 长度相等 且 互相包含
    if cur.len() != target.len() { return false; }
    for x in cur {
        if !target.contains(x) { return false; }
    }
    true
}

// 按图鉴号（优先）或中文名判断头目是否为目标精灵。
fn pokemon_is(boss, name) {
    let pid = resolve_pokemon_id(name);
    if pid != () && boss.pokedex_id != () {
        return boss.pokedex_id == pid;
    }
    boss.name == name
}

// 分值 → 评价词。
fn label_of(score) {
    if score == -1 { return "小丑头"; }
    if score == 0  { return "白给头"; }
    if score <= 3  { return "简单头"; }
    if score <= 6  { return "看脸头"; }
    "还有高手？！？"
}

fn label_en_of(zh) {
    if zh == "小丑头"       { return "Clown Alpha"; }
    if zh == "白给头"       { return "No Threat"; }
    if zh == "简单头"       { return "Easy Alpha"; }
    if zh == "看脸头"       { return "RNG Alpha"; }
    if zh == "还有高手？！？" { return "Unrated"; }
    zh
}

// 逐个技能精确计数，返回 #[n, names]
// （同一只头目可能带多个翻车/离场/防睡/先手技）
fn count_each(boss, ids, leftovers) {
    let n = 0;
    let names = [];
    for mid in ids {
        if boss_move_has_id(boss, mid) {
            n += 1;
            names.push(move_name(mid, "zh"));
        }
    }
    for nm in leftovers {
        if boss.moves.contains(nm) {
            n += 1;
            names.push(nm);
        }
    }
    [n, names]
}

// ── 主评估 ────────────────────────────────────────────────────────────────

// 性别形态判断：单性别或无性别的头目脚本队打不了（异性配队机制无从谈起），
// 评了也是白评 —— 直接不评估（`return ()`，报文里不出评语行）。
//
// `boss.gender` 是 BossView 拼好的**文本**（不是结构化数值）：
//     双性别 "50.0%公" · 只公 "100.0%公" · 只母 "0.0%公" · 无性别 ""
// 解析不出（格式变了）就不拦，宁可误评也不错杀。
fn gender_blocks(boss) {
    // 白名单豁免在前 —— 艾路雷朵只有公的，但照常评估
    for nm in gender_whitelist() {
        if pokemon_is(boss, nm) {
            return false;
        }
    }
    let g = boss.gender;
    if g == "" {
        return true; // 无性别
    }
    let i = g.index_of("%");
    if i < 0 {
        return false; // 文本不认识，不拦
    }
    let p = 0.0;
    let parsed = true;
    try {
        p = g.sub_string(0, i).parse_float();
    } catch {
        parsed = false;
    }
    if !parsed {
        return false;
    }
    p <= 0.0 || p >= 100.0 // 只母 / 只公
}

fn evaluate(boss, ctx) {
    let lang = if ctx.lang == "en" { "en" } else { "zh" };

    // ===== 0) 性别过滤：单性别/无性别且不在白名单 → 不评估 =====
    if gender_blocks(boss) {
        return ();
    }

    // ===== 1) 小丑头：最高优先级 =====
    if move_set_equal(boss, clown_set()) {
        let out = #{
            score: -1,
            label: "小丑头",
            label_en: "Clown Alpha",
            detail: "恰好只带挥指功+珍藏，前 6 回合无法正常出手",
            factors: ["clown_set:挥指功+珍藏"],
        };
        return out;
    }

    let score = 0;
    let factors = [];

    // ===== 2) 基础项：逐个累加 =====
    let crash = resolve_names(0, crash_moves());
    let away  = resolve_names(0, pivot_away_moves());
    let guard = resolve_names(0, sleep_guard_moves());
    let pri   = resolve_names(0, priority_moves());

    // 「是否命中该分类中任意一个」—— 梦话类判据要用
    let crash_hit = has_any(boss, crash.ids, crash.leftovers);
    let away_hit  = has_any(boss, away.ids, away.leftovers);
    let pri_hit   = has_any(boss, pri.ids, pri.leftovers);

    let cr = count_each(boss, crash.ids, crash.leftovers);
    let aw = count_each(boss, away.ids, away.leftovers);
    let gd = count_each(boss, guard.ids, guard.leftovers);
    let pr = count_each(boss, pri.ids, pri.leftovers);

    if cr[0] > 0 {
        score += cr[0];
        factors.push(`翻车技×${cr[0]}:${cr[1].join("/")}`);
    }
    if aw[0] > 0 {
        score += aw[0];
        factors.push(`离场技×${aw[0]}:${aw[1].join("/")}`);
    }
    if gd[0] > 0 {
        score += gd[0];
        factors.push(`防睡技×${gd[0]}:${gd[1].join("/")}`);
    }
    if pr[0] > 0 {
        score += pr[0];
        factors.push(`先手技×${pr[0]}:${pr[1].join("/")}`);
    }

    // ===== 3) 梦话类（同类只吃一次 +2）=====
    if has_all(boss, [move_sleep_talk()]) {
        if crash_hit {
            score += 2;
            factors.push("梦话+翻车技:+2");
        }
        if away_hit {
            score += 2;
            factors.push("梦话+离场技:+2");
        }
        // 梦话 + 先手技 → 额外 +1（靠梦话随机抽出先制技抢节奏）
        // 注意：先手技本身已在基础项 +1/个，这里是在其之上再叠加。
        if pri_hit {
            score += 1;
            factors.push("梦话+先手技:+1");
        }
        // 梦话 + 自愈睡眠（治愈铃声/芳香治疗）→ 独立 +2，不与上面叠加成 +4
        if has_all(boss, [moves_self_heal_sleep()[0]]) ||
           has_all(boss, [moves_self_heal_sleep()[1]]) {
            score += 2;
            factors.push("梦话+自愈睡眠:+2");
        }
    }

    // ===== 4) 特判：基础分之上额外叠加 =====
    for sp in special_cases() {
        if !pokemon_is(boss, sp.pokemon) {
            continue;
        }
        for combo in sp.combos {
            if has_all(boss, combo) {
                score += sp.score;
                factors.push(`${sp.pokemon}+${combo.join("+")}:+${sp.score}`);
                break;   // 每只精灵只算一次特判
            }
        }
    }

    // ===== 5) clamp =====
    if score < score_min() { score = score_min(); }
    if score > score_max() { score = score_max(); }
    let label = label_of(score);

    let detail = if factors.len() > 0 {
        factors.join("；")
    } else {
        "无威胁因子，脚本可稳定推进"
    };

    let out = #{
        score: score,
        label: label,
        detail: detail,
        factors: factors,
    };
    if lang == "en" {
        out.label_en = label_en_of(label);
    }
    out
}
