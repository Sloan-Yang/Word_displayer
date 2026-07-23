//! 三维地球（真 OpenGL）。
//!
//! 用 eframe 暴露的 glow 上下文，在 egui 的 `PaintCallback` 里画一颗自转的
//! 实心球：着色 + 程序化经纬网。地球本体是真三维；漂在外面的作家/作品光点
//! 由 `app` 用**同一个**视图矩阵和透视参数投影成 2D 叠在上面画，
//! 背面的光点按相机到光点的射线自行遮挡。
//!
//! egui_glow 的约定（见 painter.rs）对我们非常有利：
//! - 调回调前，GL viewport 已被设成回调的 rect，裁剪盒已按 clip_rect 开好；
//! - 回调返回后，egui 会重新 `prepare_painting`，把 DEPTH_TEST/CULL_FACE 关掉、
//!   自己的 program/blend/vao 恢复回去。所以我们在回调里可以随意开深度测试、
//!   换程序，不必善后。
//!
//! 需要一个深度缓冲：`main.rs` 里给 NativeOptions 开了 `depth_buffer: 24`。

use eframe::glow;
use glow::HasContext as _;

/// 轨道相机到球心的距离（球半径为 1）。
///
/// 留出足够距离可以让透视自然但不过度，同时保证最外层的书本光点不会穿过相机。
pub const CAMERA_DISTANCE: f32 = 4.0;

/// 3x3 行主序旋转矩阵。既喂给着色器，也在 CPU 侧投影光点。
#[derive(Clone, Copy)]
pub struct Mat3 {
    pub m: [[f32; 3]; 3],
}

impl Mat3 {
    #[allow(dead_code)] // 供测试与将来备用
    pub fn identity() -> Mat3 {
        Mat3 {
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        }
    }

    pub fn mul(self, o: Mat3) -> Mat3 {
        let mut r = [[0.0f32; 3]; 3];
        for i in 0..3 {
            for j in 0..3 {
                r[i][j] = self.m[i][0] * o.m[0][j]
                    + self.m[i][1] * o.m[1][j]
                    + self.m[i][2] * o.m[2][j];
            }
        }
        Mat3 { m: r }
    }

    /// 作用到一个向量上。
    pub fn apply(self, v: [f32; 3]) -> [f32; 3] {
        [
            self.m[0][0] * v[0] + self.m[0][1] * v[1] + self.m[0][2] * v[2],
            self.m[1][0] * v[0] + self.m[1][1] * v[1] + self.m[1][2] * v[2],
            self.m[2][0] * v[0] + self.m[2][1] * v[1] + self.m[2][2] * v[2],
        ]
    }

    /// 列主序展开，给 `uniform_matrix_3_f32_slice`（transpose=false）用。
    pub fn col_major(self) -> [f32; 9] {
        let m = self.m;
        [
            m[0][0], m[1][0], m[2][0], m[0][1], m[1][1], m[2][1], m[0][2], m[1][2], m[2][2],
        ]
    }
}

fn rot_x(a: f32) -> Mat3 {
    let (s, c) = a.sin_cos();
    Mat3 {
        m: [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
    }
}

fn rot_y(a: f32) -> Mat3 {
    let (s, c) = a.sin_cos();
    Mat3 {
        m: [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
    }
}

/// 地球模型和轨道相机合成后的视图旋转。
///
/// 地球先在世界空间绕 Y 轴自转；相机默认位于 +Z，`camera_yaw` 向右绕 Y 轴，
/// `camera_pitch` 向北半球上方移动。返回值把模型坐标变换到相机坐标，
/// 其中 +Z 表示更靠近镜头。
pub fn scene_rotation(spin: f32, camera_yaw: f32, camera_pitch: f32) -> Mat3 {
    let model = rot_y(spin);
    let view = rot_x(camera_pitch).mul(rot_y(-camera_yaw));
    view.mul(model)
}

/// 让给定经纬度正对镜头时，相机应处于的轨道角度。
pub fn camera_angles_for(lat_deg: f32, lon_deg: f32, spin: f32) -> (f32, f32) {
    (lon_deg.to_radians() + spin, lat_deg.to_radians())
}

/// 相机空间中某个深度对应的透视倍率。
pub fn perspective_factor(view_z: f32) -> f32 {
    CAMERA_DISTANCE / (CAMERA_DISTANCE - view_z)
}

/// 透视下单位球轮廓相对「球心深度处半径」的倍率。
pub fn silhouette_scale() -> f32 {
    CAMERA_DISTANCE / (CAMERA_DISTANCE * CAMERA_DISTANCE - 1.0).sqrt()
}

/// 相机空间中的点是否被单位球遮住。返回 0..=1，掠过轮廓时做一小段柔和过渡。
pub fn point_visibility(point: [f32; 3]) -> f32 {
    let ray = [
        point[0],
        point[1],
        point[2] - CAMERA_DISTANCE,
    ];
    let ray_len2 = ray[0] * ray[0] + ray[1] * ray[1] + ray[2] * ray[2];
    if ray_len2 <= 1e-6 {
        return 1.0;
    }

    // 相机 C=(0,0,d)，求线段 C->point 上离球心最近的位置。
    let t = (CAMERA_DISTANCE * (CAMERA_DISTANCE - point[2]) / ray_len2).clamp(0.0, 1.0);
    if t >= 1.0 - 1e-5 {
        return 1.0;
    }
    let closest = [
        ray[0] * t,
        ray[1] * t,
        CAMERA_DISTANCE + ray[2] * t,
    ];
    let clearance =
        (closest[0] * closest[0] + closest[1] * closest[1] + closest[2] * closest[2])
            .sqrt()
            - 1.0;
    ((clearance + 0.015) / 0.10).clamp(0.0, 1.0)
}

/// 经纬度（角度）-> 单位球面上的点。约定：+Z 朝本初子午线（经度 0），
/// +X 朝东经 90°，+Y 朝北极。这样默认相机（+Z）正对经度 0，向东（经度增大）
/// 在屏幕上向右，和真实地球仪一致。必须和着色器里 `lon = atan(x, z)` 保持一致。
pub fn sphere_point(lat_deg: f32, lon_deg: f32) -> [f32; 3] {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()]
}

/// 画一帧地球需要的参数。
///
/// `PaintCallback` 始终覆盖完整内容画布，`ndc_scale` 决定球体在这个视口里的
/// 横纵半径。这样即使画布不是正方形、窗口缩放或跨屏改变 DPI，球也不会被拉伸。
pub struct Frame {
    pub rot: Mat3,
    /// 球心深度处，一个单位半径分别占视口横/纵方向多少 NDC。
    pub ndc_scale: [f32; 2],
    /// 光照方向（相机空间，指向光源）。
    pub light: [f32; 3],
    pub ocean_top: [f32; 3],
    pub ocean_bottom: [f32; 3],
    /// 陆地颜色（有陆海遮罩时用）。
    pub land: [f32; 3],
    pub grid: [f32; 3],
    pub grid_mix: f32,
    /// 地形底图混入主题陆地色的强度。
    pub surface_mix: f32,
    /// 白色海岸线的强度（0 关闭）。夜间用它勾出大陆轮廓。
    pub coast: f32,
    /// 城市灯光的强度（0 关闭）。夜间在人口密集处叠加金色亮点。
    pub city: f32,
    /// 地形起伏强度（0 关闭）。夜间不上色，只用地形明暗给月光陆地一点海拔感。
    pub relief: f32,
    /// 边缘压暗强度，越大球感越强。
    pub edge_darken: f32,
}

/// 一张单通道陆海遮罩（等距圆柱，白=陆地）。
pub struct MaskImage {
    pub width: i32,
    pub height: i32,
    /// 每像素一个字节的亮度。
    pub data: Vec<u8>,
}

/// 一张 RGB 等距圆柱地形底图。
pub struct SurfaceImage {
    pub width: i32,
    pub height: i32,
    /// 每像素三个字节，依次为 RGB。
    pub data: Vec<u8>,
}

pub struct Globe {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    index_count: i32,
    /// 陆海遮罩纹理（有就用，没有退回纯着色海球）。
    mask: Option<glow::Texture>,
    /// 地形颜色纹理，只在陆地区域内混合。
    surface: Option<glow::Texture>,
    /// 夜间城市灯光（单通道强度，白天不用）。
    lights: Option<glow::Texture>,
}

impl Globe {
    pub fn new(
        gl: &glow::Context,
        mask: Option<MaskImage>,
        surface: Option<SurfaceImage>,
        lights: Option<MaskImage>,
    ) -> Result<Globe, String> {
        unsafe {
            let program = compile(gl)?;
            let (verts, indices) = uv_sphere(64, 128);

            let vao = gl.create_vertex_array()?;
            gl.bind_vertex_array(Some(vao));

            let vbo = gl.create_buffer()?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_cast(&verts),
                glow::STATIC_DRAW,
            );
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 12, 0);

            let ebo = gl.create_buffer()?;
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
            gl.buffer_data_u8_slice(
                glow::ELEMENT_ARRAY_BUFFER,
                bytemuck_cast(&indices),
                glow::STATIC_DRAW,
            );

            gl.bind_vertex_array(None);

            // 上传陆海遮罩为单通道纹理
            let mask = mask.and_then(|img| {
                let tex = gl.create_texture().ok()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::R8 as i32,
                    img.width,
                    img.height,
                    0,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&img.data)),
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::REPEAT as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR_MIPMAP_LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.generate_mipmap(glow::TEXTURE_2D);
                gl.bind_texture(glow::TEXTURE_2D, None);
                Some(tex)
            });

            let surface = surface.and_then(|img| {
                let tex = gl.create_texture().ok()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGB8 as i32,
                    img.width,
                    img.height,
                    0,
                    glow::RGB,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&img.data)),
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_S,
                    glow::REPEAT as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR_MIPMAP_LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.generate_mipmap(glow::TEXTURE_2D);
                gl.bind_texture(glow::TEXTURE_2D, None);
                Some(tex)
            });

            // 城市灯光：单通道，和遮罩一样的上传方式
            let lights = lights.and_then(|img| {
                let tex = gl.create_texture().ok()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::R8 as i32,
                    img.width,
                    img.height,
                    0,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&img.data)),
                );
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_WRAP_T,
                    glow::CLAMP_TO_EDGE as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::LINEAR_MIPMAP_LINEAR as i32,
                );
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::LINEAR as i32,
                );
                gl.generate_mipmap(glow::TEXTURE_2D);
                gl.bind_texture(glow::TEXTURE_2D, None);
                Some(tex)
            });

            Ok(Globe {
                program,
                vao,
                vbo,
                ebo,
                index_count: indices.len() as i32,
                mask,
                surface,
                lights,
            })
        }
    }

    /// egui 已经把 GL viewport 设成完整内容画布，这里不再动 viewport。
    pub fn paint(&self, gl: &glow::Context, f: &Frame) {
        unsafe {
            gl.clear_depth_f32(1.0);
            gl.clear(glow::DEPTH_BUFFER_BIT);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(true);
            // 不做背面剔除，靠深度测试让近处（正面半球）胜出，免去缠绕方向的坑
            gl.disable(glow::CULL_FACE);

            gl.use_program(Some(self.program));
            let u = |name: &str| gl.get_uniform_location(self.program, name);
            gl.uniform_matrix_3_f32_slice(u("u_rot").as_ref(), false, &f.rot.col_major());
            gl.uniform_2_f32(
                u("u_ndc_scale").as_ref(),
                f.ndc_scale[0],
                f.ndc_scale[1],
            );
            gl.uniform_1_f32(u("u_camera_distance").as_ref(), CAMERA_DISTANCE);

            // 陆海遮罩绑到 0 号纹理单元
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, self.mask);
            gl.uniform_1_i32(u("u_mask").as_ref(), 0);
            gl.uniform_1_f32(
                u("u_have_mask").as_ref(),
                if self.mask.is_some() { 1.0 } else { 0.0 },
            );
            gl.uniform_3_f32(u("u_land").as_ref(), f.land[0], f.land[1], f.land[2]);

            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, self.surface);
            gl.uniform_1_i32(u("u_surface").as_ref(), 1);
            gl.uniform_1_f32(
                u("u_have_surface").as_ref(),
                if self.surface.is_some() { 1.0 } else { 0.0 },
            );
            gl.uniform_1_f32(u("u_surface_mix").as_ref(), f.surface_mix);
            gl.uniform_1_f32(u("u_relief").as_ref(), f.relief);

            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, self.lights);
            gl.uniform_1_i32(u("u_lights").as_ref(), 2);
            gl.uniform_1_f32(
                u("u_have_lights").as_ref(),
                if self.lights.is_some() { 1.0 } else { 0.0 },
            );
            gl.uniform_1_f32(u("u_city").as_ref(), f.city);

            let l = norm3(f.light);
            gl.uniform_3_f32(u("u_light").as_ref(), l[0], l[1], l[2]);
            gl.uniform_3_f32(
                u("u_ocean").as_ref(),
                f.ocean_top[0],
                f.ocean_top[1],
                f.ocean_top[2],
            );
            gl.uniform_3_f32(
                u("u_ocean2").as_ref(),
                f.ocean_bottom[0],
                f.ocean_bottom[1],
                f.ocean_bottom[2],
            );
            gl.uniform_3_f32(u("u_grid").as_ref(), f.grid[0], f.grid[1], f.grid[2]);
            gl.uniform_1_f32(u("u_gridmix").as_ref(), f.grid_mix);
            gl.uniform_1_f32(u("u_coast").as_ref(), f.coast);
            gl.uniform_1_f32(u("u_edge").as_ref(), f.edge_darken);

            gl.bind_vertex_array(Some(self.vao));
            gl.draw_elements(glow::TRIANGLES, self.index_count, glow::UNSIGNED_INT, 0);
            gl.bind_vertex_array(None);

            // DEPTH_TEST 等状态由 egui 的 prepare_painting 复原，这里不用管
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
            gl.delete_buffer(self.vbo);
            gl.delete_buffer(self.ebo);
            if let Some(t) = self.mask {
                gl.delete_texture(t);
            }
            if let Some(t) = self.surface {
                gl.delete_texture(t);
            }
            if let Some(t) = self.lights {
                gl.delete_texture(t);
            }
        }
    }
}

unsafe fn compile(gl: &glow::Context) -> Result<glow::Program, String> {
    let program = gl.create_program()?;

    const VS: &str = r#"#version 330 core
        layout(location = 0) in vec3 a_pos;
        uniform mat3 u_rot;
        uniform vec2 u_ndc_scale;
        uniform float u_camera_distance;
        out vec3 v_model;
        out vec3 v_normal;
        void main() {
            v_model = a_pos;
            vec3 p = u_rot * a_pos;
            v_normal = p;

            // 相机位于 +Z 并朝向原点。横纵分别按完整画布尺寸换算，
            // 避免非正方形视口或裁剪后的回调区域把球体拉扁。
            float view_z = p.z - u_camera_distance;
            float near = 0.1;
            float far = 100.0;
            float z_clip =
                ((far + near) / (near - far)) * view_z
                + (2.0 * far * near) / (near - far);
            gl_Position = vec4(
                p.x * u_ndc_scale.x * u_camera_distance,
                p.y * u_ndc_scale.y * u_camera_distance,
                z_clip,
                -view_z
            );
        }
    "#;

    const FS: &str = r#"#version 330 core
        in vec3 v_model;
        in vec3 v_normal;
        uniform vec3 u_light;
        uniform vec3 u_ocean;
        uniform vec3 u_ocean2;
        uniform vec3 u_land;
        uniform vec3 u_grid;
        uniform float u_gridmix;
        uniform float u_coast;
        uniform float u_edge;
        uniform sampler2D u_mask;
        uniform float u_have_mask;
        uniform sampler2D u_surface;
        uniform float u_have_surface;
        uniform float u_surface_mix;
        uniform float u_relief;
        uniform sampler2D u_lights;
        uniform float u_have_lights;
        uniform float u_city;
        out vec4 frag;

        const float PI = 3.14159265;

        void main() {
            vec3 N = normalize(v_normal);
            float lambert = clamp(dot(N, normalize(u_light)), 0.0, 1.0);
            float shade = 0.38 + 0.62 * lambert;
            vec3 water = mix(u_ocean, u_ocean2, clamp(v_model.y * 0.5 + 0.5, 0.0, 1.0));

            // 陆海遮罩：等距圆柱，uv 由经纬度算出（lon = atan(x, z)，东经向右）
            float lo0 = atan(v_model.x, v_model.z);
            float la0 = asin(clamp(v_model.y, -1.0, 1.0));
            vec2 uv = vec2(lo0 / (2.0 * PI) + 0.5, 0.5 - la0 / PI);
            float landf = 0.0;
            if (u_have_mask > 0.5) {
                landf = smoothstep(0.35, 0.65, texture(u_mask, uv).r);
            }
            vec3 mapped_land = u_land;
            if (u_have_surface > 0.5) {
                vec3 terrain = texture(u_surface, uv).rgb;
                // 白天：混入真实地形颜色
                mapped_land = mix(u_land, terrain, u_surface_mix);
                // 夜间：不上色，只用地形明暗给月光陆地一点起伏 ——
                // 低处偏黑、高处/粗糙处偏亮，避免大片纯灰单调
                if (u_relief > 0.001) {
                    float tl = dot(terrain, vec3(0.299, 0.587, 0.114));
                    float f = 0.30 + 1.25 * tl;   // 明暗范围
                    mapped_land = u_land * mix(1.0, f, u_relief);
                }
            }
            vec3 base = mix(water, mapped_land, landf);
            vec3 col = base * shade;

            // 白色海岸线：遮罩值穿过 0.5 的那条窄带，用 fwidth 做抗锯齿
            if (u_have_mask > 0.5 && u_coast > 0.001) {
                float m = texture(u_mask, uv).r;
                float cw = fwidth(m) * 1.4;
                float coast = 1.0 - smoothstep(0.0, max(cw, 1e-4), abs(m - 0.5));
                coast *= clamp(N.z, 0.0, 1.0);        // 掠射角处压掉
                coast *= 0.4 + 0.6 * lambert;         // 受光面更亮
                col = mix(col, vec3(0.96, 0.98, 1.0), coast * u_coast);
            }

            // 城市灯光：人口密集处的金色亮点，叠一层柔和辉光
            if (u_have_lights > 0.5 && u_city > 0.001) {
                float lp = texture(u_lights, uv).r;
                float lg = textureLod(u_lights, uv, 4.0).r;   // 模糊层做辉光
                float lit = smoothstep(0.05, 0.5, lp) + 0.7 * smoothstep(0.02, 0.45, lg);
                vec3 gold = vec3(1.0, 0.80, 0.45);
                col += gold * lit * u_city * clamp(N.z, 0.0, 1.0);
            }

            // 程序化经纬网：每 15 度一条线
            float lat = degrees(asin(clamp(v_model.y, -1.0, 1.0)));
            float lon = degrees(atan(v_model.x, v_model.z));
            float la = abs(fract(lat / 15.0 + 0.5) - 0.5);
            float lo = abs(fract(lon / 15.0 + 0.5) - 0.5);
            float wla = fwidth(lat / 15.0) * 1.3;
            float wlo = fwidth(lon / 15.0) * 1.3;
            float grid = max(
                1.0 - smoothstep(0.0, max(wla, 1e-4), la),
                1.0 - smoothstep(0.0, max(wlo, 1e-4), lo)
            );
            float facing = clamp(N.z, 0.0, 1.0);
            grid *= pow(facing, 0.5); // 掠射角的线糊成一团，压掉
            // 陆地上把网格压淡一点，让海岸线更清楚
            col = mix(col, u_grid, grid * u_gridmix * (1.0 - 0.5 * landf));

            // 边缘压暗，增强球体的立体感
            col *= mix(1.0 - u_edge, 1.0, pow(facing, 0.7));
            frag = vec4(col, 1.0);
        }
    "#;

    let shaders = [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)];
    let mut handles = Vec::new();
    for (kind, src) in shaders {
        let sh = gl.create_shader(kind)?;
        gl.shader_source(sh, src);
        gl.compile_shader(sh);
        if !gl.get_shader_compile_status(sh) {
            return Err(format!("着色器编译失败: {}", gl.get_shader_info_log(sh)));
        }
        gl.attach_shader(program, sh);
        handles.push(sh);
    }
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        return Err(format!("着色器链接失败: {}", gl.get_program_info_log(program)));
    }
    for sh in handles {
        gl.detach_shader(program, sh);
        gl.delete_shader(sh);
    }
    Ok(program)
}

/// 生成一颗 UV 球的顶点（单位向量）和三角形索引。
fn uv_sphere(stacks: usize, slices: usize) -> (Vec<f32>, Vec<u32>) {
    let mut verts = Vec::with_capacity((stacks + 1) * (slices + 1) * 3);
    for i in 0..=stacks {
        let v = i as f32 / stacks as f32;
        let phi = v * std::f32::consts::PI; // 0..PI，从北极到南极
        let (sp, cp) = phi.sin_cos();
        for j in 0..=slices {
            let u = j as f32 / slices as f32;
            let theta = u * std::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            // y 为极轴；lon = atan(z, x)
            verts.push(sp * ct); // x
            verts.push(cp); // y
            verts.push(sp * st); // z
        }
    }
    let mut indices = Vec::with_capacity(stacks * slices * 6);
    let row = slices + 1;
    for i in 0..stacks {
        for j in 0..slices {
            let a = (i * row + j) as u32;
            let b = a + row as u32;
            indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    (verts, indices)
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    normalize3(v)
}

pub fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-6);
    [v[0] / len, v[1] / len, v[2] / len]
}

pub fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn scale3(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// 给定球面上的单位向量 `u`，返回一组切平面正交基 `(t, b)`。
/// 用来把作家/作品在锚点周围铺开。
pub fn tangent_basis(u: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    let up = [0.0, 1.0, 0.0];
    let mut t = cross3(up, u);
    if t[0] * t[0] + t[1] * t[1] + t[2] * t[2] < 1e-6 {
        // u 接近极点，另选一个参考轴
        t = cross3([1.0, 0.0, 0.0], u);
    }
    let t = normalize3(t);
    let b = normalize3(cross3(u, t));
    (t, b)
}

/// 把 `&[f32]` / `&[u32]` 当字节切片喂给 GL。仅用于本模块里自己造的连续数组。
fn bytemuck_cast<T>(slice: &[T]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(
            slice.as_ptr() as *const u8,
            std::mem::size_of_val(slice),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_point_hits_poles_and_meridian() {
        let north = sphere_point(90.0, 0.0);
        assert!((north[1] - 1.0).abs() < 1e-5);
        // 本初子午线（经度 0）朝 +Z；东经 90° 朝 +X
        let prime = sphere_point(0.0, 0.0);
        assert!((prime[2] - 1.0).abs() < 1e-5);
        let east = sphere_point(0.0, 90.0);
        assert!((east[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn rotation_preserves_length() {
        let r = scene_rotation(1.2, -0.6, 0.4);
        let v = sphere_point(30.0, 45.0);
        let p = r.apply(v);
        let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        assert!((len - 1.0).abs() < 1e-5);
    }

    #[test]
    fn camera_angles_center_target_country() {
        let spin = 0.73;
        let (yaw, pitch) = camera_angles_for(35.0, 105.0, spin);
        let p = scene_rotation(spin, yaw, pitch).apply(sphere_point(35.0, 105.0));
        assert!(p[0].abs() < 1e-5);
        assert!(p[1].abs() < 1e-5);
        assert!((p[2] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn perspective_and_occlusion_match_camera_geometry() {
        assert!(perspective_factor(0.8) > perspective_factor(-0.8));
        assert_eq!(point_visibility([0.0, 0.0, 1.0]), 1.0);
        assert_eq!(point_visibility([0.0, 0.0, -1.2]), 0.0);
        assert!(point_visibility([1.5, 0.0, -0.5]) > 0.9);
    }

    #[test]
    fn identity_is_neutral() {
        let v = [0.3, -0.7, 0.5];
        assert_eq!(Mat3::identity().apply(v), v);
    }
}
