use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use egui::{
    Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Shape, Stroke, StrokeKind, Vec2,
};

use crate::hotkey;
use crate::layout::{self, Sim};
use crate::vocab::{Graph, Week};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

/// 词团配色。
///
/// 一个视觉属性只表达一种含义 —— 颜色只表示「属于哪个词团」，
/// 别的什么都不代表。所以这里是一组饱和度压得比较低的柔和色，
/// 按词团序号循环取用，而不是每个团都算一个新色相。
const CLUSTER_LIGHT: [Color32; 6] = [
    Color32::from_rgb(0x5B, 0x8D, 0xEF), // 蓝
    Color32::from_rgb(0x47, 0xB8, 0xB2), // 青
    Color32::from_rgb(0x77, 0xB8, 0x5C), // 绿
    Color32::from_rgb(0xE6, 0xA3, 0x4A), // 橙
    Color32::from_rgb(0xDF, 0x6B, 0x63), // 红
    Color32::from_rgb(0x95, 0x75, 0xCD), // 紫
];

/// 单词在世界坐标下的基准字号。渲染时乘以 zoom，所以节点占的地方
/// 在世界坐标里是固定的 —— 这是「让单词参与碰撞」的前提。
const LABEL_FONT: f32 = 13.0;
/// 窗口边缘多宽的一圈用来拖拽改变大小
const RESIZE_EDGE: f32 = 7.0;
/// 窗口的圆角半径（逻辑像素）。给得大一点，是圆角长方形而不是普通方窗
const WINDOW_RADIUS: f32 = 34.0;
/// 窗口的默认尺寸，也是被拖得过小时的复原尺寸
const DEFAULT_WINDOW: egui::Vec2 = egui::vec2(1440.0, 900.0);
/// 小于这个尺寸就认为已经没法操作了
const MIN_WINDOW: egui::Vec2 = egui::vec2(420.0, 320.0);
/// Windows 的 Win+Shift+方向键跨屏移动会经历几帧过渡态；延后并重试归位。
const MONITOR_SWITCH_SNAP_FRAMES: u32 = 12;

/// 重新读取词库后，周列表可能因为新增/删除笔记而改变下标。
/// 按周标签重新定位旧范围，不能直接复用下标，也不能重置成最近四周。
fn remap_week_range(
    old_weeks: &[Week],
    old_lo: usize,
    old_hi: usize,
    new_weeks: &[Week],
) -> (usize, usize) {
    if new_weeks.is_empty() {
        return (0, 0);
    }

    let (Some(&lo), Some(&hi)) = (old_weeks.get(old_lo), old_weeks.get(old_hi)) else {
        let last = new_weeks.len() - 1;
        return (last.saturating_sub(3), last);
    };

    fn nearest(weeks: &[Week], target: Week) -> usize {
        match weeks.binary_search(&target) {
            Ok(index) => index,
            Err(0) => 0,
            Err(index) if index == weeks.len() => weeks.len() - 1,
            Err(index) => {
                let ordinal = |week: Week| i32::from(week.year) * 53 + i32::from(week.week);
                let target = ordinal(target);
                let before = (target - ordinal(weeks[index - 1])).abs();
                let after = (ordinal(weeks[index]) - target).abs();
                if before <= after {
                    index - 1
                } else {
                    index
                }
            }
        }
    }

    let lo = nearest(new_weeks, lo);
    let hi = nearest(new_weeks, hi);
    (lo.min(hi), lo.max(hi))
}

/// 画布上所有颜色都从这里取，换主题时不会漏掉某个硬编码的色值。
struct Palette {
    panel: Color32,
    /// 平时的连线：非常淡
    edge_base: Color32,
    /// 有节点被选中/悬停时，无关连线退到这个浓度
    edge_mute: Color32,
    text_strong: Color32,
    text_weak: Color32,
    accent: Color32,
    tip_bg: Color32,
    tip_border: Color32,
    /// 词团色往这个颜色混，调出气泡的填充；亮色主题混白，暗色主题混黑
    chip_base: Color32,
    /// 填充的上下两端各混多少
    chip_top_mix: f32,
    chip_bottom_mix: f32,
    /// 描边色相对原色的深浅
    chip_border_mix: f32,
    /// 被淡化的节点往背景混多少
    dim: f32,
    /// 背景图的染色。图本身是浅色的，暗色主题靠这个把它压暗
    bg_tint: Color32,
    /// 盖在背景图上的一层薄纱，用来压住底图、保证胶囊读得清
    bg_veil: Color32,
    /// 自绘窗口的描边
    window_border: Color32,
    /// 浮在圆里的控制卡片底色
    card: Color32,
}

impl Palette {
    fn of(theme: Theme) -> Palette {
        match theme {
            Theme::Light => Palette {
                panel: Color32::from_rgb(0xFF, 0xFF, 0xFF),
                edge_base: Color32::from_rgba_unmultiplied(0x5A, 0x69, 0x7D, 74),
                edge_mute: Color32::from_rgba_unmultiplied(0x5A, 0x69, 0x7D, 20),
                text_strong: Color32::from_rgb(0x25, 0x2A, 0x34),
                text_weak: Color32::from_rgb(0x73, 0x7A, 0x86),
                accent: Color32::from_rgb(0xE6, 0x8A, 0x2A),
                tip_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 246),
                tip_border: Color32::from_rgb(0xDD, 0xE1, 0xE7),
                chip_base: Color32::WHITE,
                chip_top_mix: 0.86,
                chip_bottom_mix: 0.68,
                chip_border_mix: 0.30,
                dim: 0.80,
                bg_tint: Color32::WHITE,
                bg_veil: Color32::TRANSPARENT,
                window_border: Color32::from_rgb(0xD8, 0xDD, 0xE4),
                card: Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 232),
            },
            Theme::Dark => Palette {
                panel: Color32::from_rgb(0x21, 0x23, 0x28),
                edge_base: Color32::from_rgba_unmultiplied(0xA8, 0xB2, 0xC0, 70),
                edge_mute: Color32::from_rgba_unmultiplied(0xA8, 0xB2, 0xC0, 22),
                text_strong: Color32::from_rgb(0xE8, 0xEB, 0xF0),
                text_weak: Color32::from_rgb(0x8A, 0x92, 0x9E),
                accent: Color32::from_rgb(0xF0, 0xA9, 0x4C),
                tip_bg: Color32::from_rgba_unmultiplied(0x2A, 0x2D, 0x34, 246),
                tip_border: Color32::from_rgb(0x3C, 0x40, 0x48),
                chip_base: Color32::from_rgb(0x1E, 0x20, 0x25),
                chip_top_mix: 0.55,
                chip_bottom_mix: 0.72,
                chip_border_mix: 0.15,
                dim: 0.82,
                // 乘法染色：把这张浅色底图压成深蓝灰
                bg_tint: Color32::from_rgb(52, 56, 64),
                bg_veil: Color32::from_rgba_unmultiplied(16, 17, 20, 92),
                window_border: Color32::from_rgb(0x3A, 0x3E, 0x46),
                card: Color32::from_rgba_unmultiplied(0x25, 0x28, 0x2E, 236),
            },
        }
    }

    /// 某个词团的主色
    fn cluster(&self, component: u32) -> Color32 {
        CLUSTER_LIGHT[component as usize % CLUSTER_LIGHT.len()]
    }
}

pub struct App {
    ctx: egui::Context,
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

    // 后台常驻 / 全局热键
    hotkey_signal: hotkey::Signal,
    hotkey_registered: bool,
    /// 只有点了「退出」才真的关掉，点 X 是收进后台
    allow_exit: bool,
    /// 首帧才建图：量文字尺寸要用字体，而字体在 Context::run 之前还没准备好
    pending_rebuild: bool,
    /// 还剩几帧去尝试摆窗口位置。屏幕尺寸要等 winit 报上来，不是立刻就有的
    place_window: u32,
    /// 上一次观察到的显示器，用来发现 Win+Shift+方向键这类系统跨屏移动
    last_monitor: Option<isize>,
    /// 这次跨屏移动最终要吸附到哪块屏，避免重试时又按过渡位置猜回旧屏
    monitor_snap_target: Option<isize>,
    /// 跨屏移动后还剩几帧继续把窗口吸回当前屏幕的顶端居中
    monitor_snap_frames: u32,
    /// 连续多少帧观测到窗口小得没法用了。跨屏换 DPI 时会瞬间读到异常值，
    /// 必须连续成立一段时间才动手，否则会误伤
    tiny_frames: u32,
    /// 背景图。解码要用 Context，所以也是首帧才上传
    backdrop: Option<egui::TextureHandle>,
    /// 左侧栏的背景图
    sidebar_backdrop: Option<egui::TextureHandle>,
    /// 左侧面板是否展开
    show_sidebar: bool,
    /// 下一帧把输入焦点交给搜索框（Ctrl+F 触发）
    focus_search: bool,

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
    edge_alpha: u16,
    node_scale: f32,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, root: PathBuf) -> App {
        setup_style(&cc.egui_ctx);

        let (graph, load_error) = match Graph::load(&root) {
            Ok(g) => (g, None),
            Err(e) => (Graph::default(), Some(e)),
        };

        let hotkey_signal = hotkey::new_signal();
        let hotkey_registered = hotkey::spawn(cc.egui_ctx.clone(), hotkey_signal.clone());

        let last = graph.weeks.len().saturating_sub(1);
        let app = App {
            ctx: cc.egui_ctx.clone(),
            root_input: root.to_string_lossy().to_string(),
            week_lo: last.saturating_sub(3),
            week_hi: last,
            hops: 1,
            // 库里绝大多数词没有链接，默认藏掉，先看成团的部分
            hide_isolated: true,
            focus: None,
            graph,
            load_error,
            sim: Sim::new(Vec::new(), Vec::new(), Vec::new()),
            cam: Pos2::ZERO,
            zoom: 1.0,
            auto_fit: true,
            selected: None,
            hovered: None,
            dragging: None,
            search: String::new(),
            hotkey_signal,
            hotkey_registered,
            allow_exit: false,
            pending_rebuild: true,
            place_window: 60,
            last_monitor: None,
            monitor_snap_target: None,
            monitor_snap_frames: 0,
            tiny_frames: 0,
            backdrop: None,
            sidebar_backdrop: None,
            show_sidebar: true,
            focus_search: false,
            drifting: true,
            drift_speed: 1.0,
            theme: Theme::Light,
            applied_theme: None,
            edge_alpha: 100,
            node_scale: 1.0,
        };
        // 这里不能 rebuild：字体还没就绪，量不了单词的宽度
        app
    }

    // -------------------------------------------------------- 显示 / 隐藏

    /// 窗口内的 Ctrl+9，以及点 X 时收进后台而不是退出。
    ///
    /// 呼出是热键线程直接做的（窗口藏着时主线程根本不会被调用），
    /// 这里只管「藏起来」这半边。
    fn handle_visibility(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;

        // 刚被热键唤醒，清账即可
        self.hotkey_signal.store(false, Ordering::SeqCst);

        // 窗口有焦点时也认 Ctrl+9，全局热键没注册上时这就是唯一的入口
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Num9)) {
            hotkey::hide();
            return;
        }

        // 点 X：收起来，别真的退出
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_exit {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hotkey::hide();
        }
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

        let extents = ids.iter().map(|&g| self.extent_of(g, &local)).collect();
        self.sim = Sim::new(ids, edges, extents);
        self.auto_fit = true;
        if let Some(sel) = self.selected {
            if !self.sim.local.contains_key(&sel) {
                self.selected = None;
            }
        }
    }

    /// 一个节点的字号（世界单位）。连接数越多的词越大 ——
    /// 「节点大小」这个视觉属性只表达连接数，不表达别的。
    fn node_font(&self, degree: usize) -> f32 {
        let f = LABEL_FONT * (0.92 + 0.17 * (degree as f32).sqrt());
        f.clamp(LABEL_FONT * 0.92, LABEL_FONT * 2.0) * self.node_scale
    }

    /// 量出一个节点占多大地方（世界单位）。
    ///
    /// 节点就是那颗写着单词的胶囊本身，所以包围盒 = 文字尺寸 + 内边距，
    /// 碰撞体和看到的形状完全一致，字母永远不会压到别人身上。
    fn extent_of(&self, global: u32, local: &HashMap<u32, usize>) -> layout::Extent {
        let node = &self.graph.nodes[global as usize];
        // 度数要按当前视图里的邻居算，和画出来的大小保持一致
        let deg = node
            .neighbors
            .iter()
            .filter(|nb| local.contains_key(nb))
            .count();
        let font = self.node_font(deg);
        let size = self.ctx.fonts(|f| {
            f.layout_no_wrap(
                node.name.clone(),
                egui::FontId::proportional(font),
                Color32::WHITE,
            )
            .size()
        });
        let half = Vec2::new(size.x * 0.5 + font * 0.62, size.y * 0.5 + font * 0.34);
        layout::Extent { half, off_y: 0.0 }
    }

    fn reload(&mut self) {
        let root = PathBuf::from(self.root_input.trim());
        let old_range = (self.week_lo, self.week_hi);
        let old_weeks = self.graph.weeks.clone();
        let focus_name = self
            .focus
            .and_then(|id| self.graph.nodes.get(id as usize))
            .map(|node| node.name.clone());
        let selected_name = self
            .selected
            .and_then(|id| self.graph.nodes.get(id as usize))
            .map(|node| node.name.clone());

        match Graph::load(&root) {
            Ok(g) => {
                self.graph = g;
                self.load_error = None;
                (self.week_lo, self.week_hi) = remap_week_range(
                    &old_weeks,
                    old_range.0,
                    old_range.1,
                    &self.graph.weeks,
                );
                self.focus = focus_name.and_then(|name| self.graph.find(&name));
                self.selected = selected_name.and_then(|name| self.graph.find(&name));
                self.hovered = None;
                self.dragging = None;
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

    /// 平滑跟拍，保证整张图始终在视野里。
    ///
    /// 不能「收敛后就撒手」—— 气泡漂浮会继续把图撑大一点，撒手之后边上的词
    /// 就飘到画外去了。所以一直跟，但加一个死区：差得不多就不动，
    /// 免得镜头跟着气泡一起晃。用户自己缩放/平移之后就交还控制权。
    fn follow_layout(&mut self, viewport: Rect) {
        let (cam, zoom) = self.fit_target(viewport);
        let off = (cam - self.cam).length();
        let ratio = (zoom / self.zoom.max(1e-6) - 1.0).abs();
        if off < 8.0 && ratio < 0.02 {
            return;
        }
        let t = if self.sim.is_settled() { 0.06 } else { 0.12 };
        self.cam += (cam - self.cam) * t;
        self.zoom += (zoom - self.zoom) * t;
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

    /// 节点的不透明度：越接近所选时间范围的末端越实，越旧越淡。
    /// 「透明度」这个视觉属性只表达时间远近。
    fn recency_alpha(&self, global: u32) -> f32 {
        let node = &self.graph.nodes[global as usize];
        let Some(last) = node.last_week() else {
            return 0.5;
        };
        let (lo, hi) = (self.week_lo as f32, self.week_hi as f32);
        if (last as f32) > hi {
            return 1.0;
        }
        if (last as f32) < lo {
            // 靠展开跳数带进来的邻居，本身不在时间范围内
            return 0.5;
        }
        let span = (hi - lo).max(1.0);
        0.6 + 0.4 * ((last as f32 - lo) / span)
    }
}

impl eframe::App for App {
    /// 窗口是透明的，圆角之外的地方要真的透出去，所以清屏色必须全透明。
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 藏起来的时候什么都不画。winit 即使在窗口隐藏时也会继续空转，
        // 光是提前 return 仍会吃掉大半个核，所以这里主动睡一下把它按住 ——
        // 隐藏期间没有任何事件要响应，热键线程是直接调 Win32 显示窗口的，
        // 不依赖这个循环。
        if !hotkey::is_visible() {
            std::thread::sleep(std::time::Duration::from_millis(120));
            return;
        }
        self.handle_visibility(ctx);

        if self.applied_theme != Some(self.theme) {
            apply_theme(ctx, self.theme);
            self.applied_theme = Some(self.theme);
        }
        let pal = Palette::of(self.theme);

        self.place_window_once(ctx);
        self.snap_after_monitor_switch();

        if self.pending_rebuild {
            self.pending_rebuild = false;
            self.rebuild();
            self.backdrop = load_texture(ctx, "backdrop", BKG_BYTES);
            self.sidebar_backdrop = load_texture(ctx, "sidebar", TAB_BKG_BYTES);
        }

        // Ctrl+B 收放左侧面板
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::B)) {
            self.show_sidebar = !self.show_sidebar;
        }
        // Ctrl+F 跳到搜索框。侧栏要是收着的，先展开，否则搜索框根本没被画出来，
        // 也就无处可聚焦
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::F)) {
            self.show_sidebar = true;
            self.focus_search = true;
        }

        // F5 重新读取笔记库并重建节点/连线。笔记文件改动后用这个刷新视图。
        if ctx.input(|i| i.key_pressed(egui::Key::F5)) {
            self.reload();
        }

        // Ctrl+N 把窗口重新摆回「当前所在这块屏幕的顶端居中」。
        // 多屏之间来回拖之后位置乱了，用这个一键归位。
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::N)) {
            hotkey::snap_top_center();
        }

        // F11 全屏。放在这里而不是画布里，是因为焦点在卡片上时画布收不到按键
        if ctx.input(|i| i.key_pressed(egui::Key::F11)) {
            let full = self.is_fullscreen(ctx);
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!full));
        }

        // Esc：正在打字就先退出输入框，否则关掉右侧详情面板。
        // 放在这里而不是画布里，是因为焦点在侧栏时画布收不到按键。
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            match ctx.memory(|m| m.focused()) {
                Some(id) => ctx.memory_mut(|m| m.surrender_focus(id)),
                None => self.selected = None,
            }
        }

        // 圆角半径要换算成物理像素，高 DPI 屏上才不会偏小
        let radius = self.effective_radius(ctx);
        hotkey::apply_window_shape((radius * ctx.pixels_per_point()).round() as i32);
        self.paint_window_frame(ctx, &pal);
        self.handle_resize(ctx);

        // 画布铺满整个圆；控制面板浮在圆里面，不再是从上到下的一条
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| self.graph_view(ui, &pal));
        if self.show_sidebar {
            self.side_panel(ctx, &pal);
        }
        self.detail_panel(ctx, &pal);

        if !self.sim.is_settled()
            || self.auto_fit
            || self.drifting
            || self.monitor_snap_frames > 0
        {
            // 排一个定时重绘而不是「立刻」，否则会以显卡能跑多快就跑多快的
            // 速度空转。气泡本来就飘得慢，30fps 完全够看，而且省一半电。
            ctx.request_repaint_after(std::time::Duration::from_millis(33));
        }
    }
}

impl App {
    // -------------------------------------------------------- 自绘窗口外框

    fn is_maximized(&self, ctx: &egui::Context) -> bool {
        ctx.input(|i| i.viewport().maximized.unwrap_or(false))
    }

    /// 把窗口摆到「当前所在这块屏幕的顶端、水平居中」。启动时跑一次，
    /// 之后可以用 Ctrl+N 手动再来一次。
    ///
    /// 前几帧窗口可能还没建出来，所以是重试若干帧；成功或次数用完就收手，
    /// 不能一直摆，否则用户自己拖走的窗口会被拽回来。
    fn place_window_once(&mut self, _ctx: &egui::Context) {
        if self.place_window == 0 {
            return;
        }
        self.place_window -= 1;
        // 开头十几帧窗口的尺寸还没定下来，这时候摆会按错误的宽度算居中，
        // 结果就是贴在屏幕左边。等它稳定了再摆，并且过一会儿再补一次，
        // 防止第一次仍然赶在 DPI 适配之前。
        if matches!(self.place_window, 45 | 18) {
            hotkey::snap_top_center();
        }
        if self.place_window < 18 {
            self.place_window = 0;
        }
    }

    /// Win+Shift+方向键由系统直接搬窗口，应用收不到普通按键事件。
    /// 所以这里看窗口所在显示器是否变化；变化后等系统搬完，再把它吸到目标屏顶部居中。
    fn snap_after_monitor_switch(&mut self) {
        if self.monitor_snap_frames > 0 {
            self.monitor_snap_frames -= 1;

            // 先让 Windows 完成跨屏移动；随后多补两次，避开不同 DPI 屏幕间的过渡尺寸。
            if matches!(self.monitor_snap_frames, 8 | 4 | 0) {
                if hotkey::snap_top_center_on(self.monitor_snap_target) {
                    self.last_monitor = self
                        .monitor_snap_target
                        .or_else(|| hotkey::current_monitor_id());
                    if self.monitor_snap_frames == 0 {
                        self.monitor_snap_target = None;
                    }
                }
            }
            return;
        }

        let current = hotkey::current_monitor_id_away_from(self.last_monitor);
        if current != self.last_monitor {
            if self.last_monitor.is_some() && current.is_some() {
                self.monitor_snap_target = current;
                self.monitor_snap_frames = MONITOR_SWITCH_SNAP_FRAMES;
            }
            self.last_monitor = current;
        }
    }

    fn is_fullscreen(&self, ctx: &egui::Context) -> bool {
        ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
    }

    /// 当前该用多大的圆角。铺满屏幕时必须是 0 —— 圆角会把屏幕四角抠掉。
    fn effective_radius(&self, ctx: &egui::Context) -> f32 {
        if self.is_fullscreen(ctx) || self.is_maximized(ctx) {
            0.0
        } else {
            WINDOW_RADIUS
        }
    }

    /// 整个窗口是一块圆角长方形。背景图铺满它，边上描一圈细线。
    ///
    /// 系统层面的裁剪由 `hotkey::apply_window_shape` 用 SetWindowRgn 完成，
    /// 这里只负责画；两者形状必须一致，否则会露边或者被切掉。
    fn paint_window_frame(&self, ctx: &egui::Context, pal: &Palette) {
        let painter = ctx.layer_painter(egui::LayerId::background());
        let screen = ctx.screen_rect();
        let pts = round_rect_points(screen.shrink(0.5), [self.effective_radius(ctx); 4]);

        painter.add(Shape::convex_polygon(pts.clone(), pal.panel, Stroke::NONE));
        if let Some(tex) = &self.backdrop {
            painter.add(textured_fan(screen, &pts, tex.id(), pal.bg_tint));
            if pal.bg_veil.a() > 0 {
                painter.add(Shape::convex_polygon(
                    pts.clone(),
                    pal.bg_veil,
                    Stroke::NONE,
                ));
            }
        }
        painter.add(Shape::closed_line(pts, Stroke::new(1.2, pal.window_border)));
    }

    /// 窗口里能放东西的区域。圆角长方形不像正圆那样浪费，
    /// 只要躲开四个圆角就行。
    fn inner_square(&self, ctx: &egui::Context) -> Rect {
        // 全屏时圆角是 0，但仍留一点边距，内容别顶到屏幕边上
        ctx.screen_rect()
            .shrink((self.effective_radius(ctx) * 0.42).max(12.0))
    }

    fn card_width(&self, ctx: &egui::Context) -> f32 {
        268.0_f32.min(self.inner_square(ctx).width() * 0.34)
    }

    /// 真正留给词图的地方：内接正方形再去掉浮在上面的卡片。
    /// 不减掉的话图会被卡片压住一大半。
    fn content_rect(&self, ctx: &egui::Context) -> Rect {
        let mut r = self.inner_square(ctx);
        if self.show_sidebar {
            r.min.x += self.card_width(ctx) + 14.0;
        }
        if self.selected.is_some() {
            r.max.x -= 230.0_f32.min(r.width() * 0.42) + 14.0;
        }
        if r.width() < 120.0 {
            // 卡片全开时窗口太小，就别再让了，宁可压一点
            r = self.inner_square(ctx);
        }
        r
    }

    /// 没有系统边框了，改变窗口大小得自己来：贴着窗口边一圈留出几个像素，
    /// 在那儿按下就交给系统去拖。
    fn handle_resize(&mut self, ctx: &egui::Context) {
        use egui::viewport::ResizeDirection;
        if self.is_maximized(ctx) || self.is_fullscreen(ctx) {
            return;
        }
        let screen = ctx.screen_rect();

        // 兜底：窗口一旦被拖成没法操作的大小，直接拉回默认尺寸。
        // 无边框窗口在某些情况下不受 min_inner_size 约束，缩成几个像素之后
        // 连边都抓不住，只能自己救自己。
        //
        // 但必须**连续**观测到才算数：窗口在不同 DPI 的显示器之间移动时，
        // 中间那几帧 screen_rect 会读到过渡态的异常值，一帧就动手的话
        // 会在跨屏时把窗口莫名其妙地重置掉。
        if screen.width() < MIN_WINDOW.x || screen.height() < MIN_WINDOW.y {
            self.tiny_frames += 1;
            if self.tiny_frames > 30 {
                self.tiny_frames = 0;
                // 用物理像素直接找系统要，不走 eframe —— 窗口塌下去之后
                // ViewportCommand::InnerSize 实测是不生效的
                let ppp = ctx.pixels_per_point();
                hotkey::restore_size(
                    (DEFAULT_WINDOW.x * ppp) as i32,
                    (DEFAULT_WINDOW.y * ppp) as i32,
                );
            }
            return;
        }
        self.tiny_frames = 0;

        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        if !screen.contains(p) {
            return;
        }

        let left = p.x <= screen.left() + RESIZE_EDGE;
        let right = p.x >= screen.right() - RESIZE_EDGE;
        let top = p.y <= screen.top() + RESIZE_EDGE;
        let bottom = p.y >= screen.bottom() - RESIZE_EDGE;

        let (dir, cursor) = match (left, right, top, bottom) {
            (true, _, true, _) => (
                Some(ResizeDirection::NorthWest),
                egui::CursorIcon::ResizeNwSe,
            ),
            (_, true, true, _) => (
                Some(ResizeDirection::NorthEast),
                egui::CursorIcon::ResizeNeSw,
            ),
            (true, _, _, true) => (
                Some(ResizeDirection::SouthWest),
                egui::CursorIcon::ResizeNeSw,
            ),
            (_, true, _, true) => (
                Some(ResizeDirection::SouthEast),
                egui::CursorIcon::ResizeNwSe,
            ),
            (true, ..) => (Some(ResizeDirection::West), egui::CursorIcon::ResizeHorizontal),
            (_, true, ..) => (
                Some(ResizeDirection::East),
                egui::CursorIcon::ResizeHorizontal,
            ),
            (_, _, true, _) => (Some(ResizeDirection::North), egui::CursorIcon::ResizeVertical),
            (_, _, _, true) => (Some(ResizeDirection::South), egui::CursorIcon::ResizeVertical),
            _ => (None, egui::CursorIcon::Default),
        };

        if let Some(dir) = dir {
            ctx.set_cursor_icon(cursor);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        }
    }

}

impl App {
    // ------------------------------------------------------------ 左侧面板

    /// 控制面板。圆形窗口里放不下从上到下的一条竖栏，改成浮在圆内左侧的卡片。
    fn side_panel(&mut self, ctx: &egui::Context, pal: &Palette) {
        let inner = self.inner_square(ctx);
        let card = Rect::from_min_size(
            inner.left_top(),
            egui::vec2(self.card_width(ctx), inner.height()),
        );

        egui::Area::new(egui::Id::new("controls"))
            .fixed_pos(card.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(card.width());
                ui.set_max_height(card.height());
                egui::Frame::NONE
                    .fill(pal.card)
                    .stroke(Stroke::new(1.0, pal.window_border))
                    .corner_radius(16)
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(card.width() - 28.0);
                        // 卡片底纹用 tab_bkg。先占一个空位，等内容画完知道了
                        // 真实高度再回填 —— 直接用 max_rect 的话底纹会一路
                        // 拖到窗口底部，而卡片本身是贴着内容收的。
                        let bg_slot = ui.painter().add(Shape::Noop);

                        // 没有标题栏了，窗口靠拖这张卡片的空白处来移动
                        let drag = ui.interact(
                            ui.max_rect(),
                            ui.id().with("window-drag"),
                            Sense::click_and_drag(),
                        );
                        if drag.drag_started() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                        }

                        ui.label(
                            RichText::new("辞境")
                                .size(21.0)
                                .strong()
                                .color(pal.text_strong),
                        );
                        ui.label(
                            RichText::new("LEXIS · Word Atlas")
                                .size(10.0)
                                .color(pal.text_weak),
                        );
                        ui.add_space(12.0);

                        if let Some(err) = self.load_error.clone() {
                            ui.colored_label(Color32::from_rgb(0xDF, 0x6B, 0x63), err);
                            ui.add_space(6.0);
                        }

                        egui::ScrollArea::vertical()
                            .max_height(card.height() - 150.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                self.time_controls(ui);
                                ui.add_space(12.0);
                                self.scope_controls(ui);
                                ui.add_space(12.0);
                                self.search_controls(ui);
                                ui.add_space(12.0);
                                ui.horizontal(|ui| {
                                    if ui.button("重置视图").clicked() {
                                        self.auto_fit = true;
                                        self.selected = None;
                                    }
                                    ui.selectable_value(&mut self.theme, Theme::Light, "亮");
                                    ui.selectable_value(&mut self.theme, Theme::Dark, "暗");
                                });
                                ui.add_space(10.0);
                                self.advanced_controls(ui);
                            });

                        ui.add_space(8.0);
                        if self.hotkey_registered {
                            ui.weak("Ctrl+9 收进后台 · Ctrl+B 收起本卡");
                        } else {
                            ui.weak("Ctrl+9 被占用，只在窗口内生效");
                        }
                        ui.weak("Ctrl+F 搜索 · F5 刷新词库 · Esc 关详情");
                        ui.weak("F 复位视野 · F11 全屏 · 方向键平移");
                        ui.weak("+/− 缩放 · 按住 Shift 加速");
                        ui.weak("Ctrl+N 窗口归位（多屏拖乱了用它）");
                        ui.weak("拖本卡空白处可移动窗口");

                        if let Some(tex) = &self.sidebar_backdrop {
                            ui.painter().set(
                                bg_slot,
                                textured_round_rect(
                                    ui.min_rect().expand(13.0),
                                    [16.0; 4],
                                    tex.id(),
                                    pal.bg_tint.gamma_multiply(0.9),
                                ),
                            );
                        }
                    });
            });
    }

    fn time_controls(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("时间范围").strong());
        ui.add_space(4.0);
        let n_weeks = self.graph.weeks.len();
        if n_weeks == 0 {
            ui.weak("没有读到任何周标签");
            return;
        }
        let labels: Vec<String> = self.graph.weeks.iter().map(|w| w.label()).collect();
        let (l1, l2) = (labels.clone(), labels.clone());
        let max = n_weeks - 1;

        let (lo_text, hi_text) = (
            labels[self.week_lo.min(max)].clone(),
            labels[self.week_hi.min(max)].clone(),
        );

        let mut changed = false;
        changed |= ui
            .add(
                egui::Slider::new(&mut self.week_lo, 0..=max)
                    .show_value(false)
                    .text(lo_text)
                    .custom_formatter(move |v, _| l1.get(v as usize).cloned().unwrap_or_default()),
            )
            .changed();
        changed |= ui
            .add(
                egui::Slider::new(&mut self.week_hi, 0..=max)
                    .show_value(false)
                    .text(hi_text)
                    .custom_formatter(move |v, _| l2.get(v as usize).cloned().unwrap_or_default()),
            )
            .changed();
        if changed && self.week_lo > self.week_hi {
            self.week_hi = self.week_lo;
        }

        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            for (text, span) in [("本周", 1usize), ("4 周", 4), ("12 周", 12)] {
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

    fn scope_controls(&mut self, ui: &mut egui::Ui) {
        let mut structural = false;
        structural |= ui
            .add(egui::Slider::new(&mut self.hops, 0..=3).text("展开层数"))
            .changed();

        // 界面上说「显示」，内部存的是「隐藏」，这里翻一下
        let mut show_isolated = !self.hide_isolated;
        if ui.checkbox(&mut show_isolated, "显示孤立单词").changed() {
            self.hide_isolated = !show_isolated;
            structural = true;
        }
        if structural {
            self.rebuild();
        }

        if let Some(f) = self.focus {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("聚焦 {}", self.graph.nodes[f as usize].name)).italics(),
                );
                if ui.small_button("×").clicked() {
                    self.focus = None;
                    self.rebuild();
                }
            });
        }
    }

    fn search_controls(&mut self, ui: &mut egui::Ui) {
        let resp = ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("搜索单词，回车定位  (Ctrl+F)")
                .desired_width(f32::INFINITY),
        );
        if self.focus_search {
            self.focus_search = false;
            resp.request_focus();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.jump_to_search();
        }
    }

    /// 调参用的开关全塞进这里，默认收起 —— 主界面只留背单词真正会用到的东西。
    fn advanced_controls(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("高级显示设置")
            .default_open(false)
            .show(ui, |ui| {
                ui.checkbox(&mut self.drifting, "气泡漂浮");
                if self.drifting {
                    ui.add(egui::Slider::new(&mut self.drift_speed, 0.1..=2.5).text("漂浮速度"));
                }
                if ui
                    .add(egui::Slider::new(&mut self.node_scale, 0.5..=2.0).text("单词大小"))
                    .changed()
                {
                    // 大小变了，碰撞盒也得跟着变，只能重排
                    self.rebuild();
                }
                ui.add(egui::Slider::new(&mut self.edge_alpha, 20..=260).text("连线浓度"));

                ui.add_space(6.0);
                ui.label(RichText::new("数据源").strong());
                ui.add(
                    egui::TextEdit::singleline(&mut self.root_input)
                        .desired_width(f32::INFINITY),
                );
                if ui.button("重新加载（F5）").clicked() {
                    self.reload();
                }

                ui.add_space(6.0);
                ui.weak(format!(
                    "显示 {} 个词 / {} 条联系 / {} 个词团",
                    self.sim.len(),
                    self.sim.edges.len(),
                    self.sim.component_count()
                ));
                let hidden = self.isolated_count();
                if hidden > 0 {
                    ui.weak(format!("{hidden} 个没有联系的词被隐藏"));
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

                ui.add_space(6.0);
                if ui.button("窗口归位（顶端居中）").clicked() {
                    hotkey::snap_top_center();
                }

                ui.add_space(6.0);
                if ui.button("退出程序").clicked() {
                    self.allow_exit = true;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
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

    /// 选中某个词后弹出的详情卡片，浮在圆内右侧。
    fn detail_panel(&mut self, ctx: &egui::Context, pal: &Palette) {
        let Some(sel) = self.selected else { return };
        let node_name = self.graph.nodes[sel as usize].name.clone();
        let mut goto: Option<u32> = None;
        let mut focus_it = false;
        let mut close = false;

        let inner = self.inner_square(ctx);
        let w = 230.0_f32.min(inner.width() * 0.42);
        let card = Rect::from_min_size(
            Pos2::new(inner.right() - w, inner.top()),
            egui::vec2(w, inner.height()),
        );

        egui::Area::new(egui::Id::new("detail"))
            .fixed_pos(card.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.set_max_width(card.width());
                egui::Frame::NONE
                    .fill(pal.card)
                    .stroke(Stroke::new(1.0, pal.window_border))
                    .corner_radius(16)
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(card.width() - 28.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(&node_name)
                                    .size(17.0)
                                    .strong()
                                    .color(pal.text_strong),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("×").clicked() {
                                        close = true;
                                    }
                                },
                            );
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
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("以此为中心").clicked() {
                                focus_it = true;
                            }
                            if ui.button("打开 md").clicked() {
                                open_path(&node.path);
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new(format!("联系 ({})", node.degree())).strong());
                        egui::ScrollArea::vertical()
                            .max_height(card.height() - 150.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
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
            });

        if close {
            self.selected = None;
        }
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

    fn graph_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = resp.rect;
        // 图以「没被卡片挡住的那块」为中心，卡片开合时才不会被压住
        let content = self.content_rect(ui.ctx());
        let center = content.center();

        // 底图已经由 paint_window_frame 铺在整个圆上了，这里不用再画一层

        // ---- 物理迭代 ----
        if !self.sim.is_settled() {
            // 还在铺开：跑退火布局，每帧多走两步收敛快一点
            for _ in 0..2 {
                self.sim.step();
            }
        } else if self.drifting {
            // 已经定型：转成气泡漂浮。用真实帧间隔，掉帧时也不会变慢
            self.sim.drift = self.drift_speed;
            let dt = ui.input(|i| i.stable_dt).clamp(1.0 / 120.0, 1.0 / 20.0);
            self.sim.drift_step(dt);
        }

        // 力导向会把图铺得比初始撒点大好几倍，所以在收敛过程中一直跟拍，
        // 否则第一帧对好的视野很快就被甩在外面，画面看上去就是一片空白。
        if self.auto_fit {
            self.follow_layout(content);
        }

        // ---- 键盘平移 / 缩放 ----
        // 只有真的在输入框里打字时才让出方向键 —— 否则光标左右移动会变成平移。
        // 注意不能用 `memory().focused()`：随便点过一个按钮或滑块之后它就一直
        // 是 Some，方向键和缩放会被整个屏蔽掉。`wants_keyboard_input()` 才是
        // 「当前有文本框在收键盘」的意思。
        let typing = ui.ctx().wants_keyboard_input();
        if !typing {
            // 长按取 key_down（连续），单击取 key_pressed（离散一步）。
            // 只用 key_down 的话，快速点一下有可能整个落在两帧之间，
            // 采样时已经抬起来了 —— 表现就是「按了没反应」。
            let (dir, step, zoom_hold, zoom_step, fast, dt) = ui.input(|i| {
                let axis = |neg, pos| {
                    (i.key_down(pos) as i32 - i.key_down(neg) as i32) as f32
                };
                let axis_step = |neg, pos| {
                    (i.key_pressed(pos) as i32 - i.key_pressed(neg) as i32) as f32
                };
                use egui::Key::*;
                // 拉近：= 和 +（同一个键，按不按 Shift 都认）
                let zin_down = i.key_down(Equals) || i.key_down(Plus);
                let zin_step = i.key_pressed(Equals) || i.key_pressed(Plus);
                (
                    Vec2::new(axis(ArrowLeft, ArrowRight), axis(ArrowUp, ArrowDown)),
                    Vec2::new(
                        axis_step(ArrowLeft, ArrowRight),
                        axis_step(ArrowUp, ArrowDown),
                    ),
                    zin_down as i32 as f32 - i.key_down(Minus) as i32 as f32,
                    zin_step as i32 as f32 - i.key_pressed(Minus) as i32 as f32,
                    i.modifiers.shift,
                    i.stable_dt.clamp(1.0 / 120.0, 1.0 / 20.0),
                )
            });

            // 按屏幕速度算再换算回世界坐标，缩放到多大手感都一样
            let speed = if fast { 1600.0 } else { 700.0 };
            let mut moved = false;
            if dir != Vec2::ZERO {
                self.cam += dir.normalized() * (speed * dt / self.zoom);
                moved = true;
            }
            if step != Vec2::ZERO {
                self.cam += step.normalized() * (60.0 / self.zoom);
                moved = true;
            }
            // 指数缩放：每一步都是「乘」而不是「加」，
            // 不然放到很大之后再按一下几乎看不出变化
            let rate = if fast { 2.6 } else { 1.4 };
            let mut factor = 0.0;
            if zoom_hold != 0.0 {
                factor += zoom_hold * rate * dt;
            }
            if zoom_step != 0.0 {
                factor += zoom_step * 0.15;
            }
            if factor != 0.0 {
                self.zoom = (self.zoom * factor.exp()).clamp(0.02, 20.0);
                moved = true;
            }
            if moved {
                self.auto_fit = false;
                ui.ctx().request_repaint();
            }
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

        // ---- 命中测试：节点是矩形胶囊，按包围盒判 ----
        let mut hover: Option<usize> = None;
        if let Some(p) = pointer {
            if rect.contains(p) {
                let world = self.to_world(center, p);
                for i in 0..self.sim.len() {
                    let e = self.sim.extent(i);
                    let c = self.sim.pos[i] + Vec2::new(0.0, e.off_y);
                    let d = world - c;
                    if d.x.abs() <= e.half.x && d.y.abs() <= e.half.y {
                        hover = Some(i);
                        break;
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

        // 快捷键。带修饰键的组合和 Esc 都归上层处理，这里只认光秃秃的 F
        let fit = ui.input(|i| !i.modifiers.any() && i.key_pressed(egui::Key::F));
        if fit {
            self.auto_fit = true;
        }

        if self.sim.len() == 0 {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "这个时间范围内没有单词，试试把时间范围拉宽一些",
                FontId::proportional(15.0),
                pal.text_weak,
            );
            return;
        }

        // ---- 四种状态：选中 > 悬停 > 邻居 > 无关 ----
        // 选中优先于悬停，这样点定一个词之后，鼠标扫过别处也不会打断阅读
        let focus_node = self.selected.or(self.hovered);
        let focus_local = focus_node.and_then(|g| self.sim.local.get(&g).copied());
        let mut related: HashSet<usize> = HashSet::new();
        if let Some(h) = focus_local {
            related.insert(h);
            for &nb in &self.graph.nodes[self.sim.ids[h] as usize].neighbors {
                if let Some(&l) = self.sim.local.get(&nb) {
                    related.insert(l);
                }
            }
        }
        let emphasis = focus_local.is_some();
        let query = self.search.trim().to_lowercase();

        // ---- 画边（先画，节点会盖住端点，看起来就是连到胶囊边上）----
        let mut shapes: Vec<Shape> = Vec::with_capacity(self.sim.edges.len());
        let base_edge = pal.edge_base.gamma_multiply(self.edge_alpha as f32 / 100.0);
        for &(a, b) in &self.sim.edges {
            let (a, b) = (a as usize, b as usize);
            let pa = self.to_screen(center, self.sim.pos[a]);
            let pb = self.to_screen(center, self.sim.pos[b]);
            if !segment_visible(rect, pa, pb) {
                continue;
            }
            let hot = focus_local.is_some_and(|h| h == a || h == b);
            let (color, width) = if hot {
                (pal.accent, 2.0)
            } else if emphasis {
                (pal.edge_mute, 1.0)
            } else {
                (base_edge, 1.5)
            };

            // 轻微的二次贝塞尔：控制点往垂直方向偏一点点，比直线自然，
            // 幅度只有长度的 8%，不至于弯得夸张
            let mid = pa + (pb - pa) * 0.5;
            let perp = Vec2::new(-(pb.y - pa.y), pb.x - pa.x).normalized();
            let ctrl = mid + perp * ((pb - pa).length() * 0.08);
            shapes.push(Shape::QuadraticBezier(
                egui::epaint::QuadraticBezierShape::from_points_stroke(
                    [pa, ctrl, pb],
                    false,
                    Color32::TRANSPARENT,
                    Stroke::new(width, color),
                ),
            ));
        }
        painter.extend(shapes);

        // ---- 画节点：单词本身就是节点 ----
        let mut shapes: Vec<Shape> = Vec::with_capacity(self.sim.len() * 3);
        let mut texts: Vec<(Pos2, String, f32, Color32)> = Vec::new();
        let cull = rect.expand(80.0);
        let mut visible_count = 0usize;

        for i in 0..self.sim.len() {
            let sp = self.to_screen(center, self.sim.pos[i]);
            let e = self.sim.extent(i);
            let size = e.half * 2.0 * self.zoom;
            let chip = Rect::from_center_size(sp + Vec2::new(0.0, e.off_y * self.zoom), size);
            if !cull.intersects(chip) {
                continue;
            }
            visible_count += 1;

            let gid = self.sim.ids[i];
            let node = &self.graph.nodes[gid as usize];
            let selected = Some(gid) == self.selected;
            let hovered = Some(gid) == self.hovered;
            let is_related = related.contains(&i);

            // 透明度 = 时间远近；无关节点再整体压暗
            let mut alpha = self.recency_alpha(gid);
            if emphasis && !is_related {
                alpha *= 1.0 - pal.dim;
            }

            let hue = pal.cluster(node.component);
            let top = mix(hue, pal.chip_base, pal.chip_top_mix);
            let bottom = mix(hue, pal.chip_base, pal.chip_bottom_mix);
            let border = mix(hue, pal.chip_base, pal.chip_border_mix);

            // 选中的外发光：几层向外扩散的圆角矩形
            if selected {
                for k in 1..=3 {
                    let grow = 3.0 * k as f32;
                    shapes.push(Shape::Path(egui::epaint::PathShape::convex_polygon(
                        pill_points(chip.expand(grow), chip.height() * 0.5 + grow),
                        hue.gamma_multiply(0.13 / k as f32),
                        Stroke::NONE,
                    )));
                }
            }

            gradient_pill(
                &mut shapes,
                chip,
                top.gamma_multiply(alpha),
                bottom.gamma_multiply(alpha),
            );

            let (stroke_c, stroke_w) = if selected {
                (mix(hue, pal.text_strong, 0.25), 2.2)
            } else if hovered {
                (border, 1.8)
            } else if !query.is_empty() && node.name.to_lowercase().contains(&query) {
                (pal.accent, 2.0)
            } else {
                (border.gamma_multiply(alpha), 1.0)
            };
            shapes.push(Shape::closed_line(
                pill_points(chip, chip.height() * 0.5),
                Stroke::new(stroke_w, stroke_c),
            ));

            // 字太小就不画了，但走的是渐隐而不是突然消失 —— 之前节点闪烁就是
            // 因为按「装得下就画」的硬阈值取舍，位置一动结论就翻转
            let font_px = self.node_font(self.sim.degree[i] as usize) * self.zoom;
            let text_fade = ((font_px - 4.5) / 3.5).clamp(0.0, 1.0);
            if text_fade > 0.01 {
                texts.push((
                    chip.center(),
                    node.name.clone(),
                    font_px,
                    pal.text_strong.gamma_multiply(alpha * text_fade),
                ));
            }
        }
        painter.extend(shapes);

        for (pos, text, font, color) in texts {
            painter.text(
                pos,
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(font),
                color,
            );
        }

        // ---- 悬停提示 ----
        if let Some(h) = self.hovered.and_then(|g| self.sim.local.get(&g).copied()) {
            let node = &self.graph.nodes[self.sim.ids[h] as usize];
            let weeks = node
                .weeks
                .iter()
                .filter_map(|&w| self.graph.weeks.get(w as usize))
                .map(|w| w.label())
                .collect::<Vec<_>>()
                .join(" ");
            let tip = format!("{} 条联系\n{}", node.degree(), weeks);
            let e = self.sim.extent(h);
            let anchor = self.to_screen(center, self.sim.pos[h])
                + Vec2::new(e.half.x * self.zoom + 10.0, 8.0);
            let galley = painter.layout_no_wrap(tip, FontId::proportional(12.0), pal.text_weak);
            let bg = Rect::from_min_size(anchor, galley.size()).expand(7.0);
            painter.rect(
                bg,
                6.0,
                pal.tip_bg,
                Stroke::new(1.0, pal.tip_border),
                StrokeKind::Inside,
            );
            painter.galley(anchor, galley, pal.text_weak);
        }

        // ---- 左下角状态 ----
        painter.text(
            content.left_bottom() + Vec2::new(4.0, -2.0),
            Align2::LEFT_BOTTOM,
            format!("视野内 {visible_count} / {} 个词", self.sim.len()),
            FontId::proportional(11.0),
            pal.text_weak.gamma_multiply(0.7),
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
/// 在两个颜色之间线性插值，`t=0` 取 a，`t=1` 取 b。
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// 圆角矩形的轮廓点，顺时针。四个角各用几段折线逼近。
fn pill_points(rect: Rect, radius: f32) -> Vec<Pos2> {
    round_rect_points(rect, [radius; 4])
}

/// 四个角半径可以不一样。顺序是 [左上, 右上, 右下, 左下] ——
/// 侧栏贴着窗口左边，只有左边两个角要跟着窗口圆，右边得是直角。
fn round_rect_points(rect: Rect, radii: [f32; 4]) -> Vec<Pos2> {
    let cap = rect.width().min(rect.height()) * 0.5;
    let [tl, tr, br, bl] = radii.map(|r| r.clamp(0.0, cap.max(0.0)));
    const SEG: usize = 6;
    // (圆心, 起始角)：从右下角开始顺时针走
    let corners = [
        (Pos2::new(rect.right() - br, rect.bottom() - br), 0.0f32, br),
        (Pos2::new(rect.left() + bl, rect.bottom() - bl), 90.0, bl),
        (Pos2::new(rect.left() + tl, rect.top() + tl), 180.0, tl),
        (Pos2::new(rect.right() - tr, rect.top() + tr), 270.0, tr),
    ];
    let mut pts = Vec::with_capacity(4 * (SEG + 1));
    for (c, a0, r) in corners {
        for k in 0..=SEG {
            let a = (a0 + 90.0 * k as f32 / SEG as f32).to_radians();
            pts.push(Pos2::new(c.x + r * a.cos(), c.y + r * a.sin()));
        }
    }
    pts
}

/// 画一颗带竖向渐变的胶囊。
///
/// egui 的 `rect_filled` 只能填纯色，所以这里自己拼网格：从中心扇形展开到
/// 圆角轮廓，每个顶点的颜色按它的 y 在上下两色之间插值，就得到了平滑的渐变。
fn gradient_pill(shapes: &mut Vec<Shape>, rect: Rect, top: Color32, bottom: Color32) {
    gradient_pill_radius(shapes, rect, rect.height() * 0.5, top, bottom);
}

fn gradient_pill_radius(
    shapes: &mut Vec<Shape>,
    rect: Rect,
    radius: f32,
    top: Color32,
    bottom: Color32,
) {
    let pts = pill_points(rect, radius);
    if pts.is_empty() {
        return;
    }
    let height = rect.height().max(0.001);
    let color_at = |y: f32| mix(top, bottom, ((y - rect.top()) / height).clamp(0.0, 1.0));

    let mut mesh = egui::Mesh::default();
    let c = rect.center();
    // 顶点色只带 rgb，alpha 要单独续上，否则渐变会把透明度抹掉
    let alpha = top.a().max(bottom.a());
    let tint = |col: Color32| {
        Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha)
    };
    mesh.colored_vertex(c, tint(color_at(c.y)));
    for p in &pts {
        mesh.colored_vertex(*p, tint(color_at(p.y)));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    shapes.push(Shape::mesh(mesh));
}

/// 把一张纹理画成圆角矩形。
///
/// egui 的 `Painter::image` 只能画方角，而画布是嵌在圆角窗口里的一张内卡片，
/// 所以这里自己拼网格：从中心扇形展开到圆角轮廓，每个顶点的 uv 按它在矩形里
/// 的相对位置算出来。
fn textured_round_rect(
    rect: Rect,
    radii: [f32; 4],
    tex: egui::TextureId,
    tint: Color32,
) -> Shape {
    textured_fan(rect, &round_rect_points(rect, radii), tex, tint)
}

/// 把一张纹理贴到任意凸多边形上：中心扇形展开，uv 按顶点在 `rect` 里的
/// 相对位置算。圆形窗口和圆角卡片都走这条路。
fn textured_fan(rect: Rect, pts: &[Pos2], tex: egui::TextureId, tint: Color32) -> Shape {
    let mut mesh = egui::Mesh::with_texture(tex);
    let uv = |p: Pos2| {
        Pos2::new(
            ((p.x - rect.left()) / rect.width().max(0.001)).clamp(0.0, 1.0),
            ((p.y - rect.top()) / rect.height().max(0.001)).clamp(0.0, 1.0),
        )
    };
    let mut push = |p: Pos2| {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: p,
            uv: uv(p),
            color: tint,
        });
    };
    push(rect.center());
    for p in pts {
        push(*p);
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    Shape::mesh(mesh)
}

/// 背景图直接编进 exe，这样程序拷到哪都能跑，不用带 assets 目录。
/// 换图的话替换 `assets/` 里的文件再重新编译即可。
const BKG_BYTES: &[u8] = include_bytes!("../assets/bkg.png");
const TAB_BKG_BYTES: &[u8] = include_bytes!("../assets/tab_bkg.png");

fn load_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let pixels = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(name, pixels, egui::TextureOptions::LINEAR))
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

#[cfg(test)]
mod tests {
    use super::remap_week_range;
    use crate::vocab::Week;

    fn week(year: u16, week: u8) -> Week {
        Week { year, week }
    }

    #[test]
    fn reload_preserves_selected_week_labels() {
        let old = [week(2026, 2), week(2026, 3), week(2026, 4), week(2026, 5)];
        let new = [
            week(2026, 1),
            week(2026, 2),
            week(2026, 3),
            week(2026, 4),
            week(2026, 5),
            week(2026, 6),
        ];

        assert_eq!(remap_week_range(&old, 1, 2, &new), (2, 3));
    }

    #[test]
    fn reload_uses_nearest_weeks_when_old_boundaries_were_removed() {
        let old = [week(2026, 2), week(2026, 3), week(2026, 4), week(2026, 5)];
        let new = [week(2026, 1), week(2026, 2), week(2026, 5), week(2026, 6)];

        assert_eq!(remap_week_range(&old, 1, 2, &new), (1, 2));
    }

    /// 背景图是 include_bytes! 进来的，编译期就在，这里确认它真能解码 ——
    /// 解码失败会静默退回纯色背景，光看界面分不出是「没解码」还是「太淡」。
    #[test]
    fn backdrop_decodes() {
        const BYTES: &[u8] = include_bytes!("../assets/bkg.png");
        let img = image::load_from_memory(BYTES).expect("背景图解码失败").to_rgba8();
        assert_eq!((img.width(), img.height()), (1672, 941));
    }
}

fn setup_style(ctx: &egui::Context) {
    // egui 自带一套「Ctrl +/-/0 缩放整个界面」的快捷键，而且会把这些事件吃掉。
    // 我们自己有画布缩放和 Ctrl+0 归位，冲突了 —— 关掉它的。
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
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
        // 侧栏只有 272 宽，滑块太长会把后面的文字顶出面板
        s.spacing.slider_width = 132.0;
        s.visuals.window_corner_radius = 6.into();
        // 控件跟着窗口一起圆一点，铺在底纹上才不显得生硬
        for w in [
            &mut s.visuals.widgets.noninteractive,
            &mut s.visuals.widgets.inactive,
            &mut s.visuals.widgets.hovered,
            &mut s.visuals.widgets.active,
            &mut s.visuals.widgets.open,
        ] {
            w.corner_radius = 6.into();
        }
        s.visuals.panel_fill = pal.panel;
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
