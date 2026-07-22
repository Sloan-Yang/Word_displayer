//! 读取 Vocab 目录里的 md 文件，解析成一张词图。
//!
//! 每个 md 形如：
//! ```text
//! ---
//! id: abalone
//! aliases: []
//! tags:
//!   - modified/2026-W02
//! ---
//! # abalone
//! [[oyster]]
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

/// 一个 ISO 周标签，例如 2026-W02。用 (year, week) 排序即时间序。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Week {
    pub year: u16,
    pub week: u8,
}

impl Week {
    pub fn label(&self) -> String {
        format!("{}-W{:02}", self.year, self.week)
    }
}

pub struct Node {
    pub name: String,
    pub path: PathBuf,
    /// 该词出现过的所有周（已排序去重），存的是 `Graph::weeks` 里的下标。
    pub weeks: Vec<u16>,
    /// 该笔记的标签（除去 modified/*），存的是 `Graph::tags` 里的下标。
    pub tags: Vec<u16>,
    /// 无向邻居（全局节点下标，已去重）。
    pub neighbors: Vec<u32>,
    /// 连通分量编号，用来给节点上色。
    pub component: u32,
}

impl Node {
    pub fn degree(&self) -> usize {
        self.neighbors.len()
    }

    /// 最新一次被修改的周（下标）。
    pub fn last_week(&self) -> Option<u16> {
        self.weeks.last().copied()
    }
}

#[derive(Default)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<(u32, u32)>,
    /// 全库出现过的所有周，升序。节点里的 weeks 存的是这里的下标。
    pub weeks: Vec<Week>,
    /// 全库出现过的所有标签（除去 modified/*），升序去重。
    pub tags: Vec<String>,
    /// 小写名字 -> 节点下标，用于解析链接（Obsidian 的链接大小写不敏感）。
    by_name: HashMap<String, u32>,
    /// 指向不存在的 md 的链接数量，用来提示数据里的笔误。
    pub dangling_links: usize,
}

impl Graph {
    pub fn find(&self, name: &str) -> Option<u32> {
        self.by_name.get(&name.to_lowercase()).copied()
    }

    pub fn load(root: &Path) -> Result<Graph, String> {
        if !root.is_dir() {
            return Err(format!("目录不存在: {}", root.display()));
        }

        // 1. 收集所有 md 文件
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

        // 2. 并行解析
        let parsed: Vec<Parsed> = files
            .par_iter()
            .filter_map(|path| {
                let text = std::fs::read_to_string(path).ok()?;
                let name = path.file_stem()?.to_string_lossy().to_string();
                Some(parse_one(name, path.clone(), &text))
            })
            .collect();

        // 3. 建名字索引
        let mut by_name: HashMap<String, u32> = HashMap::with_capacity(parsed.len() * 2);
        for (i, p) in parsed.iter().enumerate() {
            by_name.entry(p.name.to_lowercase()).or_insert(i as u32);
        }

        // 4. 全局周表
        let mut all_weeks: Vec<Week> = parsed.iter().flat_map(|p| p.weeks.iter().copied()).collect();
        all_weeks.sort_unstable();
        all_weeks.dedup();
        let week_idx: HashMap<Week, u16> = all_weeks
            .iter()
            .enumerate()
            .map(|(i, w)| (*w, i as u16))
            .collect();

        // 4b. 全局标签表
        let mut all_tags: Vec<String> =
            parsed.iter().flat_map(|p| p.tags.iter().cloned()).collect();
        all_tags.sort_unstable();
        all_tags.dedup();
        let tag_idx: HashMap<&str, u16> = all_tags
            .iter()
            .enumerate()
            .map(|(i, t)| (t.as_str(), i as u16))
            .collect();

        // 5. 解析链接 -> 边
        let mut nodes: Vec<Node> = parsed
            .iter()
            .map(|p| {
                let mut weeks: Vec<u16> = p.weeks.iter().filter_map(|w| week_idx.get(w).copied()).collect();
                weeks.sort_unstable();
                weeks.dedup();
                let mut tags: Vec<u16> =
                    p.tags.iter().filter_map(|t| tag_idx.get(t.as_str()).copied()).collect();
                tags.sort_unstable();
                tags.dedup();
                Node {
                    name: p.name.clone(),
                    path: p.path.clone(),
                    weeks,
                    tags,
                    neighbors: Vec::new(),
                    component: 0,
                }
            })
            .collect();

        let mut dangling = 0usize;
        let mut edges: Vec<(u32, u32)> = Vec::new();
        for (i, p) in parsed.iter().enumerate() {
            let a = i as u32;
            for link in &p.links {
                match by_name.get(&link.to_lowercase()) {
                    Some(&b) if b != a => {
                        // 统一方向后去重，得到无向边
                        edges.push((a.min(b), a.max(b)));
                    }
                    Some(_) => {}
                    None => dangling += 1,
                }
            }
        }
        edges.sort_unstable();
        edges.dedup();

        for &(a, b) in &edges {
            nodes[a as usize].neighbors.push(b);
            nodes[b as usize].neighbors.push(a);
        }

        let mut graph = Graph {
            nodes,
            edges,
            weeks: all_weeks,
            tags: all_tags,
            by_name,
            dangling_links: dangling,
        };
        graph.compute_components();
        Ok(graph)
    }

    /// 并查集式的 BFS，给每个连通分量编号（按大小降序，0 是最大的那团）。
    fn compute_components(&mut self) {
        let n = self.nodes.len();
        let mut comp = vec![u32::MAX; n];
        let mut sizes: Vec<(u32, usize)> = Vec::new();
        let mut stack: Vec<u32> = Vec::new();
        let mut next = 0u32;

        for start in 0..n {
            if comp[start] != u32::MAX {
                continue;
            }
            let id = next;
            next += 1;
            let mut size = 0usize;
            stack.push(start as u32);
            comp[start] = id;
            while let Some(cur) = stack.pop() {
                size += 1;
                for &nb in &self.nodes[cur as usize].neighbors {
                    if comp[nb as usize] == u32::MAX {
                        comp[nb as usize] = id;
                        stack.push(nb);
                    }
                }
            }
            sizes.push((id, size));
        }

        // 按分量大小排名重编号，保证配色稳定且大团拿到靠前的颜色
        sizes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut rank = vec![0u32; next as usize];
        for (r, (id, _)) in sizes.iter().enumerate() {
            rank[*id as usize] = r as u32;
        }
        for (i, node) in self.nodes.iter_mut().enumerate() {
            node.component = rank[comp[i] as usize];
        }
    }
}

struct Parsed {
    name: String,
    path: PathBuf,
    weeks: Vec<Week>,
    tags: Vec<String>,
    links: Vec<String>,
}

fn parse_one(name: String, path: PathBuf, text: &str) -> Parsed {
    let (front, body) = split_frontmatter(text);
    Parsed {
        name,
        path,
        weeks: parse_weeks(front),
        tags: parse_tags(front),
        links: parse_links(body),
    }
}

/// 返回 (frontmatter, 正文)。没有 frontmatter 时前半为空。
fn split_frontmatter(text: &str) -> (&str, &str) {
    let t = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = match t.strip_prefix("---\n").or_else(|| t.strip_prefix("---\r\n")) {
        Some(r) => r,
        None => return ("", t),
    };
    // 找到独占一行的结束分隔符
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return (&rest[..offset], &rest[offset + line.len()..]);
        }
        offset += line.len();
    }
    ("", t)
}

/// 从 frontmatter 里抓所有 `modified/YYYY-Www`（也接受其它前缀的同格式标签）。
fn parse_weeks(front: &str) -> Vec<Week> {
    let mut out = Vec::new();
    let bytes = front.as_bytes();
    let mut i = 0usize;
    while i + 8 <= bytes.len() {
        // 匹配 YYYY-W## 或 YYYY-W#
        if bytes[i].is_ascii_digit()
            && bytes[i + 1..i + 4].iter().all(u8::is_ascii_digit)
            && bytes[i + 4] == b'-'
            && (bytes[i + 5] == b'W' || bytes[i + 5] == b'w')
            && bytes[i + 6].is_ascii_digit()
        {
            let year: u16 = front[i..i + 4].parse().unwrap_or(0);
            let mut j = i + 6;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let week: u8 = front[i + 6..j].parse().unwrap_or(0);
            if year > 1900 && (1..=53).contains(&week) {
                out.push(Week { year, week });
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// 从 frontmatter 的 tags 列表里抓标签，去掉 `modified/*` 那些时间标签。
///
/// 只认块状写法（每行 `  - xxx`）。这库的 frontmatter 里唯一的列表就是 tags
/// （aliases 是空的 `[]`），所以扫所有 `- item` 行就够了，不必真解析 YAML。
fn parse_tags(front: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in front.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix("- ") else {
            continue;
        };
        let tag = rest.trim().trim_matches(['"', '\'']).trim();
        if tag.is_empty() || tag.starts_with("modified/") {
            continue;
        }
        out.push(tag.to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// 抓正文里的 `[[target]]` / `[[target|别名]]` / `[[target#小节]]`。
fn parse_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("[[") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find("]]") else { break };
        let inner = &rest[..end];
        rest = &rest[end + 2..];
        let target = inner
            .split(['|', '#'])
            .next()
            .unwrap_or("")
            .trim();
        if !target.is_empty() && !target.contains('\n') {
            out.push(target.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_typical_note() {
        let text = "---\nid: abalone\naliases: []\ntags:\n  - modified/2026-W02\n  - modified/2025-W51\n---\n# abalone\n[[oyster]]\n[[shellfish|贝类]]\n";
        let p = parse_one("abalone".into(), PathBuf::new(), text);
        assert_eq!(
            p.weeks,
            vec![Week { year: 2025, week: 51 }, Week { year: 2026, week: 2 }]
        );
        assert_eq!(p.links, vec!["oyster".to_string(), "shellfish".to_string()]);
    }

    #[test]
    fn note_without_frontmatter_still_yields_links() {
        let p = parse_one("x".into(), PathBuf::new(), "# x\n[[y]]\n");
        assert!(p.weeks.is_empty());
        assert_eq!(p.links, vec!["y".to_string()]);
    }

    #[test]
    fn parses_tags_and_drops_modified() {
        let text = "---\naliases: []\ntags:\n  - academic\n  - modified/2025-W52\n  - Paper\n  - modified/2026-W01\n---\nbody\n";
        let p = parse_one("Academic".into(), PathBuf::new(), text);
        // modified/* 不算标签；其余按名字排序去重
        assert_eq!(p.tags, vec!["Paper".to_string(), "academic".to_string()]);
        assert_eq!(p.weeks.len(), 2);
    }

    #[test]
    fn tag_index_attaches_to_the_right_note() {
        let dir = std::env::temp_dir().join(format!("wd_tagtest_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("A.md"),
            "---\ntags:\n  - academic\n  - modified/2025-W01\n---\n[[B]]\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("B.md"),
            "---\ntags:\n  - modified/2025-W01\n---\nplain\n",
        )
        .unwrap();

        let g = Graph::load(&dir).unwrap();
        let acad = g.tags.iter().position(|t| t == "academic").unwrap() as u16;
        let a = g.find("A").unwrap() as usize;
        let b = g.find("B").unwrap() as usize;
        assert!(g.nodes[a].tags.contains(&acad));
        assert!(!g.nodes[b].tags.contains(&acad));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
