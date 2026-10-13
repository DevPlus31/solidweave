//! A 3D preview drawn with egui's painter: an orbit camera, back faces
//! culled, flat Lambert shading, triangles sorted far to near (the painter's
//! algorithm). Fine for previewing a few thousand triangles; a GPU depth
//! buffer (wgpu) can replace it later without changing the camera.

use crate::document::{result_solids, solids_of};
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
            scene.highlighted.extend(solids_of(graph, id).iter().flat_map(|s| s.tessellate()));
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
/// fit-to-view (double click).
pub fn show(ui: &mut Ui, camera: &mut Camera, scene: &Scene) {
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
    painter.add(Shape::mesh(shaded_mesh(camera, scene, rect)));
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

/// Front-facing triangles, shaded and sorted far to near.
pub fn shaded_mesh(camera: &Camera, scene: &Scene, rect: Rect) -> egui::Mesh {
    let (right, up, forward) = camera.basis();
    let eye = camera.eye();
    // Key light from over the viewer's right shoulder.
    let light = (-forward + up * 0.6 + right * 0.4).normalize();
    let mut visible: Vec<(f64, [Pos2; 3], Color32)> = Vec::new();
    // Results in warm metal; highlighted solids in blue.
    let tinted = scene.triangles.iter().map(|t| (t, [215.0, 180.0, 140.0])).chain(scene.highlighted.iter().map(|t| (t, [110.0, 170.0, 255.0])));
    for ([a, b, c], [r, g, bl]) in tinted {
        let n = (*b - *a).cross(*c - *a).normalize();
        if n.dot(eye - *a) <= 0.0 {
            continue; // facing away
        }
        let (Some(pa), Some(pb), Some(pc)) = (camera.project(*a, rect), camera.project(*b, rect), camera.project(*c, rect)) else {
            continue;
        };
        let shade = (0.3 + 0.7 * n.dot(light).max(0.0)) as f32;
        let color = Color32::from_rgb((r * shade) as u8, (g * shade) as u8, (bl * shade) as u8);
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
    use crate::document::tests::example;

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
    fn only_front_faces_are_drawn() {
        let scene = Scene { triangles: mini_brep::Brep::make_box(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0, 1.0).tessellate(), ..Default::default() };
        let mut cam = Camera::default();
        cam.fit(&scene);
        // From a corner-on view, 3 of the 6 faces (6 of 12 triangles) face the camera.
        let mesh = shaded_mesh(&cam, &scene, screen());
        assert_eq!(mesh.indices.len() / 3, 6);
    }
}
