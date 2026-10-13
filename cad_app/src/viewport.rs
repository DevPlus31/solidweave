//! A 3D preview: an orbit camera, back faces culled and flat Lambert shading
//! on the CPU, then drawn on the GPU through a wgpu paint callback with a
//! depth test, so faces hide each other per pixel. Without a wgpu renderer
//! it falls back to egui's painter, sorting triangles far to near (the
//! painter's algorithm), which can draw a long thin triangle over a nearer
//! face.

use crate::document::result_solids;
use eframe::egui_wgpu::{self, wgpu};
use egui::{Color32, Pos2, Rect, Sense, Shape, Stroke, Ui};
use mini_brep::Vec3;
use node_graph::{Graph, NodeId};

/// Triangles to draw: the graph's results, and the solids of the selected
/// nodes, highlighted.
#[derive(Default)]
pub struct Scene {
    pub triangles: Vec<[Vec3; 3]>,
    pub highlighted: Vec<[Vec3; 3]>,
}

impl Scene {
    /// The graph's results (solids no other geometry node consumes), plus
    /// the solids of `selected` nodes, highlighted even when they are only
    /// steps, like a boolean's cutter.
    pub fn from_graph(graph: &Graph, selected: &[NodeId]) -> Self {
        let mut scene = Scene::default();
        for (id, s) in result_solids(graph) {
            if !selected.contains(&id) {
                scene.triangles.extend(s.tessellate());
            }
        }
        for &id in selected {
            scene.highlighted.extend(graph.solids_of(id).iter().flat_map(|s| s.tessellate()));
        }
        scene
    }

    /// Centre and radius of the bounding box, if there is anything to show.
    pub fn bounds(&self) -> Option<(Vec3, f64)> {
        let mut pts = self.triangles.iter().chain(&self.highlighted).flatten();
        let first = *pts.next()?;
        let (lo, hi) = pts.fold((first, first), |(lo, hi), p| {
            (Vec3::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)), Vec3::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)))
        });
        Some(((lo + hi) * 0.5, ((hi - lo).length() * 0.5).max(1e-6)))
    }
}

/// Orbit camera round `target`, Z up. Angles in radians.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub target: Vec3,
}

/// Vertical field of view.
const FOV: f64 = 0.8;

impl Default for Camera {
    fn default() -> Self {
        Camera { yaw: -2.2, pitch: 0.6, distance: 50.0, target: Vec3::new(0.0, 0.0, 0.0) }
    }
}

impl Camera {
    pub fn eye(&self) -> Vec3 {
        let (cp, sp) = (self.pitch.cos(), self.pitch.sin());
        self.target + Vec3::new(cp * self.yaw.cos(), cp * self.yaw.sin(), sp) * self.distance
    }

    /// Unit (right, up, forward) vectors.
    fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let forward = (self.target - self.eye()).normalize();
        let right = forward.cross(Vec3::new(0.0, 0.0, 1.0)).normalize();
        (right, right.cross(forward), forward)
    }

    /// Frames the whole scene.
    pub fn fit(&mut self, scene: &Scene) {
        if let Some((center, radius)) = scene.bounds() {
            self.target = center;
            self.distance = radius / (FOV / 2.0).sin() * 1.15;
        }
    }

    /// Screen position and depth of `p` in `rect`; None if behind the camera.
    pub fn project(&self, p: Vec3, rect: Rect) -> Option<(Pos2, f64)> {
        let (right, up, forward) = self.basis();
        let d = p - self.eye();
        let depth = d.dot(forward);
        if depth < self.distance * 1e-3 {
            return None;
        }
        let focal = 0.5 * rect.height() as f64 / (FOV / 2.0).tan();
        let (x, y) = (d.dot(right) / depth * focal, d.dot(up) / depth * focal);
        Some((rect.center() + egui::vec2(x as f32, -y as f32), depth))
    }
}

/// Draws the scene and handles orbit (drag), zoom (scroll) and
/// fit-to-view (double click). `gpu` says whether [`init_gpu`] ran.
pub fn show(ui: &mut Ui, camera: &mut Camera, scene: &Scene, gpu: bool) {
    let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
    let rect = response.rect;
    if response.dragged() {
        let d = response.drag_delta();
        camera.yaw -= d.x as f64 * 0.01;
        camera.pitch = (camera.pitch + d.y as f64 * 0.01).clamp(-1.5, 1.5);
    }
    if response.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y) as f64;
        camera.distance *= (-scroll * 0.002).exp();
    }
    if response.double_clicked() {
        camera.fit(scene);
    }

    painter.rect_filled(rect, 0.0, Color32::from_rgb(32, 34, 40));
    if gpu {
        let draw = GpuDraw { matrix: view_projection(camera, scene, rect), triangles: shaded_triangles(camera, scene) };
        painter.add(egui_wgpu::Callback::new_paint_callback(rect, draw));
    } else {
        painter.add(Shape::mesh(shaded_mesh(camera, scene, rect)));
    }
    draw_axes(&painter, camera, scene, rect);
    painter.text(
        rect.left_bottom() + egui::vec2(8.0, -8.0),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "{} triangles · drag to orbit, scroll to zoom, double-click to fit · selected nodes show in blue",
            scene.triangles.len() + scene.highlighted.len()
        ),
        egui::FontId::proportional(12.0),
        Color32::GRAY,
    );
}

/// Front-facing triangles in world space, each with its shaded colour.
fn shaded_triangles(camera: &Camera, scene: &Scene) -> Vec<([Vec3; 3], Color32)> {
    let (right, up, forward) = camera.basis();
    let eye = camera.eye();
    // Key light from over the viewer's right shoulder.
    let light = (-forward + up * 0.6 + right * 0.4).normalize();
    // Results in warm metal; highlighted solids in blue.
    let tinted = scene.triangles.iter().map(|t| (t, [215.0, 180.0, 140.0])).chain(scene.highlighted.iter().map(|t| (t, [110.0, 170.0, 255.0])));
    let mut out = Vec::new();
    for (&[a, b, c], [r, g, bl]) in tinted {
        let n = (b - a).cross(c - a).normalize();
        if n.dot(eye - a) <= 0.0 {
            continue; // facing away
        }
        let shade = (0.3 + 0.7 * n.dot(light).max(0.0)) as f32;
        out.push(([a, b, c], Color32::from_rgb((r * shade) as u8, (g * shade) as u8, (bl * shade) as u8)));
    }
    out
}

/// Front-facing triangles, shaded, projected and sorted far to near: the
/// fallback when there is no GPU depth buffer.
pub fn shaded_mesh(camera: &Camera, scene: &Scene, rect: Rect) -> egui::Mesh {
    let mut visible: Vec<(f64, [Pos2; 3], Color32)> = Vec::new();
    for ([a, b, c], color) in shaded_triangles(camera, scene) {
        let (Some(pa), Some(pb), Some(pc)) = (camera.project(a, rect), camera.project(b, rect), camera.project(c, rect)) else {
            continue;
        };
        visible.push(((pa.1 + pb.1 + pc.1) / 3.0, [pa.0, pb.0, pc.0], color));
    }
    visible.sort_by(|x, y| y.0.total_cmp(&x.0));
    let mut mesh = egui::Mesh::default();
    for (_, pts, color) in visible {
        let base = mesh.vertices.len() as u32;
        for p in pts {
            mesh.colored_vertex(p, color);
        }
        mesh.add_triangle(base, base + 1, base + 2);
    }
    mesh
}

/// The camera as a column-major view-projection matrix for `rect`: x and y
/// land where [`Camera::project`] puts them, and depth runs from 0 at the
/// near plane to 1 at the far one, which are fitted round the scene.
pub fn view_projection(camera: &Camera, scene: &Scene, rect: Rect) -> [f32; 16] {
    let (right, up, forward) = camera.basis();
    let eye = camera.eye();
    let (near, far) = match scene.bounds() {
        Some((center, radius)) => {
            let mid = (center - eye).dot(forward);
            let far = (mid + radius * 1.05).max(camera.distance * 2e-3);
            ((mid - radius * 1.05).max(far * 1e-4), far)
        }
        None => (camera.distance * 1e-3, camera.distance * 2.0),
    };
    let f = 1.0 / (FOV / 2.0).tan();
    let aspect = (rect.width() / rect.height().max(1.0)) as f64;
    let (a, b) = (far / (far - near), -near * far / (far - near));
    let row = |v: Vec3, s: f64, extra: f64| [v.x * s, v.y * s, v.z * s, -v.dot(eye) * s + extra];
    let rows = [row(right, f / aspect, 0.0), row(up, f, 0.0), row(forward, a, b), row(forward, 1.0, 0.0)];
    std::array::from_fn(|i| rows[i % 4][i / 4] as f32)
}

/// Depth bits the window is created with; the 3D pass tests against them.
pub const DEPTH_BITS: u8 = 32;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Multisampling the window is created with, for smooth model edges.
pub const MSAA: u16 = 4;

const SHADER: &str = "
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec3<f32> };
@group(0) @binding(0) var<uniform> view_projection: mat4x4<f32>;
@vertex fn vs_main(@location(0) pos: vec3<f32>, @location(1) color: vec3<f32>) -> VsOut {
    return VsOut(view_projection * vec4<f32>(pos, 1.0), color);
}
@fragment fn fs_main(v: VsOut) -> @location(0) vec4<f32> { return vec4<f32>(v.color, 1.0); }
";

/// The 3D pipeline and its buffers, kept in egui-wgpu's callback resources.
struct GpuResources {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    vertex_count: u32,
    /// The window stores linear colour, so sRGB values are converted.
    linear: bool,
}

/// Bytes per vertex: position and colour, three f32 each.
const VERTEX_SIZE: u64 = 6 * 4;

fn vertex_buffer(device: &wgpu::Device, vertices: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("viewport_vertices"),
        size: vertices.max(3) * VERTEX_SIZE,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// Builds the 3D pipeline. Call once at start-up, with the window created
/// with [`DEPTH_BITS`] and [`MSAA`].
pub fn init_gpu(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewport"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let uniform = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("viewport_view_projection"),
        size: 64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("viewport"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("viewport"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("viewport"),
        bind_group_layouts: &[Some(&layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("viewport"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: VERTEX_SIZE,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        // Back faces are already dropped on the CPU.
        primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState { count: MSAA as u32, mask: !0, alpha_to_coverage_enabled: false },
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: render_state.target_format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    });
    let resources = GpuResources {
        pipeline,
        uniform,
        bind_group,
        vertices: vertex_buffer(device, 3),
        vertex_count: 0,
        linear: render_state.target_format.is_srgb(),
    };
    render_state.renderer.write().callback_resources.insert(resources);
}

/// One frame's 3D drawing, handed to egui-wgpu.
struct GpuDraw {
    matrix: [f32; 16],
    triangles: Vec<([Vec3; 3], Color32)>,
}

impl egui_wgpu::CallbackTrait for GpuDraw {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get_mut::<GpuResources>() else { return Vec::new() };
        let channel = |c: u8| {
            let c = c as f32 / 255.0;
            if gpu.linear { if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } } else { c }
        };
        let mut bytes = Vec::with_capacity(self.triangles.len() * 3 * VERTEX_SIZE as usize);
        for (pts, color) in &self.triangles {
            let rgb = [channel(color.r()), channel(color.g()), channel(color.b())];
            for p in pts {
                for x in [p.x as f32, p.y as f32, p.z as f32].into_iter().chain(rgb) {
                    bytes.extend_from_slice(&x.to_ne_bytes());
                }
            }
        }
        let count = self.triangles.len() as u64 * 3;
        if gpu.vertices.size() < count * VERTEX_SIZE {
            gpu.vertices = vertex_buffer(device, count.next_power_of_two());
        }
        if !bytes.is_empty() {
            queue.write_buffer(&gpu.vertices, 0, &bytes);
        }
        let matrix: Vec<u8> = self.matrix.iter().flat_map(|x| x.to_ne_bytes()).collect();
        queue.write_buffer(&gpu.uniform, 0, &matrix);
        gpu.vertex_count = count as u32;
        Vec::new()
    }

    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>, resources: &egui_wgpu::CallbackResources) {
        let Some(gpu) = resources.get::<GpuResources>() else { return };
        if gpu.vertex_count == 0 {
            return;
        }
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &gpu.bind_group, &[]);
        pass.set_vertex_buffer(0, gpu.vertices.slice(..));
        pass.draw(0..gpu.vertex_count, 0..1);
    }
}

fn draw_axes(painter: &egui::Painter, camera: &Camera, scene: &Scene, rect: Rect) {
    let len = scene.bounds().map_or(10.0, |(_, r)| r * 0.4);
    let origin = Vec3::new(0.0, 0.0, 0.0);
    let axes = [
        (Vec3::new(len, 0.0, 0.0), Color32::from_rgb(220, 80, 80)),
        (Vec3::new(0.0, len, 0.0), Color32::from_rgb(80, 200, 90)),
        (Vec3::new(0.0, 0.0, len), Color32::from_rgb(90, 140, 240)),
    ];
    for (tip, color) in axes {
        if let (Some((a, _)), Some((b, _))) = (camera.project(origin, rect), camera.project(tip, rect)) {
            painter.line_segment([a, b], Stroke::new(1.5, color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::example;

    fn starter_graph() -> Graph {
        example("Drilled")
    }

    fn screen() -> Rect {
        Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))
    }

    fn starter_scene() -> Scene {
        let mut g = starter_graph();
        g.evaluate();
        Scene::from_graph(&g, &[])
    }

    #[test]
    fn target_projects_to_the_centre_and_points_behind_are_dropped() {
        let cam = Camera::default();
        let (p, depth) = cam.project(cam.target, screen()).unwrap();
        assert!((p - screen().center()).length() < 1e-3);
        assert!((depth - cam.distance).abs() < 1e-9);
        let behind = cam.eye() + (cam.eye() - cam.target);
        assert!(cam.project(behind, screen()).is_none());
    }

    #[test]
    fn fit_frames_the_whole_scene() {
        let scene = starter_scene();
        assert!(!scene.triangles.is_empty());
        let mut cam = Camera::default();
        cam.fit(&scene);
        for p in scene.triangles.iter().flatten() {
            let (q, _) = cam.project(*p, screen()).unwrap();
            assert!(screen().contains(q), "{p:?} -> {q:?}");
        }
    }

    #[test]
    fn the_gpu_matrix_agrees_with_project_and_keeps_the_scene_in_depth_range() {
        let scene = starter_scene();
        let mut cam = Camera::default();
        cam.fit(&scene);
        cam.distance *= 0.6; // close in, so the near plane matters
        let rect = Rect::from_min_size(egui::pos2(300.0, 40.0), egui::vec2(700.0, 500.0));
        let m = view_projection(&cam, &scene, rect);
        for p in scene.triangles.iter().flatten() {
            let clip: [f64; 4] =
                std::array::from_fn(|r| (0..4).map(|c| m[c * 4 + r] as f64 * [p.x, p.y, p.z, 1.0][c]).sum());
            let (x, y, z) = (clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]);
            let screen = rect.center() + egui::vec2(x as f32 * rect.width() / 2.0, -y as f32 * rect.height() / 2.0);
            let (want, _) = cam.project(*p, rect).unwrap();
            assert!((screen - want).length() < 0.01, "{p:?}: {screen:?} vs {want:?}");
            assert!((0.0..1.0).contains(&z), "{p:?} has depth {z}");
        }
    }

    #[test]
    fn only_front_faces_are_drawn() {
        let scene = Scene { triangles: mini_brep::Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0, 1.0).tessellate(), ..Default::default() };
        let mut cam = Camera::default();
        cam.fit(&scene);
        // From a corner-on view, 3 of the 6 faces (6 of 12 triangles) face the camera.
        let mesh = shaded_mesh(&cam, &scene, screen());
        assert_eq!(mesh.indices.len() / 3, 6);
    }
}
