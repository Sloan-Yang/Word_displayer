//! 布局。
//!
//! 这个词库里绝大多数「词团」是彼此不相连的小团（两三个词一组），如果把它们
//! 丢进一个全局力导向里，各团互相排斥、再被向心力拉回来，最后会摊成一个中间
//! 空掉的大圆环 —— 既难看又浪费画面。
//!
//! 所以这里分两步：
//!   1. 先把子图切成连通分量，每个分量分配一个圆形槽位，按大小做同心环装箱；
//!   2. 每个分量在自己的槽位里独立跑 Fruchterman-Reingold（斥力用 Barnes-Hut
//!      近似），槽位边缘加一圈软墙，防止它挤到邻居身上。
//!
//! 结果是一片排布整齐的词团，而不是一个空心圆环。

use std::collections::HashMap;

use egui::{Pos2, Vec2};

const THETA: f32 = 0.9; // Barnes-Hut 精度：越小越准越慢
const MAX_DEPTH: u32 = 24;
/// 理想边长，同时也是所有间距的基准单位
const K: f32 = 62.0;

// ---- 气泡漂浮的参数 ----
/// 游走加速度。配合下面的阻尼，终速大约是 ACCEL * dt / (1 - DAMPING)
const WANDER_ACCEL: f32 = 42.0;
/// 每帧速度衰减，越小越黏、越不容易飘远
const DAMPING: f32 = 0.92;
/// 限速，世界单位/秒。K 是 62，所以横穿一条边要七八秒
const MAX_SPEED: f32 = 8.0;
/// 边的弹簧劲度：把节点拉回理想边长
const EDGE_SPRING: f32 = 3.0;
/// 出了槽位之后被推回来的劲度
const WALL_SPRING: f32 = 6.0;
/// 碰撞恢复系数，1 是完全弹性
const RESTITUTION: f32 = 0.55;
/// 气泡之间留一点缝，看起来不至于糊在一起
const COLLIDE_PAD: f32 = 3.0;

struct Component {
    members: Vec<u32>,
    anchor: Vec2,
    radius: f32,
}

pub struct Sim {
    /// 局部下标 -> 全局节点下标
    pub ids: Vec<u32>,
    /// 全局节点下标 -> 局部下标
    pub local: HashMap<u32, usize>,
    pub pos: Vec<Pos2>,
    /// 局部下标表示的边
    pub edges: Vec<(u32, u32)>,
    pub degree: Vec<u32>,
    disp: Vec<Vec2>,
    /// 每个节点属于哪个分量
    comp_of: Vec<u32>,
    comps: Vec<Component>,
    /// 退火温度系数，1 -> 0
    pub alpha: f32,
    /// 被鼠标按住的节点不参与位移
    pub pinned: Option<usize>,

    // ---- 布局收敛之后的气泡漂浮 ----
    vel: Vec<Vec2>,
    /// 每个节点当前的游走方向，以及这个方向自转的角速度
    wander: Vec<f32>,
    wander_rate: Vec<f32>,
    /// 漂浮速度倍率，0 表示完全静止
    pub drift: f32,
    /// 节点显示半径的倍率，碰撞用它算接触距离
    pub node_scale: f32,
}

impl Sim {
    pub fn new(ids: Vec<u32>, edges: Vec<(u32, u32)>) -> Sim {
        let n = ids.len();
        let local: HashMap<u32, usize> = ids.iter().enumerate().map(|(i, &g)| (g, i)).collect();

        let mut degree = vec![0u32; n];
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &(a, b) in &edges {
            degree[a as usize] += 1;
            degree[b as usize] += 1;
            adj[a as usize].push(b);
            adj[b as usize].push(a);
        }

        let (comp_of, mut comps) = split_components(n, &adj);
        pack(&mut comps);

        // 每个分量在自己槽位里用黄金角螺旋撒点，比随机均匀，收敛也快
        let mut pos = vec![Pos2::ZERO; n];
        for c in &comps {
            let m = c.members.len();
            for (i, &node) in c.members.iter().enumerate() {
                let a = i as f32 * 2.399_963;
                let r = c.radius * 0.75 * ((i as f32 + 0.5) / m as f32).sqrt();
                pos[node as usize] = Pos2::ZERO + c.anchor + Vec2::angled(a) * r;
            }
        }

        Sim {
            ids,
            local,
            pos,
            edges,
            degree,
            disp: vec![Vec2::ZERO; n],
            comp_of,
            comps,
            alpha: 1.0,
            pinned: None,
            vel: vec![Vec2::ZERO; n],
            // 相位和转速都散列自下标：各转各的，不会同步成整齐划一的抽动
            wander: (0..n).map(|i| hash01(i as u32) * std::f32::consts::TAU).collect(),
            wander_rate: (0..n)
                .map(|i| 0.12 + hash01(i as u32 ^ 0x5BF0_3635) * 0.30)
                .collect(),
            drift: 1.0,
            node_scale: 1.0,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn component_count(&self) -> usize {
        self.comps.len()
    }

    pub fn is_settled(&self) -> bool {
        self.alpha <= 0.005
    }

    /// 用户拖动/改动后重新加热，让布局继续收敛。
    pub fn reheat(&mut self, amount: f32) {
        self.alpha = self.alpha.max(amount);
    }

    pub fn step(&mut self) {
        let n = self.pos.len();
        if n == 0 {
            return;
        }
        for d in &mut self.disp {
            *d = Vec2::ZERO;
        }

        // --- 斥力：只在分量内部算，分量之间靠装箱隔开 ---
        let k2 = K * K;
        for c in &self.comps {
            if c.members.len() < 2 {
                continue;
            }
            let tree = QuadTree::build(&self.pos, &c.members);
            for &i in &c.members {
                self.disp[i as usize] += tree.repulsion(self.pos[i as usize], k2);
            }
        }

        // --- 边的引力 ---
        for &(a, b) in &self.edges {
            let (a, b) = (a as usize, b as usize);
            let delta = self.pos[b] - self.pos[a];
            let dist = delta.length().max(0.01);
            let force = dist * dist / K;
            let dir = delta / dist;
            self.disp[a] += dir * force;
            self.disp[b] -= dir * force;
        }

        // --- 槽位约束：弱回心力 + 边缘软墙 ---
        for i in 0..n {
            let c = &self.comps[self.comp_of[i] as usize];
            let delta = c.anchor - self.pos[i].to_vec2();
            let dist = delta.length();
            if dist < 1e-4 {
                continue;
            }
            let dir = delta / dist;
            self.disp[i] += dir * (dist * 0.02);
            let over = dist - c.radius;
            if over > 0.0 {
                self.disp[i] += dir * (over * over / K) * 2.0;
            }
        }

        // --- 限幅位移并降温 ---
        let temp = self.alpha * K * 0.7;
        for i in 0..n {
            if Some(i) == self.pinned {
                continue;
            }
            let d = self.disp[i];
            let len = d.length();
            if len > 1e-4 {
                self.pos[i] += d / len * len.min(temp);
            }
        }
        self.alpha *= 0.985;
        if self.alpha < 0.005 {
            self.alpha = 0.0;
        }
    }

    /// 节点的碰撞半径，和画出来的圆保持一致。
    fn node_radius(&self, i: usize) -> f32 {
        (7.0 + 3.0 * (self.degree[i] as f32).sqrt()) * self.node_scale
    }

    /// 布局收敛之后的气泡漂浮。
    ///
    /// 和之前渲染时加正弦偏移的做法完全不同：这里是真的在积分速度。
    /// 每个节点有一个缓慢转向的游走方向（转一圈要十几秒），配上较重的阻尼，
    /// 于是它是在「滑」而不是在「抖」；两个节点的圆碰上了才互相弹开。
    pub fn drift_step(&mut self, dt: f32) {
        let n = self.pos.len();
        if n == 0 || self.drift <= 0.0 {
            return;
        }

        // --- 游走：方向慢慢转，给一个很小的加速度 ---
        for i in 0..n {
            self.wander[i] += self.wander_rate[i] * dt;
            let dir = Vec2::angled(self.wander[i]);
            self.vel[i] += dir * (WANDER_ACCEL * self.drift * dt);
        }

        // --- 边：回到理想长度的弹簧，别让团被游走力扯散 ---
        for &(a, b) in &self.edges {
            let (a, b) = (a as usize, b as usize);
            let delta = self.pos[b] - self.pos[a];
            let dist = delta.length().max(0.01);
            let dir = delta / dist;
            let f = (dist - K) * EDGE_SPRING * dt;
            self.vel[a] += dir * f;
            self.vel[b] -= dir * f;
        }

        // --- 槽位软墙：越界就被推回自己的地盘 ---
        for i in 0..n {
            let c = &self.comps[self.comp_of[i] as usize];
            let delta = c.anchor - self.pos[i].to_vec2();
            let dist = delta.length();
            if dist < 1e-4 {
                continue;
            }
            let over = dist - c.radius;
            if over > 0.0 {
                self.vel[i] += delta / dist * (over * WALL_SPRING * dt);
            }
        }

        self.resolve_collisions();

        // --- 积分：阻尼 + 限速，保证是慢悠悠地飘 ---
        for i in 0..n {
            if Some(i) == self.pinned {
                self.vel[i] = Vec2::ZERO;
                continue;
            }
            self.vel[i] *= DAMPING;
            let speed = self.vel[i].length();
            let cap = MAX_SPEED * self.drift;
            if speed > cap {
                self.vel[i] *= cap / speed;
            }
            self.pos[i] += self.vel[i] * dt;
        }
    }

    /// 气泡碰撞：圆压到一起就分开，并沿法线交换速度。
    /// 用均匀网格做邻域查询，不然节点一多就退化成两两比较。
    fn resolve_collisions(&mut self) {
        let n = self.pos.len();
        if n < 2 {
            return;
        }
        let radii: Vec<f32> = (0..n).map(|i| self.node_radius(i)).collect();
        let max_r = radii.iter().copied().fold(0.0f32, f32::max);
        let cell = (2.0 * max_r + COLLIDE_PAD).max(1.0);

        let key = |p: Pos2| ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32);
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::with_capacity(n);
        for i in 0..n {
            grid.entry(key(self.pos[i])).or_default().push(i as u32);
        }

        for i in 0..n {
            let (cx, cy) = key(self.pos[i]);
            for gx in cx - 1..=cx + 1 {
                for gy in cy - 1..=cy + 1 {
                    let Some(bucket) = grid.get(&(gx, gy)) else {
                        continue;
                    };
                    for &jj in bucket {
                        let j = jj as usize;
                        if j <= i {
                            continue;
                        }
                        let delta = self.pos[j] - self.pos[i];
                        let dist = delta.length();
                        let min_d = radii[i] + radii[j] + COLLIDE_PAD;
                        if dist >= min_d || dist < 1e-4 {
                            continue;
                        }
                        let dir = delta / dist;
                        let overlap = min_d - dist;

                        // 先把重叠推开，避免一直穿透着抖
                        let push = dir * (overlap * 0.5);
                        if Some(i) != self.pinned {
                            self.pos[i] -= push;
                        }
                        if Some(j) != self.pinned {
                            self.pos[j] += push;
                        }

                        // 只在互相靠近时反弹，分开中的不要再踹一脚
                        let approach = (self.vel[j] - self.vel[i]).dot(dir);
                        if approach < 0.0 {
                            let imp = dir * (-(1.0 + RESTITUTION) * approach * 0.5);
                            self.vel[i] -= imp;
                            self.vel[j] += imp;
                        }
                    }
                }
            }
        }
    }

    pub fn bounds(&self) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        for p in &self.pos {
            rect.extend_with(*p);
        }
        rect
    }
}

/// BFS 切连通分量，按大小降序排列（大团先装箱，排出来更整齐）。
fn split_components(n: usize, adj: &[Vec<u32>]) -> (Vec<u32>, Vec<Component>) {
    let mut comp_of = vec![u32::MAX; n];
    let mut groups: Vec<Vec<u32>> = Vec::new();
    let mut stack: Vec<u32> = Vec::new();

    for start in 0..n {
        if comp_of[start] != u32::MAX {
            continue;
        }
        let id = groups.len() as u32;
        let mut members = Vec::new();
        comp_of[start] = id;
        stack.push(start as u32);
        while let Some(cur) = stack.pop() {
            members.push(cur);
            for &nb in &adj[cur as usize] {
                if comp_of[nb as usize] == u32::MAX {
                    comp_of[nb as usize] = id;
                    stack.push(nb);
                }
            }
        }
        groups.push(members);
    }

    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    let mut comps: Vec<Component> = Vec::with_capacity(groups.len());
    for (id, members) in groups.into_iter().enumerate() {
        for &m in &members {
            comp_of[m as usize] = id as u32;
        }
        let radius = slot_radius(members.len());
        comps.push(Component {
            members,
            anchor: Vec2::ZERO,
            radius,
        });
    }
    (comp_of, comps)
}

/// 把下标打散成 [0,1) 的伪随机数，给每个节点一个固定的游走相位。
fn hash01(x: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9);
    h ^= h >> 15;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// 一个 n 节点的团摊开后大概占多大。
fn slot_radius(n: usize) -> f32 {
    if n <= 1 {
        K * 0.30
    } else {
        K * (0.55 * (n as f32).sqrt() + 0.35)
    }
}

/// 同心环装箱：最大的团占住圆心，其余的按半径从大到小一圈圈往外排，
/// 整体轮廓是个圆盘。
///
/// 环内不重叠的依据：把第 i 个团在环上占的半角取成
/// `h_i = asin((R_i + 间隙/2) / 环半径)`，相邻两团圆心的夹角就是 `h_i + h_j`，
/// 于是圆心距 `2r·sin((h_i+h_j)/2) ≥ r·sin(h_i) + r·sin(h_j) = R_i + R_j + 间隙`
/// （用了和差化积，`sin` 在这段上是凹的）。环与环之间则靠外接半径隔开。
fn pack(comps: &mut [Component]) {
    if comps.is_empty() {
        return;
    }
    let gutter = K * 0.45;

    // 最大的一团放圆心
    comps[0].anchor = Vec2::ZERO;
    let mut ring_outer = comps[0].radius;
    let mut i = 1usize;

    while i < comps.len() {
        // 本环里最大的就是排在最前面的那个（已按半径降序）
        let r_max = comps[i].radius;
        let ring_r = ring_outer + gutter + r_max;

        // 先数一数这一圈能塞下几个
        let half_angle = |radius: f32| -> f32 {
            ((radius + gutter * 0.5) / ring_r).clamp(0.0, 1.0).asin()
        };
        let mut used = 0.0f32;
        let mut count = 0usize;
        while i + count < comps.len() {
            let w = 2.0 * half_angle(comps[i + count].radius);
            if count > 0 && used + w > std::f32::consts::TAU {
                break;
            }
            used += w;
            count += 1;
        }

        // 富余的角度均摊到每个团之间，排得匀一些
        let slack = (std::f32::consts::TAU - used).max(0.0);
        let gap = slack / count as f32;
        let mut cursor = 0.0f32;
        for j in 0..count {
            let c = &mut comps[i + j];
            let h = half_angle(c.radius);
            cursor += h;
            c.anchor = Vec2::angled(cursor) * ring_r;
            cursor += h + gap;
        }

        ring_outer = ring_r + r_max;
        i += count;
    }
}

// ---------------------------------------------------------------- Barnes-Hut

struct QNode {
    com: Vec2,
    mass: f32,
    half: f32,
    /// 四个象限的孩子，-1 表示空
    child: [i32; 4],
}

struct QuadTree {
    nodes: Vec<QNode>,
    root: i32,
}

impl QuadTree {
    /// 只对 `subset` 里的点建树。
    fn build(points: &[Pos2], subset: &[u32]) -> QuadTree {
        let mut tree = QuadTree {
            nodes: Vec::with_capacity(subset.len() * 2),
            root: -1,
        };
        if subset.is_empty() {
            return tree;
        }
        let mut rect = egui::Rect::NOTHING;
        for &i in subset {
            rect.extend_with(points[i as usize]);
        }
        let center = rect.center().to_vec2();
        let half = (rect.width().max(rect.height()) * 0.5).max(1.0) * 1.05;
        tree.root = tree.insert(points, subset, center, half, 0);
        tree
    }

    /// 自顶向下按象限切分，返回新建节点的下标。
    fn insert(&mut self, points: &[Pos2], idx: &[u32], center: Vec2, half: f32, depth: u32) -> i32 {
        if idx.is_empty() {
            return -1;
        }
        let mass = idx.len() as f32;
        let mut com = Vec2::ZERO;
        for &i in idx {
            com += points[i as usize].to_vec2();
        }
        com /= mass;

        let me = self.nodes.len() as i32;
        self.nodes.push(QNode {
            com,
            mass,
            half,
            child: [-1; 4],
        });

        if idx.len() == 1 || depth >= MAX_DEPTH {
            return me;
        }

        let mut buckets: [Vec<u32>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for &i in idx {
            let p = points[i as usize];
            let q = (if p.x >= center.x { 1 } else { 0 }) | (if p.y >= center.y { 2 } else { 0 });
            buckets[q].push(i);
        }

        let quarter = half * 0.5;
        for (q, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let c = Vec2::new(
                center.x + if q & 1 != 0 { quarter } else { -quarter },
                center.y + if q & 2 != 0 { quarter } else { -quarter },
            );
            let child = self.insert(points, bucket, c, quarter, depth + 1);
            self.nodes[me as usize].child[q] = child;
        }
        me
    }

    fn repulsion(&self, p: Pos2, k2: f32) -> Vec2 {
        let mut force = Vec2::ZERO;
        if self.root >= 0 {
            self.accumulate(self.root as usize, p.to_vec2(), k2, &mut force);
        }
        force
    }

    fn accumulate(&self, i: usize, p: Vec2, k2: f32, out: &mut Vec2) {
        let n = &self.nodes[i];
        let delta = n.com - p;
        let dist2 = delta.length_sq();
        let is_leaf = n.child.iter().all(|&c| c < 0);

        // 就是自己：跳过
        if is_leaf && n.mass <= 1.0 && dist2 < 1e-6 {
            return;
        }

        let dist = dist2.max(0.25).sqrt();
        if is_leaf || (2.0 * n.half / dist) < THETA {
            // FR 斥力 k^2/d，方向背离质心
            *out -= delta / dist * (k2 * n.mass / dist);
            return;
        }
        for &c in &n.child {
            if c >= 0 {
                self.accumulate(c as usize, p, k2, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_no_overlap(sizes: &[usize]) {
        let mut comps: Vec<Component> = sizes
            .iter()
            .map(|&n| Component {
                members: (0..n as u32).collect(),
                anchor: Vec2::ZERO,
                radius: slot_radius(n),
            })
            .collect();
        pack(&mut comps);

        for i in 0..comps.len() {
            for j in i + 1..comps.len() {
                let d = (comps[i].anchor - comps[j].anchor).length();
                let need = comps[i].radius + comps[j].radius;
                assert!(d >= need * 0.98, "分量 {i} 和 {j} 的槽位重叠了: {d} < {need}");
            }
        }
    }

    /// 装箱后各分量的槽位不应该互相重叠。
    #[test]
    fn slots_do_not_overlap() {
        check_no_overlap(&[40, 12, 5, 3, 3, 2, 2, 2, 1, 1, 1, 1, 1]);
    }

    /// 真实数据的形状：一个大团 + 一长串两三个词的小团 + 一堆孤词。
    /// 这种极端的半径落差最容易在环上排错。
    #[test]
    fn slots_do_not_overlap_at_scale() {
        let mut sizes: Vec<usize> = vec![3000, 51, 43, 40, 33, 28, 25, 25, 23];
        sizes.extend(std::iter::repeat_n(3, 60));
        sizes.extend(std::iter::repeat_n(2, 400));
        sizes.extend(std::iter::repeat_n(1, 800));
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        check_no_overlap(&sizes);
    }

    /// 两个气泡压在一起时应该被弹开。
    #[test]
    fn overlapping_bubbles_push_apart() {
        let mut sim = Sim::new(vec![0, 1], Vec::new());
        sim.pos[0] = Pos2::new(0.0, 0.0);
        sim.pos[1] = Pos2::new(4.0, 0.0);
        let before = (sim.pos[1] - sim.pos[0]).length();
        sim.drift_step(1.0 / 60.0);
        let after = (sim.pos[1] - sim.pos[0]).length();
        assert!(after > before, "重叠的气泡没有被推开: {before} -> {after}");
    }

    /// 漂浮再久也不该飘出自己的槽位，否则词团之间会串味。
    #[test]
    fn drifting_stays_inside_its_slot() {
        let ids: Vec<u32> = (0..30).collect();
        let mut sim = Sim::new(ids, Vec::new());
        while !sim.is_settled() {
            sim.step();
        }
        for _ in 0..1800 {
            sim.drift_step(1.0 / 60.0);
        }
        for i in 0..sim.len() {
            let c = &sim.comps[sim.comp_of[i] as usize];
            let d = (sim.pos[i].to_vec2() - c.anchor).length();
            assert!(d <= c.radius + 12.0, "节点 {i} 飘出槽位: {d} > {}", c.radius);
        }
    }

    /// 一堆互不相连的词应该排成一个圆盘：宽高相当，且中间不能是空的。
    #[test]
    fn disconnected_nodes_form_a_disc() {
        let ids: Vec<u32> = (0..200).collect();
        let sim = Sim::new(ids, Vec::new());
        assert_eq!(sim.component_count(), 200);

        let b = sim.bounds();
        let ratio = b.width() / b.height();
        assert!((0.75..1.35).contains(&ratio), "不够圆: 宽高比 {ratio}");

        // 圆心附近必须有节点，否则又变成空心环了
        let c = b.center();
        let inner = b.width() * 0.25;
        let n_inner = sim
            .pos
            .iter()
            .filter(|p| (**p - c.to_vec2()).to_vec2().length() < inner)
            .count();
        assert!(n_inner > 0, "圆心是空的");
    }
}
