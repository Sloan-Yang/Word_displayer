use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use egui::{
    Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Vec2,
};

use crate::layout::Sim;
use crate::vocab::Graph;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorMode {
    Component,
    Recency,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

/// 画布上所有颜色都从这里取，换主题时不会漏掉某个硬编码的色值。
struct Palette {
    bg_top: Color32,
    bg_bottom: Color32,
    grid: Color32,
    grid_alpha: f32,
    edge: [u8; 3],
    edge_hot: Color32,
    label: Color32,
    label_dim: Color32,
    ring_selected: Color32,
    ring_hover: Color32,
    ring_search: Color32,
    tip_bg: Color32,
    tip_border: Color32,
    tip_text: Color32,
    status: Color32,
    /// 节点配色的饱和度/明度，亮底要更深更饱和才压得住
    node_sat: f32,
    node_val: f32,
    /// 淡化非邻居节点时往这个颜色混，亮底往白混、暗底往黑混
    fade_to: Color32,
}

impl Palette {
    fn of(theme: Theme) -> Palette {
        match theme {
            Theme::Light => Palette {
                bg_top: Color32::from_rgb(252, 252, 251),
                bg_bottom: Color32::from_rgb(238, 238, 236),
                grid: Color32::from_rgb(40, 40, 45),
                grid_alpha: 42.0,
                edge: [96, 102, 116],
                edge_hot: Color32::from_rgb(196, 104, 18),
                label: Color32::from_rgb(28, 28, 32),
                label_dim: Color32::from_rgb(176, 176, 180),
                ring_selected: Color32::from_rgb(20, 20, 24),
                ring_hover: Color32::from_rgb(110, 110, 118),
                ring_search: Color32::from_rgb(214, 122, 16),
                tip_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 245),
                tip_border: Color32::from_rgb(198, 198, 200),
                tip_text: Color32::from_rgb(28, 28, 32),
                status: Color32::from_rgb(150, 150, 155),
                node_sat: 0.78,
                node_val: 0.74,
                fade_to: Color32::from_rgb(245, 245, 244),
            },
            Theme::Dark => Palette {
                bg_top: Color32::from_rgb(31, 31, 33),
                bg_bottom: Color32::from_rgb(20, 20, 21),
                grid: Color32::from_rgb(180, 180, 180),
                grid_alpha: 34.0,
                edge: [165, 168, 175],
                edge_hot: Color32::from_rgb(255, 214, 130),
                label: Color32::from_gray(232),
                label_dim: Color32::from_gray(90),
                ring_selected: Color32::WHITE,
                ring_hover: Color32::from_gray(220),
                ring_search: Color32::from_rgb(255, 200, 90),
                tip_bg: Color32::from_rgba_unmultiplied(24, 28, 36, 235),
                tip_border: Color32::from_gray(70),
                tip_text: Color32::from_gray(235),
                status: Color32::from_gray(110),
                node_sat: 0.55,
                node_val: 0.95,
                fade_to: Color32::from_rgb(24, 24, 26),
            },
        }
    }

    fn panel_fill(&self) -> Color32 {
        self.bg_bottom
    }
}

pub struct App {
    graph: Graph,
    load_error: Option<String>,
    root_input: String,

    // 筛选
    week_lo: usize,
    week_hi: usize,
    hops: usize,
    hide_isolated: bool,
    focus: Option<u32>,

    // 视图
    sim: Sim,
    cam: Pos2,
    zoom: f32,
    /// 布局还在铺开时持续跟拍；一旦用户自己平移/缩放就交还控制权。
    auto_fit: bool,

    // 交互
    selected: Option<u32>,
    hovered: Option<u32>,
    dragging: Option<usize>,
    search: String,

    // 外观
    drifting: bool,
    drift_speed: f32,
    theme: Theme,
    /// 已经套用到 egui 上的主题，变了才重新设置样式
    applied_theme: Option<Theme>,
    color_mode: ColorMode,
    show_labels: bool,
    label_limit: usize,
    edge_alpha: u8,
    node_scale: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, root: PathBuf) -> App {
        setup_style(&cc.egui_ctx);

        let (graph, load_error) = match Graph::load(&root) {
            Ok(g) => (g, None),
            Err(e) => (Graph::default(), Some(e)),
        };

        let last = graph.weeks.len().saturating_sub(1);
        let mut app = App {
            root_input: root.to_string_lossy().to_string(),
            week_lo: last.saturating_sub(3),
            week_hi: last,
            hops: 1,
            // 库里绝大多数词没有链接，默认藏掉，先看成团的部分
            hide_isolated: true,
            focus: None,
            graph,
            load_error,
            sim: Sim::new(Vec::new(), Vec::new()),
            cam: Pos2::ZERO,
            zoom: 1.0,
            auto_fit: true,
            selected: None,
            hovered: None,
            dragging: None,
            search: String::new(),
            drifting: true,
            drift_speed: 1.0,
            theme: Theme::Light,
            applied_theme: None,
            color_mode: ColorMode::Component,
            show_labels: true,
            label_limit: 600,
            edge_alpha: 60,
            node_scale: 1.0,
        };
        app.rebuild();
        app
    }

    // ------------------------------------------------------------ 选点与重建

    /// 依据当前筛选条件算出要显示的节点集合。
    fn compute_selection(&self) -> Vec<u32> {
        let g = &self.graph;
        let mut seeds: Vec<u32> = Vec::new();
        if let Some(f) = self.focus {
            seeds.push(f);
        } else {
            let (lo, hi) = (self.week_lo as u16, self.week_hi as u16);
            for (i, node) in g.nodes.iter().enumerate() {
                if node.weeks.iter().any(|&w| w >= lo && w <= hi) {
                    seeds.push(i as u32);
                }
            }
        }

        // 按跳数扩散，把有联系的词一起带出来
        let mut seen: HashSet<u32> = seeds.iter().copied().collect();
        let mut frontier: VecDeque<(u32, usize)> = seeds.iter().map(|&s| (s, 0usize)).collect();
        while let Some((cur, d)) = frontier.pop_front() {
            if d >= self.hops {
                continue;
            }
            for &nb in &g.nodes[cur as usize].neighbors {
                if seen.insert(nb) {
                    frontier.push_back((nb, d + 1));
                }
            }
        }

        let mut ids: Vec<u32> = seen.into_iter().collect();
        ids.sort_unstable();

        if self.hide_isolated {
            let set: HashSet<u32> = ids.iter().copied().collect();
            ids.retain(|&i| {
                g.nodes[i as usize]
                    .neighbors
                    .iter()
                    .any(|nb| set.contains(nb))
            });
        }
        ids
    }

    /// 当前时间范围内、但因为没有任何联系而被藏起来的词数。
    fn isolated_count(&self) -> usize {
        if !self.hide_isolated || self.focus.is_some() {
            return 0;
        }
        let (lo, hi) = (self.week_lo as u16, self.week_hi as u16);
        self.graph
            .nodes
            .iter()
            .filter(|n| n.neighbors.is_empty() && n.weeks.iter().any(|&w| w >= lo && w <= hi))
            .count()
    }

    fn rebuild(&mut self) {
        let ids = self.compute_selection();
        let local: HashMap<u32, usize> = ids.iter().enumerate().map(|(i, &g)| (g, i)).collect();

        let mut edges: Vec<(u32, u32)> = Vec::new();
        for (&a, _) in local.iter() {
            for &b in &self.graph.nodes[a as usize].neighbors {
                if a < b {
                    if let Some(&lb) = local.get(&b) {
                        edges.push((local[&a] as u32, lb as u32));
                    }
                }
            }
        }
        edges.sort_unstable();

        self.sim = Sim::new(ids, edges);
        self.auto_fit = true;
        if let Some(sel) = self.selected {
            if !self.sim.local.contains_key(&sel) {
                self.selected = None;
            }
        }
    }

    fn reload(&mut self) {
        let root = PathBuf::from(self.root_input.trim());
        match Graph::load(&root) {
            Ok(g) => {
                self.graph = g;
                self.load_error = None;
                self.focus = None;
                self.selected = None;
                let last = self.graph.weeks.len().saturating_sub(1);
                self.week_hi = last;
                self.week_lo = last.saturating_sub(3);
                self.rebuild();
            }
            Err(e) => self.load_error = Some(e),
        }
    }

    // ---------------------------------------------------------------- 相机

    /// 算出刚好装下整张图的相机参数。
    fn fit_target(&self, viewport: Rect) -> (Pos2, f32) {
        if self.sim.len() == 0 {
            return (Pos2::ZERO, 1.0);
        }
        let b = self.sim.bounds();
        // 留白按图本身的尺寸给，节点少的时候不至于被拉到超大
        let pad = (b.size().max_elem() * 0.06).max(40.0);
        let b = b.expand(pad);
        let zx = viewport.width() / b.width().max(1.0);
        let zy = viewport.height() / b.height().max(1.0);
        (b.center(), zx.min(zy).clamp(0.02, 2.5))
    }

    /// 布局铺开的过程中平滑跟拍，收敛后停在最终视野上。
    fn follow_layout(&mut self, viewport: Rect) {
        let (cam, zoom) = self.fit_target(viewport);
        let t = if self.sim.is_settled() { 0.2 } else { 0.12 };
        self.cam += (cam - self.cam) * t;
        self.zoom += (zoom - self.zoom) * t;

        // 跟到位了就不用再每帧重绘
        if self.sim.is_settled()
            && (cam - self.cam).length() < 0.5
            && (zoom - self.zoom).abs() < 0.001
        {
            self.cam = cam;
            self.zoom = zoom;
            self.auto_fit = false;
        }
    }

    fn to_screen(&self, center: Pos2, p: Pos2) -> Pos2 {
        center + (p - self.cam) * self.zoom
    }

    fn to_world(&self, center: Pos2, p: Pos2) -> Pos2 {
        self.cam + (p - center) / self.zoom
    }

    fn center_on(&mut self, global_id: u32) {
        if let Some(&li) = self.sim.local.get(&global_id) {
            self.cam = self.sim.pos[li];
            self.zoom = self.zoom.max(1.2);
            self.auto_fit = false;
        }
    }

    fn radius(&self, local: usize) -> f32 {
        let deg = self.sim.degree[local] as f32;
        (7.0 + 3.0 * deg.sqrt()) * self.node_scale
    }

    fn node_color(&self, global: u32, pal: &Palette) -> Color32 {
        let node = &self.graph.nodes[global as usize];
        match self.color_mode {
            ColorMode::Component => {
                let h = (node.component as f32 * 0.381_966_0).fract();
                hsv(h, pal.node_sat, pal.node_val)
            }
            ColorMode::Recency => {
                let total = self.graph.weeks.len().max(1) as f32;
                let t = node.last_week().unwrap_or(0) as f32 / total;
                // 旧 -> 新：靛蓝到暖橙
                hsv(0.62 - 0.50 * t, pal.node_sat, pal.node_val * (0.85 + 0.15 * t))
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.applied_theme != Some(self.theme) {
            apply_theme(ctx, self.theme);
            self.applied_theme = Some(self.theme);
        }
        let pal = Palette::of(self.theme);

        self.side_panel(ctx);
        self.detail_panel(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(pal.bg_bottom))
            .show(ctx, |ui| self.graph_view(ui, &pal));

        if !self.sim.is_settled() || self.auto_fit || self.drifting {
            ctx.request_repaint();
        }
    }
}

impl App {
    // ------------------------------------------------------------ 左侧面板

    fn side_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("controls")
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.heading("单词图网");
                ui.add_space(4.0);

                if let Some(err) = self.load_error.clone() {
                    ui.colored_label(Color32::from_rgb(200, 60, 60), err);
                }

                egui::CollapsingHeader::new("数据源")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.root_input).desired_width(f32::INFINITY));
                        if ui.button("重新加载").clicked() {
                            self.reload();
                        }
                    });

                ui.separator();

                // ---- 时间筛选 ----
                ui.label(RichText::new("时间筛选").strong());
                let n_weeks = self.graph.weeks.len();
                if n_weeks == 0 {
                    ui.weak("没有读到任何周标签");
                } else {
                    let labels: Vec<String> = self.graph.weeks.iter().map(|w| w.label()).collect();
                    let l1 = labels.clone();
                    let l2 = labels.clone();
                    let max = n_weeks - 1;

                    let mut changed = false;
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut self.week_lo, 0..=max)
                                .text("起")
                                .custom_formatter(move |v, _| {
                                    l1.get(v as usize).cloned().unwrap_or_default()
                                }),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut self.week_hi, 0..=max)
                                .text("止")
                                .custom_formatter(move |v, _| {
                                    l2.get(v as usize).cloned().unwrap_or_default()
                                }),
                        )
                        .changed();
                    if self.week_lo > self.week_hi {
                        // 拖过头时把另一端顶着走
                        if changed {
                            self.week_hi = self.week_lo;
                        }
                    }

                    ui.horizontal_wrapped(|ui| {
                        for (text, span) in [("最近 1 周", 1usize), ("4 周", 4), ("12 周", 12)] {
                            if ui.small_button(text).clicked() {
                                self.week_hi = max;
                                self.week_lo = max.saturating_sub(span - 1);
                                changed = true;
                            }
                        }
                        if ui.small_button("全部").clicked() {
                            self.week_lo = 0;
                            self.week_hi = max;
                            changed = true;
                        }
                    });

                    if changed {
                        self.focus = None;
                        self.rebuild();
                    }
                }

                ui.add_space(6.0);
                let mut structural = false;
                structural |= ui
                    .add(egui::Slider::new(&mut self.hops, 0..=3).text("扩展邻居跳数"))
                    .changed();
                structural |= ui.checkbox(&mut self.hide_isolated, "隐藏孤立节点").changed();
                if structural {
                    self.rebuild();
                }

                if let Some(f) = self.focus {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("聚焦: {}", self.graph.nodes[f as usize].name)).italics());
                        if ui.small_button("✕").clicked() {
                            self.focus = None;
                            self.rebuild();
                        }
                    });
                }

                ui.separator();

                // ---- 搜索 ----
                ui.label(RichText::new("搜索").strong());
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("输入单词，回车定位")
                        .desired_width(f32::INFINITY),
                );
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.jump_to_search();
                }

                ui.separator();

                // ---- 外观 ----
                egui::CollapsingHeader::new("外观")
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("主题");
                            ui.selectable_value(&mut self.theme, Theme::Light, "亮色");
                            ui.selectable_value(&mut self.theme, Theme::Dark, "暗色");
                        });
                        ui.horizontal(|ui| {
                            ui.label("配色");
                            ui.selectable_value(&mut self.color_mode, ColorMode::Component, "词群");
                            ui.selectable_value(&mut self.color_mode, ColorMode::Recency, "新旧");
                        });
                        ui.checkbox(&mut self.drifting, "气泡漂浮");
                        if self.drifting {
                            ui.add(
                                egui::Slider::new(&mut self.drift_speed, 0.1..=2.5).text("漂浮速度"),
                            );
                        }
                        ui.checkbox(&mut self.show_labels, "显示单词标签");
                        ui.add(egui::Slider::new(&mut self.label_limit, 50..=3000).text("标签上限"));
                        ui.add(egui::Slider::new(&mut self.node_scale, 0.4..=2.5).text("节点大小"));
                        ui.add(egui::Slider::new(&mut self.edge_alpha, 10..=180).text("连线浓度"));
                        if ui.button("重新排布 (R)").clicked() {
                                        self.rebuild();
                        }
                    });

                ui.separator();

                // ---- 统计 ----
                let shown = self.sim.len();
                let edges = self.sim.edges.len();
                ui.label(format!(
                    "显示 {shown} 个词 / {edges} 条联系 / {} 个词团",
                    self.sim.component_count()
                ));
                let hidden = self.isolated_count();
                if hidden > 0 {
                    ui.weak(format!("另有 {hidden} 个没有联系的词被隐藏"));
                }
                ui.weak(format!(
                    "库内共 {} 个词，{} 条联系，{} 周",
                    self.graph.nodes.len(),
                    self.graph.edges.len(),
                    self.graph.weeks.len()
                ));
                if self.graph.dangling_links > 0 {
                    ui.weak(format!("{} 个链接指向不存在的词", self.graph.dangling_links));
                }

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.add_space(6.0);
                    ui.weak("滚轮缩放 · 拖空白平移 · 拖节点 · 双击聚焦");
                });
            });
    }

    fn jump_to_search(&mut self) {
        let q = self.search.trim().to_lowercase();
        if q.is_empty() {
            return;
        }
        let hit = self
            .graph
            .find(&q)
            .or_else(|| {
                self.graph
                    .nodes
                    .iter()
                    .position(|n| n.name.to_lowercase().contains(&q))
                    .map(|i| i as u32)
            });
        if let Some(id) = hit {
            self.selected = Some(id);
            if self.sim.local.contains_key(&id) {
                self.center_on(id);
            } else {
                // 不在当前筛选里，就直接聚焦到它
                self.focus = Some(id);
                self.rebuild();
            }
        }
    }

    // ------------------------------------------------------------ 右侧详情

    fn detail_panel(&mut self, ctx: &egui::Context) {
        let Some(sel) = self.selected else { return };
        let node_name = self.graph.nodes[sel as usize].name.clone();
        let mut goto: Option<u32> = None;
        let mut focus_it = false;

        egui::SidePanel::right("detail")
            .default_width(240.0)
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading(&node_name);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            goto = None;
                            self.selected = None;
                        }
                    });
                });

                let node = &self.graph.nodes[sel as usize];
                ui.weak(
                    node.weeks
                        .iter()
                        .filter_map(|&w| self.graph.weeks.get(w as usize))
                        .map(|w| w.label())
                        .collect::<Vec<_>>()
                        .join("  "),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("以此为中心").clicked() {
                        focus_it = true;
                    }
                    if ui.button("打开 md").clicked() {
                        open_path(&node.path);
                    }
                });
                ui.separator();
                ui.label(RichText::new(format!("联系 ({})", node.degree())).strong());
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut names: Vec<(u32, &str)> = node
                        .neighbors
                        .iter()
                        .map(|&n| (n, self.graph.nodes[n as usize].name.as_str()))
                        .collect();
                    names.sort_by_key(|(_, n)| n.to_lowercase());
                    for (id, name) in names {
                        let in_view = self.sim.local.contains_key(&id);
                        let text = if in_view {
                            RichText::new(name)
                        } else {
                            RichText::new(name).weak()
                        };
                        if ui.selectable_label(false, text).clicked() {
                            goto = Some(id);
                        }
                    }
                });
            });

        if focus_it {
            self.focus = Some(sel);
            self.rebuild();
            self.auto_fit = true;
        }
        if let Some(id) = goto {
            self.selected = Some(id);
            if self.sim.local.contains_key(&id) {
                self.center_on(id);
            }
        }
    }

    // -------------------------------------------------------------- 图画布

    /// 画布底：中性深灰的竖向渐变 + 极淡的网格。刻意不带蓝，
    /// 让节点的彩色是画面里唯一的颜色。
    fn paint_background(&self, painter: &egui::Painter, rect: Rect, center: Pos2, pal: &Palette) {
        let top = pal.bg_top;
        let bottom = pal.bg_bottom;
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), top);
        mesh.colored_vertex(rect.right_top(), top);
        mesh.colored_vertex(rect.right_bottom(), bottom);
        mesh.colored_vertex(rect.left_bottom(), bottom);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));

        // 底纹：三角点阵。行间错开半格，看上去是斜向排列的点，
        // 没有方格那种硬邦邦的横平竖直；越靠画面边缘越淡，自带一圈暗角。
        let spacing = 78.0 * self.zoom;
        if spacing > 13.0 && spacing < 420.0 {
            let row_h = spacing * 0.866; // 正三角形的行高
            let origin = self.to_screen(center, Pos2::ZERO);
            let j0 = ((rect.top() - origin.y) / row_h).floor() as i64;
            let j1 = ((rect.bottom() - origin.y) / row_h).ceil() as i64;
            let i0 = ((rect.left() - origin.x - spacing) / spacing).floor() as i64;
            let i1 = ((rect.right() - origin.x) / spacing).ceil() as i64;

            // 点太多就不画了，缩得很远时底纹本来也没意义
            if (j1 - j0 + 1) * (i1 - i0 + 1) <= 8000 {
                let max_d = rect.size().length() * 0.5;
                // 缩放到临界值附近时整体淡出，避免突然出现/消失
                let scale_fade = ((spacing - 13.0) / 25.0).clamp(0.0, 1.0);
                let mut dots: Vec<Shape> = Vec::new();
                for j in j0..=j1 {
                    let y = origin.y + j as f32 * row_h;
                    let stagger = if j.rem_euclid(2) == 0 { 0.0 } else { spacing * 0.5 };
                    for i in i0..=i1 {
                        let p = Pos2::new(origin.x + stagger + i as f32 * spacing, y);
                        let t = 1.0 - ((p - center).length() / max_d).clamp(0.0, 1.0);
                        let a = pal.grid_alpha * scale_fade * (0.25 + 0.75 * t * t);
                        if a < 1.0 {
                            continue;
                        }
                        dots.push(Shape::circle_filled(
                            p,
                            1.4,
                            pal.grid.gamma_multiply(a / 255.0),
                        ));
                    }
                }
                painter.extend(dots);
            }
        }
    }

    fn graph_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        let center = rect.center();

        self.paint_background(&painter, rect, center, pal);

        // ---- 物理迭代 ----
        if !self.sim.is_settled() {
            // 还在铺开：跑退火布局，每帧多走两步收敛快一点
            for _ in 0..2 {
                self.sim.step();
            }
        } else if self.drifting {
            // 已经定型：转成气泡漂浮。固定步长，跟帧率无关，才不会忽快忽慢
            self.sim.drift = self.drift_speed;
            self.sim.node_scale = self.node_scale;
            self.sim.drift_step(1.0 / 60.0);
        }

        // 力导向会把图铺得比初始撒点大好几倍，所以在收敛过程中一直跟拍，
        // 否则第一帧对好的视野很快就被甩在外面，画面看上去就是一片空白。
        if self.auto_fit {
            self.follow_layout(rect);
        }

        // ---- 缩放 / 平移 ----
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.1 {
                let anchor = pointer.unwrap_or(center);
                let before = self.to_world(center, anchor);
                self.zoom = (self.zoom * (scroll * 0.0015).exp()).clamp(0.02, 20.0);
                let after = self.to_world(center, anchor);
                self.cam -= after - before;
                self.auto_fit = false;
            }
        }

        // ---- 命中测试 ----
        let mut hover: Option<usize> = None;
        if let Some(p) = pointer {
            if rect.contains(p) {
                let world = self.to_world(center, p);
                let mut best = f32::INFINITY;
                for i in 0..self.sim.len() {
                    let r = (self.radius(i) + 4.0 / self.zoom).max(6.0 / self.zoom);
                    let d = (self.sim.pos[i] - world).length();
                    if d < r && d < best {
                        best = d;
                        hover = Some(i);
                    }
                }
            }
        }
        self.hovered = hover.map(|i| self.sim.ids[i]);

        // ---- 拖拽 ----
        if resp.drag_started() {
            self.dragging = hover;
            self.sim.pinned = hover;
        }
        if resp.dragged() {
            let delta = resp.drag_delta();
            match self.dragging {
                Some(i) => {
                    self.sim.pos[i] += delta / self.zoom;
                    self.sim.reheat(0.25);
                }
                None => {
                    self.cam -= delta / self.zoom;
                    self.auto_fit = false;
                }
            }
        }
        if resp.drag_stopped() {
            self.dragging = None;
            self.sim.pinned = None;
        }

        if resp.clicked() {
            self.selected = hover.map(|i| self.sim.ids[i]);
        }
        if resp.double_clicked() {
            if let Some(i) = hover {
                self.focus = Some(self.sim.ids[i]);
                self.selected = Some(self.sim.ids[i]);
                self.rebuild();
            }
        }

        // 快捷键
        ui.input(|i| {
            if i.key_pressed(egui::Key::F) {
                self.auto_fit = true;
            }
            if i.key_pressed(egui::Key::Escape) {
                self.selected = None;
            }
        });

        if self.sim.len() == 0 {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "这个时间范围内没有单词，试试把滑块拉宽一些",
                FontId::proportional(16.0),
                pal.status,
            );
            return;
        }

        // ---- 高亮集合 ----
        let highlight = self.hovered.or(self.selected);
        let highlight_local = highlight.and_then(|g| self.sim.local.get(&g).copied());
        let mut near: HashSet<usize> = HashSet::new();
        if let Some(h) = highlight_local {
            near.insert(h);
            for &nb in &self.graph.nodes[self.sim.ids[h] as usize].neighbors {
                if let Some(&l) = self.sim.local.get(&nb) {
                    near.insert(l);
                }
            }
        }
        let dim = highlight_local.is_some();

        let query = self.search.trim().to_lowercase();

        // ---- 画边 ----
        let mut shapes: Vec<Shape> = Vec::with_capacity(self.sim.edges.len() + self.sim.len());
        let base_edge =
            Color32::from_rgba_unmultiplied(pal.edge[0], pal.edge[1], pal.edge[2], self.edge_alpha);
        let dim_edge =
            Color32::from_rgba_unmultiplied(pal.edge[0], pal.edge[1], pal.edge[2], self.edge_alpha / 3);
        let hot_edge = pal.edge_hot;
        for &(a, b) in &self.sim.edges {
            let (a, b) = (a as usize, b as usize);
            let pa = self.to_screen(center, self.sim.pos[a]);
            let pb = self.to_screen(center, self.sim.pos[b]);
            if !segment_visible(rect, pa, pb) {
                continue;
            }
            let hot = highlight_local.is_some_and(|h| h == a || h == b);
            let (color, w) = if hot {
                (hot_edge, 1.8)
            } else if dim {
                (dim_edge, 1.0)
            } else {
                (base_edge, 1.0)
            };
            shapes.push(Shape::line_segment([pa, pb], Stroke::new(w, color)));
        }
        painter.extend(shapes);

        // ---- 画点 ----
        let mut shapes: Vec<Shape> = Vec::with_capacity(self.sim.len() * 2);
        let mut labels: Vec<(Pos2, f32, String, Color32)> = Vec::new();
        let visible_r = rect.expand(40.0);
        let mut visible_count = 0usize;

        for i in 0..self.sim.len() {
            let sp = self.to_screen(center, self.sim.pos[i]);
            if !visible_r.contains(sp) {
                continue;
            }
            visible_count += 1;
            let gid = self.sim.ids[i];
            let r = (self.radius(i) * self.zoom).clamp(2.0, 60.0);
            let mut color = self.node_color(gid, pal);
            let is_near = near.contains(&i);
            if dim && !is_near {
                color = fade(color, pal.fade_to, 0.78);
            }

            shapes.push(Shape::circle_filled(sp, r, color));

            // 选中 / 搜索命中的描边
            let name_lc = self.graph.nodes[gid as usize].name.to_lowercase();
            if Some(gid) == self.selected {
                shapes.push(Shape::circle_stroke(
                    sp,
                    r + 3.0,
                    Stroke::new(2.0, pal.ring_selected),
                ));
            } else if !query.is_empty() && name_lc.contains(&query) {
                shapes.push(Shape::circle_stroke(
                    sp,
                    r + 3.0,
                    Stroke::new(2.0, pal.ring_search),
                ));
            } else if Some(gid) == self.hovered {
                shapes.push(Shape::circle_stroke(
                    sp,
                    r + 3.0,
                    Stroke::new(1.5, pal.ring_hover),
                ));
            }

            if self.show_labels {
                let text_color = if dim && !is_near {
                    pal.label_dim
                } else {
                    pal.label
                };
                labels.push((
                    sp + Vec2::new(0.0, r + 2.0),
                    r,
                    self.graph.nodes[gid as usize].name.clone(),
                    text_color,
                ));
            }
        }
        painter.extend(shapes);

        // 标签：大节点优先，已经被占掉的位置就不再画，避免单词叠在一起看不清
        if self.show_labels {
            labels.sort_by(|a, b| b.1.total_cmp(&a.1));
            labels.truncate(self.label_limit);
            let font = FontId::proportional((12.0 * self.zoom.clamp(0.8, 1.4)).clamp(11.0, 16.0));
            let mut taken: Vec<Rect> = Vec::with_capacity(labels.len());
            for (pos, _, text, color) in labels {
                let galley = painter.layout_no_wrap(text, font.clone(), color);
                let area = Rect::from_min_size(
                    pos - Vec2::new(galley.size().x * 0.5, 0.0),
                    galley.size(),
                )
                .expand2(Vec2::new(2.0, 1.0));
                if taken.iter().any(|t| t.intersects(area)) {
                    continue;
                }
                taken.push(area);
                painter.galley(area.min + Vec2::new(2.0, 1.0), galley, color);
            }
        }

        // ---- 悬停提示 ----
        if let Some(h) = highlight_local {
            if self.hovered.is_some() {
                let node = &self.graph.nodes[self.sim.ids[h] as usize];
                let weeks = node
                    .weeks
                    .iter()
                    .filter_map(|&w| self.graph.weeks.get(w as usize))
                    .map(|w| w.label())
                    .collect::<Vec<_>>()
                    .join(" ");
                let tip = format!("{}\n{} 条联系\n{}", node.name, node.degree(), weeks);
                let anchor = self.to_screen(center, self.sim.pos[h]) + Vec2::new(14.0, 14.0);
                let galley = painter.layout_no_wrap(tip, FontId::proportional(12.0), pal.tip_text);
                let bg = Rect::from_min_size(anchor, galley.size()).expand(6.0);
                painter.rect(
                    bg,
                    4.0,
                    pal.tip_bg,
                    Stroke::new(1.0, pal.tip_border),
                    StrokeKind::Inside,
                );
                painter.galley(anchor, galley, pal.tip_text);
            }
        }

        // ---- 左下角状态 ----
        painter.text(
            rect.left_bottom() + Vec2::new(10.0, -10.0),
            Align2::LEFT_BOTTOM,
            format!("视野内 {visible_count} / {} 个词", self.sim.len()),
            FontId::proportional(11.0),
            pal.status,
        );
    }
}

// ------------------------------------------------------------------ 小工具

fn segment_visible(rect: Rect, a: Pos2, b: Pos2) -> bool {
    // 粗筛：两端点的包围盒和视口相交即认为可见
    Rect::from_two_pos(a, b).intersects(rect)
}

/// 把颜色往背景色混，`t` 越大越接近背景。暗底往黑混、亮底往白混，
/// 直接乘系数在亮背景上会越淡越黑，反而更扎眼。
fn fade(c: Color32, toward: Color32, t: f32) -> Color32 {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
    Color32::from_rgb(
        mix(c.r(), toward.r()),
        mix(c.g(), toward.g()),
        mix(c.b(), toward.b()),
    )
}

fn hsv(h: f32, s: f32, v: f32) -> Color32 {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color32::from_rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

fn open_path(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn setup_style(ctx: &egui::Context) {
    install_cjk_font(ctx);
}

fn apply_theme(ctx: &egui::Context, theme: Theme) {
    let pal = Palette::of(theme);
    ctx.set_visuals(match theme {
        Theme::Light => egui::Visuals::light(),
        Theme::Dark => egui::Visuals::dark(),
    });
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.slider_width = 170.0;
        s.visuals.window_corner_radius = 6.into();
        s.visuals.panel_fill = pal.panel_fill();
        s.visuals.widgets.noninteractive.bg_stroke.color = match theme {
            Theme::Light => Color32::from_gray(205),
            Theme::Dark => Color32::from_gray(48),
        };
    });
}

/// 界面文字是中文，默认字体没有汉字，这里挂一个系统中文字体（找不到就算了）。
fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &[
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhl.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ];
    for path in CANDIDATES {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        // 作为后备字体追加：拉丁字母仍用默认字体，好看一些
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .push("cjk".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .push("cjk".to_owned());
        ctx.set_fonts(fonts);
        return;
    }
}
