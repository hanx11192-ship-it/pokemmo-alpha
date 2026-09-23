//! 地图点位中英对照表（种子表）。
//!
//! 结构：地区(英文名小写) -> { 该地区的具名点位(小写英文): 中文地图名 }
//!
//! 说明：
//! - 这些是中英对照「种子」，覆盖五个地区的常见具名点；完整官方点位请以
//!   PokeMMO 官方图鉴为准，按相同格式在此追加即可。
//! - 通用规则（见 [`crate::pokemmotools_landing::translate_location`]）会先把
//!   `Route N` 转成 `道路 N`，因此绝大多数刷新点无需在此列出。
//! - 未命中且非 Route 的地点，保留英文原样（不丢信息）。
//!
//! 本表由 `tools/gen_location_map.py` 从原版 Python 实现导出，请勿手改。

use once_cell::sync::Lazy;
use std::collections::HashMap;

/// 地区(英文小写) -> (点位小写英文 -> 中文名)
pub type LocationMap = HashMap<&'static str, HashMap<&'static str, &'static str>>;

/// 地区 -> (点位英文小写 -> 中文名)
pub static LOCATION_MAP: Lazy<LocationMap> = Lazy::new(|| {
    let mut m: LocationMap = HashMap::new();
    {
        let mut z = HashMap::new();
        z.insert("pallet town", "真新镇");
        z.insert("viridian city", "常磐市");
        z.insert("pewter city", "深灰市");
        z.insert("cerulean city", "华蓝市");
        z.insert("vermilion city", "枯叶市");
        z.insert("lavender town", "紫苑镇");
        z.insert("celadon city", "玉虹市");
        z.insert("fuchsia city", "浅红市");
        z.insert("cinnabar island", "红莲岛");
        z.insert("saffron city", "金黄市");
        z.insert("indigo plateau", "石英高原");
        z.insert("mt. moon", "月见山");
        z.insert("rock tunnel", "岩石隧道");
        z.insert("victory road", "冠军之路");
        z.insert("pokemon tower", "紫苑鬼塔");
        z.insert("safari zone", "浅红狩猎区");
        z.insert("seafoam islands", "双子岛");
        z.insert("power plant", "无人发电厂");
        z.insert("diglett's cave", "地鼠洞");
        z.insert("rocket hideout", "火箭队基地");
        z.insert("silph co.", "西尔佛公司");
        z.insert("pokemon mansion", "红莲宝可梦屋");
        z.insert("cerulean cave", "华蓝洞窟");
        z.insert("viridian forest", "常磐森林");
        z.insert("pokemon league", "宝可梦联盟");
        z.insert("one island", "1岛");
        z.insert("two island", "2岛");
        z.insert("three island", "3岛");
        z.insert("four island", "4岛");
        z.insert("five island", "5岛");
        z.insert("six island", "6岛");
        z.insert("seven island", "7岛");
        z.insert("kindle road", "火照山道");
        z.insert("mt. ember", "灯火山");
        z.insert("treasure beach", "宝物海滩");
        z.insert("cape brink", "岐波湾");
        z.insert("lost cave", "不归穴");
        z.insert("bond bridge", "索桥");
        z.insert("berry forest", "果实森林");
        z.insert("trainer tower", "训练家之塔");
        z.insert("icefall cave", "冰瀑洞窟");
        z.insert("five isle meadow", "5岛空地");
        z.insert("five island meadow", "5岛草原");
        z.insert("memorial pillar", "回忆之塔");
        z.insert("water path", "水道");
        z.insert("green path", "绿道");
        z.insert("ruin valley", "遗迹山谷");
        z.insert("outcast island", "外岛");
        z.insert("water labyrinth", "水之迷宫");
        z.insert("canyon entrance", "溪谷入口");
        z.insert("sevault canyon", "七宝溪谷");
        z.insert("tanoby ruins", "阿斯卡纳遗迹");
        z.insert("birth island", "诞生岛");
        z.insert("navel rock", "肚脐岩");
        z.insert("altering cave", "变化洞窟");
        z.insert("kanto altering cave", "变化洞窟");
        z.insert("resort gorgeous", "高级度假区");
        z.insert("pokémon tower", "紫苑鬼塔");
        z.insert("pokémon mansion", "红莲宝可梦屋");
        z.insert("mt  moon", "月见山");
        z.insert("mt  ember", "灯火山");
        z.insert("three isle port", "三岛码头");
        z.insert("tanoby chambers", "阿斯卡纳石室");
        z.insert("pattern bush", "记号森林");
        z.insert("botted hole", "点穴");
        m.insert("kanto", z);
    }
    {
        let mut z = HashMap::new();
        z.insert("new bark town", "若叶镇");
        z.insert("cherrygrove city", "吉花市");
        z.insert("violet city", "桔梗市");
        z.insert("azalea town", "桧皮镇");
        z.insert("cianwood city", "湛蓝市");
        z.insert("olivine city", "浅葱市");
        z.insert("goldenrod city", "满金市");
        z.insert("ecruteak city", "缘朱市");
        z.insert("mahogany town", "桃源市");
        z.insert("blackthorn city", "烟墨市");
        z.insert("lake of rage", "愤怒湖");
        z.insert("mt. silver", "白银市");
        z.insert("whirl islands", "漩涡列岛");
        z.insert("tin tower", "铃铛塔");
        z.insert("burned tower", "烧焦塔");
        z.insert("ice path", "冰雪小茎");
        z.insert("dragon's den", "龙穴");
        z.insert("slowpoke well", "呆呆兽之井");
        z.insert("ilex forest", "挖洞森林");
        z.insert("mt. mortar", "卡吉东洞");
        z.insert("ruins of alph", "阿露福遗迹");
        z.insert("union cave", "互连洞");
        z.insert("dark cave", "黑暗穴");
        z.insert("cliff cave", "断崖洞窟");
        z.insert("sinjoh ruins", "连入遗迹");
        z.insert("pokemon league", "宝可梦联盟");
        z.insert("mt  silver", "白银市");
        z.insert("tohjo falls", "成都瀑布");
        z.insert("dark.cave", "黑暗穴");
        z.insert("mt  mortar", "卡吉东洞");
        z.insert("bell tower", "铃铛塔");
        z.insert("battle frontier", "对战开拓区");
        z.insert("safari zone gate", "狩猎区入口");
        z.insert("sprout tower", "喇叭芽塔");
        m.insert("johto", z);
    }
    {
        let mut z = HashMap::new();
        z.insert("littleroot town", "未白镇");
        z.insert("oldale town", "古辰镇");
        z.insert("petalburg city", "橙华市");
        z.insert("rustboro city", "卡那兹市");
        z.insert("dewford town", "武斗镇");
        z.insert("slateport city", "凯那市");
        z.insert("mauville city", "紫堇市");
        z.insert("verdanturf town", "绿荫镇");
        z.insert("lavaridge town", "釜炎镇");
        z.insert("fallarbor town", "秋叶镇");
        z.insert("fortree city", "茵郁市");
        z.insert("lilycove city", "琉璃市");
        z.insert("mossdeep city", "暮水镇");
        z.insert("sootopolis city", "彩幽市");
        z.insert("pacifidlog town", "海运市");
        z.insert("ever grande city", "彩翠镇");
        z.insert("meteor falls", "陨石瀑布");
        z.insert("mt. chimney", "烟囱山");
        z.insert("cave of origin", "觉醒神社");
        z.insert("sky pillar", "天空之柱");
        z.insert("seafloor cavern", "海底洞窟");
        z.insert("petalburg woods", "橙华森林");
        z.insert("jagged pass", "险路");
        z.insert("fiery path", "焦灼小径");
        z.insert("desert ruins", "沙漠遗迹");
        z.insert("island cave", "孤岛");
        z.insert("ancient tomb", "远古墓室");
        z.insert("scorched slab", "焦岩");
        z.insert("abandoned ship", "弃船");
        z.insert("weather institute", "气象研究所");
        z.insert("safari zone", "狩猎地带");
        z.insert("battle tower", "对战塔");
        z.insert("artisan cave", "工匠之穴");
        z.insert("marine cave", "海洋洞窟");
        z.insert("terra cave", "陆地洞窟");
        z.insert("nameless cavern", "无名洞穴");
        z.insert("shoal cave", "浅滩洞穴");
        z.insert("new mauville", "新紫堇");
        z.insert("magma hideout", "熔岩队基地");
        z.insert("aqua hideout", "海洋队基地");
        z.insert("mirage island", "幻之岛");
        z.insert("granite cave", "武斗洞窟");
        z.insert("rusturf tunnel", "天旱隧道");
        z.insert("victory road", "冠军之路");
        z.insert("battle resort", "对战度假村");
        z.insert("southern island", "南方孤岛");
        z.insert("birth island", "诞生岛");
        z.insert("faraway island", "遥远小岛");
        m.insert("hoenn", z);
    }
    {
        let mut z = HashMap::new();
        z.insert("twinleaf town", "双叶镇");
        z.insert("sandgem town", "真砂镇");
        z.insert("jubilife city", "祝庆市");
        z.insert("oreburgh city", "黑金市");
        z.insert("floaroma town", "花蕊镇");
        z.insert("eterna city", "苑之镇");
        z.insert("hearthome city", "缘之市");
        z.insert("solaceon town", "随意镇");
        z.insert("veilstone city", "帷幕市");
        z.insert("pastoria city", "湿原市");
        z.insert("celestic town", "苑之花市");
        z.insert("canalave city", "钢铁市");
        z.insert("snowpoint city", "雪峰市");
        z.insert("sunyshore city", "滨海市");
        z.insert("fight area", "战斗区");
        z.insert("survival area", "生存区");
        z.insert("resort area", "度假区");
        z.insert("lake verity", "心齐湖");
        z.insert("lake valor", "英湖");
        z.insert("lake acuity", "睿智湖");
        z.insert("mt. coronet", "天冠山");
        z.insert("spear pillar", "枪之柱");
        z.insert("victory road", "冠军之路");
        z.insert("oreburgh mine", "黑金炭坑");
        z.insert("wayward cave", "迷失洞穴");
        z.insert("iron island", "铁岛");
        z.insert("snowpoint temple", "雪峰神殿");
        z.insert("stark mountain", "烈焰山");
        z.insert("turnback cave", "回转洞窟");
        z.insert("sendoff spring", "送泉");
        z.insert("ravaged path", "荒芜小径");
        z.insert("eterna forest", "苑之森林");
        z.insert("old chateau", "森之洋馆");
        z.insert("valley windworks", "风力发电厂");
        z.insert("fuego ironworks", "火焰炼铁厂");
        z.insert("lost tower", "迷失塔");
        z.insert("solaceon ruins", "随意遗迹");
        z.insert("spring path", "春之路");
        z.insert("pal park", "伙伴公园");
        z.insert("fullmoon island", "满月岛");
        z.insert("newmoon island", "新月岛");
        z.insert("flower paradise", "花之乐园");
        z.insert("distortion world", "反转世界");
        z.insert("battle zone", "战斗区域");
        m.insert("sinnoh", z);
    }
    {
        let mut z = HashMap::new();
        z.insert("nuvema town", "鹿子镇");
        z.insert("accumula town", "唐草镇");
        z.insert("striaton city", "三曜市");
        z.insert("nacrene city", "七宝市");
        z.insert("castelia city", "飞云市");
        z.insert("nimbasa city", "雷文市");
        z.insert("driftveil city", "帆巴市");
        z.insert("mistralton city", "吹寄市");
        z.insert("icirrus city", "雪花市");
        z.insert("opelucid city", "双龙市");
        z.insert("lacunosa town", "涟漪镇");
        z.insert("undella town", "小波镇");
        z.insert("black city", "黑色市");
        z.insert("white forest", "白色森林");
        z.insert("village bridge", "村庄桥");
        z.insert("pinwheel forest", "矢车森林");
        z.insert("desert resort", "荒野名胜区");
        z.insert("relic castle", "城堡遗迹");
        z.insert("dragonspiral tower", "龙旋之塔");
        z.insert("victory road", "冠军之路");
        z.insert("twist mountain", "罗斯山");
        z.insert("chargestone cave", "充电石洞穴");
        z.insert("mistralton cave", "牙牙洞");
        z.insert("reversal mountain", "反转山");
        z.insert("giant chasm", "巨人洞窟");
        z.insert("abundant shrine", "丰饶之社");
        z.insert("marvelous bridge", "迷幻桥");
        z.insert("strange house", "奇异小屋");
        z.insert("pokemon league", "宝可梦联盟");
        z.insert("liberty garden", "自由庭园岛");
        z.insert("royal unova", "皇家合众号");
        z.insert("tubeline bridge", "双龙左桥");
        z.insert("dreamyard", "梦境遗址");
        z.insert("celestial tower", "天堂之塔");
        z.insert("cold storage", "冰仓");
        z.insert("anville town", "金轮镇");
        z.insert("floccesy ranch", "立涌牧场");
        z.insert("virbank city", "立涌市");
        z.insert("humilau city", "青海波市");
        z.insert("join avenue", "连接大道");
        z.insert("moor of icirrus", "雪花湿地");
        z.insert("challenger's cave", "修行岩屋");
        z.insert("marine tube", "海洋隧道");
        z.insert("nature preserve", "自然保护区");
        z.insert("plasma frigate", "电浆团飞船");
        z.insert("white treehollow", "白色树洞");
        z.insert("black tower", "黑色摩天楼");
        z.insert("pwt", "宝可梦世界锦标赛");
        z.insert("cave of being", "生命之宙洞穴");
        z.insert("n's castle", "N的城堡");
        z.insert("unity tower", "联合塔");
        z.insert("p2 laboratory", "p2实验室");
        z.insert("wellspring cave", "地下水脉之穴");
        z.insert("chargetone cave", "电石洞穴");
        z.insert("driftveil drawbridge", "帆巴桥");
        z.insert("skyarrow bridge", "天之箭桥");
        z.insert("undella bay", "小波湾");
        z.insert("lostlorn forest", "迷失森林");
        m.insert("unova", z);
    }
    m
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_regions() {
        assert_eq!(LOCATION_MAP.len(), 5);
        for r in ["kanto", "johto", "hoenn", "sinnoh", "unova"] {
            assert!(LOCATION_MAP.contains_key(r), "缺少地区 {r}");
        }
    }

    #[test]
    fn spot_checks() {
        assert_eq!(LOCATION_MAP["kanto"]["mt. moon"], "月见山");
        assert_eq!(LOCATION_MAP["sinnoh"]["lake verity"], "心齐湖");
        assert_eq!(LOCATION_MAP["hoenn"]["abandoned ship"], "弃船");
        assert_eq!(LOCATION_MAP["unova"]["village bridge"], "村庄桥");
    }
}
