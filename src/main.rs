// 关掉 Windows 上多余的控制台窗口（debug 下保留，方便看 panic）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod hotkey;
mod layout;
mod vocab;

use std::path::PathBuf;

/// 没有从命令行给目录时，默认去哪儿找词库。
///
/// 每个平台把库放在各自习惯的位置：Windows 上是固定盘符，macOS 上放在
/// 用户主目录下的 WorkSpace/English_words（换用户名也能用，所以拼 HOME
/// 而不是写死 /Users/xxx）。
fn default_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"D:\Code\Vocab")
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
            .join("WorkSpace/English_words")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join("English_words")
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root: PathBuf = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(default_root);

    // 不开窗口，只把解析结果打出来，方便确认目录读对了
    if args.iter().any(|a| a == "--stats") {
        print_stats(&root);
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([760.0, 520.0])
            // 不要系统标题栏，自己画一个圆角卡片当窗口
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            .with_title("LEXIS · Word Atlas"),
        ..Default::default()
    };

    eframe::run_native(
        "LEXIS · Word Atlas",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, root)))),
    )
}

fn print_stats(root: &std::path::Path) {
    let g = match vocab::Graph::load(root) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };
    println!(
        "{} 个词 / {} 条联系 / {} 个周标签 / {} 个失效链接",
        g.nodes.len(),
        g.edges.len(),
        g.weeks.len(),
        g.dangling_links
    );

    let mut comp_size = std::collections::BTreeMap::new();
    for n in &g.nodes {
        *comp_size.entry(n.component).or_insert(0usize) += 1;
    }
    let mut sizes: Vec<usize> = comp_size.values().copied().collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    println!("连通分量 {} 个，最大的几个: {:?}", sizes.len(), &sizes[..sizes.len().min(8)]);

    println!("每周词数:");
    for (i, w) in g.weeks.iter().enumerate() {
        let c = g.nodes.iter().filter(|n| n.weeks.contains(&(i as u16))).count();
        println!("  {}  {}", w.label(), c);
    }
}
