//! 读取「阅读地图」库：每个 md = 一本读过的书。
//!
//! 和单词/笔记库不同，这个库不画平面网络，而是喂给三维地球。
//! 每本书挂在一个作家名下，作家挂在一个国家/地区上，国家对应地球上的
//! 一个经纬度锚点。渲染时作家和作品是漂浮在地球外的光点，尾巴连回锚点。
//!
//! 每个 md 形如：
//! ```text
//! ---
//! author: Albert Camus
//! country: France
//! region: Algiers          # 可选，仅作标签
//! rating: 5                # 可选 1-5
//! lat: 36.75               # 可选，覆盖国家默认坐标
//! lon: 3.06
//! date: 2026-W20           # 复用周标签做时间筛选
//! ---
//! 随便写点读后感……
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::vocab::{parse_weeks, split_frontmatter, Week};

/// 地球上的一个国家/地区锚点。
pub struct Country {
    pub name: String,
    /// 纬度（北正），角度制。
    pub lat: f32,
    /// 经度（东正），角度制。
    pub lon: f32,
    /// 属于这个锚点的作家（`ReadingMap::authors` 的下标）。
    pub authors: Vec<u32>,
}

/// 一个作家，隶属一个国家锚点，名下若干作品。
pub struct Author {
    pub name: String,
    pub country: u32,
    pub books: Vec<u32>,
}

/// 一本读过的书。
pub struct Book {
    pub title: String,
    pub author: u32,
    pub path: PathBuf,
    /// 评分 1-5，未填为 None。
    pub rating: Option<u8>,
    /// 可选的地区标签（城市/省），只用于展示。
    pub region: Option<String>,
    /// 出现过的周（`ReadingMap::weeks` 的下标），用来做时间筛选与新旧渐变。
    pub weeks: Vec<u16>,
}

impl Book {
    pub fn last_week(&self) -> Option<u16> {
        self.weeks.last().copied()
    }
}

#[derive(Default)]
pub struct ReadingMap {
    pub countries: Vec<Country>,
    pub authors: Vec<Author>,
    pub books: Vec<Book>,
    /// 全库出现过的周，升序。
    pub weeks: Vec<Week>,
    /// 有 country 但查不到坐标、也没写 lat/lon 的书数量，用来提示拼写问题。
    pub unplaced: usize,
}

struct RawBook {
    title: String,
    path: PathBuf,
    author: String,
    country: String,
    region: Option<String>,
    rating: Option<u8>,
    coord: Option<(f32, f32)>,
    weeks: Vec<Week>,
}

impl ReadingMap {
    pub fn load(root: &Path) -> Result<ReadingMap, String> {
        if !root.is_dir() {
            return Err(format!("目录不存在: {}", root.display()));
        }

        let files: Vec<PathBuf> = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("md"))
            })
            .collect();

        if files.is_empty() {
            return Err(format!("{} 里没有找到任何 .md 文件", root.display()));
        }

        let raws: Vec<RawBook> = files
            .par_iter()
            .filter_map(|path| {
                let text = std::fs::read_to_string(path).ok()?;
                let title = path.file_stem()?.to_string_lossy().to_string();
                Some(parse_book(title, path.clone(), &text))
            })
            // 没写作者或国家的先跳过 —— 挂不到地球上
            .filter(|r| !r.author.is_empty() && !r.country.is_empty())
            .collect();

        Ok(build(raws))
    }
}

fn build(raws: Vec<RawBook>) -> ReadingMap {
    // 1. 全局周表
    let mut all_weeks: Vec<Week> = raws.iter().flat_map(|r| r.weeks.iter().copied()).collect();
    all_weeks.sort_unstable();
    all_weeks.dedup();
    let week_idx: HashMap<Week, u16> = all_weeks
        .iter()
        .enumerate()
        .map(|(i, w)| (*w, i as u16))
        .collect();

    let mut map = ReadingMap {
        weeks: all_weeks,
        ..ReadingMap::default()
    };

    // 2. 国家锚点：按解析出的坐标聚合。key 用规整过的国家名，
    //    这样「France」和「france」是同一个锚点。
    let mut country_of: HashMap<String, u32> = HashMap::new();
    let mut author_of: HashMap<String, u32> = HashMap::new();

    for raw in &raws {
        let key = normalize(&raw.country);
        let coord = raw.coord.or_else(|| country_coord(&raw.country));
        let Some((lat, lon)) = coord else {
            map.unplaced += 1;
            continue;
        };

        let ci = *country_of.entry(key).or_insert_with(|| {
            map.countries.push(Country {
                name: raw.country.clone(),
                lat,
                lon,
                authors: Vec::new(),
            });
            (map.countries.len() - 1) as u32
        });

        // 作家在「国家 + 名字」下唯一
        let akey = format!("{}::{}", ci, normalize(&raw.author));
        let ai = *author_of.entry(akey).or_insert_with(|| {
            map.authors.push(Author {
                name: raw.author.clone(),
                country: ci,
                books: Vec::new(),
            });
            let ai = (map.authors.len() - 1) as u32;
            map.countries[ci as usize].authors.push(ai);
            ai
        });

        let mut weeks: Vec<u16> = raw
            .weeks
            .iter()
            .filter_map(|w| week_idx.get(w).copied())
            .collect();
        weeks.sort_unstable();
        weeks.dedup();

        map.books.push(Book {
            title: raw.title.clone(),
            author: ai,
            path: raw.path.clone(),
            rating: raw.rating,
            region: raw.region.clone(),
            weeks,
        });
        let bi = (map.books.len() - 1) as u32;
        map.authors[ai as usize].books.push(bi);
    }

    map
}

fn parse_book(title: String, path: PathBuf, text: &str) -> RawBook {
    let (front, _body) = split_frontmatter(text);
    let lat = field(front, "lat").and_then(|v| v.parse::<f32>().ok());
    let lon = field(front, "lon").and_then(|v| v.parse::<f32>().ok());
    let coord = match (lat, lon) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    };
    RawBook {
        // frontmatter 里写了 title 就用它，否则用文件名
        title: field(front, "title").unwrap_or(title),
        path,
        author: field(front, "author").unwrap_or_default(),
        country: field(front, "country").unwrap_or_default(),
        region: field(front, "region"),
        rating: field(front, "rating").and_then(|v| v.parse::<u8>().ok()),
        coord,
        weeks: parse_weeks(front),
    }
}

/// 从 frontmatter 里取 `key: value`。只认最朴素的一行一键，够这个库用。
fn field(front: &str, key: &str) -> Option<String> {
    for line in front.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        let Some(val) = rest.trim_start().strip_prefix(':') else {
            continue;
        };
        let val = val.trim().trim_matches(['"', '\'']).trim();
        if !val.is_empty() {
            return Some(val.to_string());
        }
    }
    None
}

/// 归一化国家名：去空白、转小写，用来当聚合 key。
fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// 国家/地区 -> (纬度, 经度)。收录常见的文学国度，中英文名都认。
///
/// 只到国家粒度；更细的城市/地区可以在 frontmatter 里写 lat/lon 覆盖。
pub fn country_coord(name: &str) -> Option<(f32, f32)> {
    let n = normalize(name);
    let (lat, lon) = match n.as_str() {
        "france" | "法国" => (46.6, 2.4),
        "united kingdom" | "uk" | "britain" | "england" | "英国" | "英格兰" => (52.5, -1.5),
        "ireland" | "爱尔兰" => (53.3, -7.7),
        "germany" | "德国" => (51.2, 10.4),
        "russia" | "russian" | "俄罗斯" | "俄国" | "苏联" => (58.0, 40.0),
        "united states" | "usa" | "us" | "america" | "美国" => (39.0, -98.0),
        "japan" | "日本" => (36.2, 138.3),
        "china" | "中国" => (35.9, 104.2),
        "italy" | "意大利" => (42.8, 12.6),
        "spain" | "西班牙" => (40.2, -3.7),
        "portugal" | "葡萄牙" => (39.6, -8.0),
        "greece" | "希腊" => (39.1, 22.0),
        "austria" | "奥地利" => (47.6, 14.1),
        "switzerland" | "瑞士" => (46.8, 8.2),
        "czech" | "czechia" | "捷克" => (49.8, 15.5),
        "poland" | "波兰" => (52.1, 19.4),
        "norway" | "挪威" => (60.5, 8.5),
        "sweden" | "瑞典" => (60.1, 15.6),
        "denmark" | "丹麦" => (56.0, 9.5),
        "finland" | "芬兰" => (64.0, 26.0),
        "iceland" | "冰岛" => (64.9, -19.0),
        "netherlands" | "holland" | "荷兰" => (52.1, 5.3),
        "belgium" | "比利时" => (50.6, 4.5),
        "india" | "印度" => (22.0, 79.0),
        "turkey" | "türkiye" | "土耳其" => (39.0, 35.2),
        "iran" | "persia" | "伊朗" | "波斯" => (32.4, 53.7),
        "israel" | "以色列" => (31.5, 34.9),
        "egypt" | "埃及" => (26.8, 30.8),
        "nigeria" | "尼日利亚" => (9.1, 8.7),
        "south africa" | "南非" => (-30.6, 24.0),
        "kenya" | "肯尼亚" => (0.0, 37.9),
        "brazil" | "巴西" => (-14.2, -51.9),
        "argentina" | "阿根廷" => (-38.4, -63.6),
        "colombia" | "哥伦比亚" => (4.6, -74.3),
        "chile" | "智利" => (-35.7, -71.5),
        "peru" | "秘鲁" => (-9.2, -75.0),
        "mexico" | "墨西哥" => (23.6, -102.5),
        "canada" | "加拿大" => (56.1, -106.3),
        "australia" | "澳大利亚" => (-25.3, 133.8),
        "new zealand" | "新西兰" => (-41.0, 174.0),
        "korea" | "south korea" | "韩国" | "南韩" => (36.5, 127.8),
        "vietnam" | "越南" => (14.1, 108.3),
        "indonesia" | "印度尼西亚" | "印尼" => (-2.5, 118.0),
        "hungary" | "匈牙利" => (47.2, 19.5),
        "romania" | "罗马尼亚" => (45.9, 25.0),
        "ukraine" | "乌克兰" => (48.4, 31.2),
        "scotland" | "苏格兰" => (56.5, -4.2),
        _ => return None,
    };
    Some((lat, lon))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_book_frontmatter() {
        let text = "---\nauthor: Albert Camus\ncountry: France\nregion: Algiers\nrating: 5\ndate: 2026-W20\n---\n随便的读后感\n";
        let r = parse_book("异乡人".into(), PathBuf::new(), text);
        assert_eq!(r.author, "Albert Camus");
        assert_eq!(r.country, "France");
        assert_eq!(r.region.as_deref(), Some("Algiers"));
        assert_eq!(r.rating, Some(5));
        assert_eq!(r.weeks, vec![Week { year: 2026, week: 20 }]);
    }

    #[test]
    fn lat_lon_override_wins() {
        let text = "---\nauthor: X\ncountry: Nowhereland\nlat: 12.5\nlon: -3.0\n---\n";
        let r = parse_book("t".into(), PathBuf::new(), text);
        assert_eq!(r.coord, Some((12.5, -3.0)));
    }

    #[test]
    fn groups_books_under_authors_and_countries() {
        let raws = vec![
            RawBook {
                title: "The Stranger".into(),
                path: PathBuf::new(),
                author: "Camus".into(),
                country: "France".into(),
                region: None,
                rating: Some(5),
                coord: None,
                weeks: vec![],
            },
            RawBook {
                title: "The Plague".into(),
                path: PathBuf::new(),
                author: "camus".into(), // 大小写不同也算同一个作家
                country: "france".into(),
                region: None,
                rating: None,
                coord: None,
                weeks: vec![],
            },
            RawBook {
                title: "Kokoro".into(),
                path: PathBuf::new(),
                author: "Natsume Soseki".into(),
                country: "Japan".into(),
                region: None,
                rating: None,
                coord: None,
                weeks: vec![],
            },
        ];
        let m = build(raws);
        assert_eq!(m.countries.len(), 2);
        assert_eq!(m.authors.len(), 2);
        assert_eq!(m.books.len(), 3);
        // Camus 名下两本
        let camus = m.authors.iter().find(|a| a.name == "Camus").unwrap();
        assert_eq!(camus.books.len(), 2);
    }

    #[test]
    fn unknown_country_without_coord_is_counted_unplaced() {
        let raws = vec![RawBook {
            title: "t".into(),
            path: PathBuf::new(),
            author: "a".into(),
            country: "Nowhereland".into(),
            region: None,
            rating: None,
            coord: None,
            weeks: vec![],
        }];
        let m = build(raws);
        assert_eq!(m.unplaced, 1);
        assert_eq!(m.books.len(), 0);
    }

    #[test]
    fn coord_table_knows_common_countries() {
        assert!(country_coord("France").is_some());
        assert!(country_coord("日本").is_some());
        assert!(country_coord("俄罗斯").is_some());
        assert!(country_coord("Mars").is_none());
    }
}
