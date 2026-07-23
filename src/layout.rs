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
/// 力导向力在漂浮时的增益。图已经在平衡点附近，只需缓慢拉回结构。
const FORCE_GAIN: f32 = 14.0;
/// 力场低通响应速度。Barnes-Hut 树在边界附近会产生细小跳变，不能直接喂给速度。
const FORCE_RESPONSE: f32 = 2.4;
/// 游走加速度。有斥力维持结构之后，游走只提供很轻的惯性漂移。
const WANDER_ACCEL: f32 = 3.2;
/// 速度衰减，按 60fps 每帧计；实际会按 dt 换算，换帧率不影响手感
const DAMPING: f32 = 0.94;
/// 限速，世界单位/秒。K 是 62，所以横穿一条边要七八秒
const MAX_SPEED: f32 = 7.0;
/// 斥力倍率。碰撞已经保证不重叠了，斥力只用来把团摊开，所以压得比较低
const REPULSION: f32 = 0.35;
/// 气泡之间留一点缝，看起来不至于糊在一起
const COLLIDE_PAD: f32 = 3.0;
/// 允许极小的接触误差，避免浮点误差让两个静止节点逐帧来回修正。
const COLLISION_SLOP: f32 = 0.12;
/// 每次只修正大部分穿透，连续两轮约束比一次硬弹开更平滑。
const COLLISION_CORRECTION: f32 = 0.82;

/// 一个节点在世界坐标里实际占的地方 —— 圆 **加上** 它底下那行单词。
/// 碰撞按这个包围盒算，所以两个单词的字母永远不会叠在一起。
#[derive(Clone, Copy)]
pub struct Extent {
    /// 包围盒的半宽半高
    pub half: Vec2,
    /// 包围盒中心相对节点锚点的 y 偏移
    pub off_y: f32,
}

impl Extent {
    fn fallback(radius: f32) -> Extent {
        Extent {
            half: Vec2::splat(radius),
            off_y: 0.0,
        }
    }
}

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
    /// 低通后的力场，隔离 Barnes-Hut 近似和接触边界产生的高频变化。
    force_ema: Vec<Vec2>,
    /// 每个节点当前的游走方向，以及这个方向自转的角速度
    wander: Vec<f32>,
    wander_rate: Vec<f32>,
    /// 漂浮速度倍率，0 表示完全静止
    pub drift: f32,
    /// 每个节点连标签在内占的地方
    extents: Vec<Extent>,
}

impl Sim {
    pub fn new(ids: Vec<u32>, edges: Vec<(u32, u32)>, extents: Vec<Extent>) -> Sim {
        let n = ids.len();
        let extents = if extents.len() == n {
            extents
        } else {
            vec![Extent::fallback(9.0); n]
        };
        let local: HashMap<u32, usize> = ids.iter().enumerate().map(|(i, &g)| (g, i)).collect();

        let mut degree = vec![0u32; n];
        let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
        for &(a, b) in &edges {
            degree[a as usize] += 1;
            degree[b as usize] += 1;
            adj[a as usize].push(b);
            adj[b as usize].push(a);
        }

        let (comp_of, mut comps) = split_components(n, &adj, &extents);
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
            force_ema: vec![Vec2::ZERO; n],
            // 相位和转速都散列自下标：各转各的，不会同步成整齐划一的抽动
            wander: (0..n).map(|i| hash01(i as u32) * std::f32::consts::TAU).collect(),
            wander_rate: (0..n)
                .map(|i| 0.12 + hash01(i as u32 ^ 0x5BF0_3635) * 0.30)
                .collect(),
            drift: 1.0,
            extents,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// 某个节点连标签在内占的地方，命中测试和绘制都要用。
    pub fn extent(&self, i: usize) -> Extent {
        self.extents[i]
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
        for velocity in &mut self.vel {
            *velocity *= 0.2;
        }
        self.force_ema.fill(Vec2::ZERO);
    }

    /// 把力导向的三种力（斥力 + 边引力 + 槽位约束）累加到 `self.disp`。
    /// 退火 `step` 和漂浮 `drift_step` 用的是同一套力 —— 这样漂浮时图会一直
    /// 停在退火算出的那个平衡形状上，而不会因为缺了斥力慢慢塌成一团。
    fn accumulate_forces(&mut self) {
        let n = self.pos.len();
        for d in &mut self.disp {
            *d = Vec2::ZERO;
        }

        // --- 斥力：只在分量内部算，分量之间靠装箱隔开。这是维持「团」形状的关键 ---
        let k2 = K * K * REPULSION;
        for c in &self.comps {
            if c.members.len() < 2 {
                continue;
            }
            let tree = QuadTree::build(&self.pos, &c.members);
            for &i in &c.members {
                self.disp[i as usize] += tree.repulsion(self.pos[i as usize], k2);
            }
        }

        // --- 边的引力：拉到理想长度，让相连的词贴在一起 ---
        for &(a, b) in &self.edges {
            let (a, b) = (a as usize, b as usize);
            let delta = self.pos[b] - self.pos[a];
            let dist = delta.length().max(0.01);
            let dir = delta / dist;
            let force = (dist - self.rest_len(a, b)) * 0.22;
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
    }

    pub fn step(&mut self) {
        let n = self.pos.len();
        if n == 0 {
            return;
        }
        self.accumulate_forces();

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

        // 位移完成后再解碰撞，不能拿旧位置算完碰撞又把节点推回重叠区。
        for _ in 0..2 {
            self.resolve_collisions(false);
        }
        self.alpha *= 0.985;
        if self.alpha < 0.005 {
            self.alpha = 0.0;
        }
    }

    /// 布局收敛之后的气泡漂浮。
    ///
    /// 和之前渲染时加正弦偏移的做法完全不同：这里是真的在积分速度。
    /// 每个节点有一个缓慢转向的游走方向（转一圈要十几秒），配上较重的阻尼，
    /// 于是它是在「滑」而不是在「抖」；两个节点接触时会消掉相向速度。
    pub fn drift_step(&mut self, dt: f32) {
        let n = self.pos.len();
        if n == 0 || self.drift <= 0.0 {
            return;
        }

        // 和退火同一套力：斥力把团摊开、边引力把相连的拉近、槽位约束居中。
        // 缺了斥力就是之前塌成一团的病根，所以这里必须一起算。
        self.accumulate_forces();

        let force_blend = 1.0 - (-FORCE_RESPONSE * dt).exp();
        for i in 0..n {
            // 先把力场低通，再作为加速度。树结构或碰撞边界的一帧跳变不会直接
            // 反映到节点位置上，慢变化的结构恢复力仍然完整保留。
            let smoothed_force = self.force_ema[i];
            self.force_ema[i] += (self.disp[i] - smoothed_force) * force_blend;
            self.vel[i] += self.force_ema[i] * (FORCE_GAIN * dt);

            // 游走：方向缓慢转动，给一点有机的漂移感，幅度很小别盖过结构
            self.wander[i] += self.wander_rate[i] * dt;
            self.vel[i] += Vec2::angled(self.wander[i]) * (WANDER_ACCEL * self.drift * dt);
        }

        // --- 积分：阻尼 + 限速，保证是慢悠悠地飘 ---
        // 阻尼是「每帧」的量，按 dt 折算成等效衰减，30fps 和 60fps 手感一致
        let damping = DAMPING.powf(dt * 60.0);
        for i in 0..n {
            if Some(i) == self.pinned {
                self.vel[i] = Vec2::ZERO;
                self.force_ema[i] = Vec2::ZERO;
                continue;
            }
            self.vel[i] *= damping;
            let speed = self.vel[i].length();
            let cap = MAX_SPEED * self.drift;
            if speed > cap {
                self.vel[i] *= cap / speed;
            }
            self.pos[i] += self.vel[i] * dt;
        }

        // 先完成本帧运动，再处理接触。接触只消掉相向速度，不产生反弹。
        for _ in 0..2 {
            self.resolve_collisions(true);
        }
    }

    /// 一条边的理想长度。固定值不行 —— 胶囊本身就有一两百个单位宽，
    /// 比固定边长还长，连着的两个词反而被挤得老远，图就没有「关系感」了。
    /// 所以理想长度按两端胶囊的宽度来定，让它们刚好挨着。
    fn rest_len(&self, a: usize, b: usize) -> f32 {
        (self.extents[a].half.x + self.extents[b].half.x) * 0.95 + K * 0.30
    }

    /// 包围盒中心（节点圆心往下偏一点，因为标签在圆的下面）。
    fn box_center(&self, i: usize) -> Vec2 {
        self.pos[i].to_vec2() + Vec2::new(0.0, self.extents[i].off_y)
    }

    /// 气泡碰撞。
    ///
    /// 碰的是「圆 + 底下那行单词」的包围盒，不是光秃秃的圆 —— 所以两个单词的
    /// 字母一旦要压到一起，节点就会先被分开，永远轮不到「谁的标签让位」。
    /// 用均匀网格做邻域查询，不然节点一多就退化成两两比较。
    fn resolve_collisions(&mut self, affect_velocity: bool) {
        let n = self.pos.len();
        if n < 2 {
            return;
        }
        let max_half = self
            .extents
            .iter()
            .map(|e| e.half.x.max(e.half.y))
            .fold(0.0f32, f32::max);
        let cell = (2.0 * max_half + COLLIDE_PAD).max(1.0);

        let key = |p: Vec2| ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32);
        let mut grid: HashMap<(i32, i32), Vec<u32>> = HashMap::with_capacity(n);
        for i in 0..n {
            grid.entry(key(self.box_center(i))).or_default().push(i as u32);
        }

        for i in 0..n {
            let (cx, cy) = key(self.box_center(i));
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
                        let delta = self.box_center(j) - self.box_center(i);
                        let need_x = self.extents[i].half.x + self.extents[j].half.x + COLLIDE_PAD;
                        let need_y = self.extents[i].half.y + self.extents[j].half.y + COLLIDE_PAD;
                        let over_x = need_x - delta.x.abs();
                        let over_y = need_y - delta.y.abs();
                        if over_x <= 0.0 || over_y <= 0.0 {
                            continue; // 盒子没相交
                        }

                        // 深度非常接近时固定选择一根轴，避免 x/y 法线逐帧翻转。
                        let prefer_x = ((i as u32).wrapping_mul(0x9E37_79B9) ^ j as u32) & 1 == 0;
                        let use_x = if (over_x - over_y).abs() < 1.0 {
                            prefer_x
                        } else {
                            over_x < over_y
                        };
                        let (normal, depth) = if use_x {
                            (
                                Vec2::new(if delta.x < 0.0 { -1.0 } else { 1.0 }, 0.0),
                                over_x,
                            )
                        } else {
                            (
                                Vec2::new(0.0, if delta.y < 0.0 { -1.0 } else { 1.0 }),
                                over_y,
                            )
                        };

                        let movable_i = (Some(i) != self.pinned) as u8 as f32;
                        let movable_j = (Some(j) != self.pinned) as u8 as f32;
                        let movable = movable_i + movable_j;
                        if movable <= 0.0 {
                            continue;
                        }

                        // 留一点亚像素容差并渐进修正；钉住一端时由另一端承担全部位移。
                        let correction =
                            normal * ((depth - COLLISION_SLOP).max(0.0) * COLLISION_CORRECTION);
                        self.pos[i] -= correction * (movable_i / movable);
                        self.pos[j] += correction * (movable_j / movable);

                        if !affect_velocity {
                            continue;
                        }

                        // 只消掉相向的法线速度，不反弹。可视化节点不是弹珠，
                        // 零恢复系数能让接触自然停稳，也不会下一帧再次撞回来。
                        let approach = (self.vel[j] - self.vel[i]).dot(normal);
                        if approach < 0.0 {
                            let impulse = normal * (-approach / movable);
                            self.vel[i] -= impulse * movable_i;
                            self.vel[j] += impulse * movable_j;
                        }
                    }
                }
            }
        }
    }

    /// 整张图的包围盒。要把胶囊的宽高算进去，否则边上那些长单词会被切掉一半。
    pub fn bounds(&self) -> egui::Rect {
        let mut rect = egui::Rect::NOTHING;
        for (i, p) in self.pos.iter().enumerate() {
            let e = &self.extents[i];
            let c = *p + Vec2::new(0.0, e.off_y);
            rect.extend_with(c - e.half);
            rect.extend_with(c + e.half);
        }
        rect
    }
}

/// BFS 切连通分量，按大小降序排列（大团先装箱，排出来更整齐）。
fn split_components(
    n: usize,
    adj: &[Vec<u32>],
    extents: &[Extent],
) -> (Vec<u32>, Vec<Component>) {
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
        let radius = slot_radius_for(&members, extents);
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

/// 一个团摊开后大概占多大。
///
/// 之前只按节点个数估，现在标签也参与碰撞了，长单词占的地方大得多，
/// 所以改成按成员包围盒的总面积折算，否则槽位装不下、节点会一直顶着软墙。
fn slot_radius_for(members: &[u32], extents: &[Extent]) -> f32 {
    let area: f32 = members
        .iter()
        .map(|&m| {
            let e = &extents[m as usize];
            4.0 * e.half.x * e.half.y
        })
        .sum();
    // 装箱填充率大概七成，再留一圈余量
    let by_area = (area / std::f32::consts::PI).sqrt() * 1.25 + K * 0.20;
    // 至少要装得下最大的那一个
    let biggest = members
        .iter()
        .map(|&m| extents[m as usize].half.length())
        .fold(0.0f32, f32::max);
    by_area.max(biggest * 1.08)
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

    /// 造一组假的 extent：每个节点一个 18x9 的盒子，够测装箱了。
    fn fake_extents(n: usize) -> Vec<Extent> {
        vec![
            Extent {
                half: Vec2::new(18.0, 9.0),
                off_y: 4.0,
            };
            n
        ]
    }

    fn check_no_overlap(sizes: &[usize]) {
        let total: usize = sizes.iter().sum();
        let ext = fake_extents(total);
        let mut comps: Vec<Component> = sizes
            .iter()
            .map(|&n| {
                let members: Vec<u32> = (0..n as u32).collect();
                let radius = slot_radius_for(&members, &ext);
                Component {
                    members,
                    anchor: Vec2::ZERO,
                    radius,
                }
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
        let mut sim = Sim::new(vec![0, 1], Vec::new(), fake_extents(2));
        sim.pos[0] = Pos2::new(0.0, 0.0);
        sim.pos[1] = Pos2::new(6.0, 0.0);
        let before = (sim.pos[1] - sim.pos[0]).length();
        sim.drift_step(1.0 / 60.0);
        let after = (sim.pos[1] - sim.pos[0]).length();
        assert!(after > before, "重叠的气泡没有被推开: {before} -> {after}");
    }

    /// 接触只应阻止继续靠近，不能产生下一帧撞回来的反弹速度。
    #[test]
    fn collision_contact_does_not_bounce() {
        let mut sim = Sim::new(vec![0, 1], Vec::new(), fake_extents(2));
        sim.pos[0] = Pos2::new(0.0, 0.0);
        sim.pos[1] = Pos2::new(30.0, 0.0);
        sim.vel[0] = Vec2::new(5.0, 0.0);
        sim.vel[1] = Vec2::new(-5.0, 0.0);

        sim.resolve_collisions(true);

        let relative_speed = (sim.vel[1] - sim.vel[0]).x;
        assert!(
            relative_speed.abs() < 1e-4,
            "接触后仍有反弹或穿透速度: {relative_speed}"
        );
    }

    /// 漂移可以缓慢转向，但不能连续帧来回翻转方向形成视觉抖动。
    #[test]
    fn drift_has_no_high_frequency_direction_flips() {
        let ids: Vec<u32> = (0..14).collect();
        let edges: Vec<(u32, u32)> = (1..14).map(|i| (0, i)).collect();
        let mut sim = Sim::new(ids, edges, fake_extents(14));
        while !sim.is_settled() {
            sim.step();
        }

        let mut previous = vec![Vec2::ZERO; sim.len()];
        let mut reversals = 0usize;
        let mut moving_samples = 0usize;
        for _ in 0..900 {
            let before = sim.pos.clone();
            sim.drift_step(1.0 / 60.0);
            for i in 0..sim.len() {
                let delta = sim.pos[i] - before[i];
                let product = delta.length() * previous[i].length();
                if product > 1e-6 {
                    moving_samples += 1;
                    if delta.dot(previous[i]) < product * -0.5 {
                        reversals += 1;
                    }
                }
                previous[i] = delta;
            }
        }

        assert!(
            reversals * 200 < moving_samples.max(1),
            "连续帧方向反转过多: {reversals} / {moving_samples}"
        );
    }

    /// 漂浮再久也不该飘出自己的槽位，否则词团之间会串味。
    #[test]
    fn drifting_stays_inside_its_slot() {
        let ids: Vec<u32> = (0..30).collect();
        let mut sim = Sim::new(ids, Vec::new(), fake_extents(30));
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

    /// 标签的包围盒不许叠在一起 —— 叠了就说明会出现「字母压字母」。
    #[test]
    fn label_boxes_never_overlap_after_settling() {
        // 一个 12 个词的团，标签比圆大得多，最容易挤在一起
        let ids: Vec<u32> = (0..12).collect();
        let edges: Vec<(u32, u32)> = (1..12).map(|i| (0, i)).collect();
        let ext = vec![
            Extent {
                half: Vec2::new(46.0, 12.0),
                off_y: 6.0,
            };
            12
        ];
        let mut sim = Sim::new(ids, edges, ext);
        while !sim.is_settled() {
            sim.step();
        }
        for _ in 0..600 {
            sim.drift_step(1.0 / 60.0);
        }
        for i in 0..sim.len() {
            for j in i + 1..sim.len() {
                let d = sim.box_center(j) - sim.box_center(i);
                let need_x = sim.extents[i].half.x + sim.extents[j].half.x;
                let need_y = sim.extents[i].half.y + sim.extents[j].half.y;
                assert!(
                    d.x.abs() >= need_x * 0.95 || d.y.abs() >= need_y * 0.95,
                    "节点 {i} 和 {j} 的标签压在一起了: {d:?}"
                );
            }
        }
    }

    /// 一堆互不相连的词应该排成一个圆盘：宽高相当，且中间不能是空的。
    #[test]
    fn disconnected_nodes_form_a_disc() {
        let ids: Vec<u32> = (0..200).collect();
        let sim = Sim::new(ids, Vec::new(), fake_extents(200));
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
