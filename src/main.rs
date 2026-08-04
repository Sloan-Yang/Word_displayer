// 关掉 Windows 上多余的控制台窗口（debug 下保留，方便看 panic）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod globe;
mod hotkey;
mod layout;
mod reading;
mod single_instance;
mod vocab;

use std::path::PathBuf;

const DEFAULT_ROOT: &str = r"D:\Code\Vocab";
const APP_ICON_BYTES: &[u8] = include_bytes!("../assets/app_icon.png");

fn app_icon() -> egui::IconData {
    let image = image::load_from_memory(APP_ICON_BYTES)
        .expect("内置应用图标无法解码")
        .to_rgba8();
    egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root: PathBuf = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT));

    // 不开窗口，只把解析结果打出来，方便确认目录读对了
    if args.iter().any(|a| a == "--stats") {
        print_stats(&root);
        return Ok(());
    }

    let Some(instance_guard) = single_instance::acquire()? else {
        return Ok(());
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([760.0, 520.0])
            // 不要系统标题栏，自己画一个圆角卡片当窗口
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(true)
            .with_icon(app_icon())
            .with_title("LEXIS · Word Atlas"),
        // 阅读地图那颗三维地球要用深度缓冲，让正面半球盖住背面
        depth_buffer: 24,
        ..Default::default()
    };

    eframe::run_native(
        "LEXIS · Word Atlas",
        options,
        Box::new(move |cc| {
            instance_guard.listen_for_activation(cc.egui_ctx.clone());
            Ok(Box::new(app::App::new(cc, root)))
        }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::app_icon;

    #[test]
    fn bundled_app_icon_is_valid_rgba() {
        let icon = app_icon();
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] == 255));
    }
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
