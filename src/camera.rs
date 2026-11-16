//! The camera the player moves over a board: a zoom (1 shows the board as the fit frames it)
//! and a pan over the water, kept within limits that hold the board in view. Drawing, picking,
//! the menu's labels and the cursor light all see the board through `Stage::camera`.

use glam::{Vec2, Vec3};

use crate::gfx::{self, Camera};

/// Points a world unit (one cell) may grow to at the closest zoom...
const MOST_UNIT_POINTS: f32 = 80.0;
/// ...though the closest zoom is never under this, however big the board shows at the fit.
const LEAST_MAX_ZOOM: f32 = 2.0;
/// Over this much zoom past the fit the pan opens up to its full range: at the fit the board
/// sits where the fit puts it.
const PAN_RAMP: f32 = 0.25;

/// How the player has moved the camera: `zoom` times closer than the fit, moved by `pan`
/// (world x, z) over the water.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f32,
    pub pan: Vec2,
}

impl Default for View {
    fn default() -> View {
        View { zoom: 1.0, pan: Vec2::ZERO }
    }
}

/// A board as the camera sees it: the board element's rect in window points, the
/// half-extents the fit frames at zoom 1, and the half-extents a pan may bring into view (both
/// about the world's origin).
#[derive(Clone, Copy, Debug)]
pub struct Stage {
    pub rect: [f32; 4],
    pub fit: (f32, f32),
    pub area: (f32, f32),
}

impl Stage {
    fn aspect(&self) -> f32 {
        self.rect[2].max(1.0) / self.rect[3].max(1.0)
    }

    /// The camera for `view` as it is, limits or not.
    fn raw(&self, v: View) -> Camera {
        gfx::look(self.aspect(), self.fit, v.zoom, v.pan)
    }

    /// The camera for `view`, within the limits.
    pub fn camera(&self, view: View) -> Camera {
        self.raw(self.clamp(view))
    }

    /// Window point (x, y) in the board element's normalised device coordinates.
    fn ndc(&self, x: f32, y: f32) -> Vec2 {
        let [bx, by, bw, bh] = self.rect;
        let (u, v) = ((x - bx) / bw.max(1.0), (y - by) / bh.max(1.0));
        Vec2::new(u * 2.0 - 1.0, 1.0 - v * 2.0)
    }

    /// The water point under window point (x, y) through `cam`, if it is over the board element.
    pub fn water_at(&self, cam: &Camera, x: f32, y: f32) -> Option<Vec2> {
        let [bx, by, bw, bh] = self.rect;
        let (u, v) = ((x - bx) / bw.max(1.0), (y - by) / bh.max(1.0));
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return None;
        }
        cam.water_at(Vec2::new(u * 2.0 - 1.0, 1.0 - v * 2.0))
    }

    /// The water point under window point (x, y) through the camera for `view`, wherever the
    /// point is on the window (a pinch's centre can be off the board element).
    pub fn under(&self, view: View, x: f32, y: f32) -> Option<Vec2> {
        self.camera(view).water_at(self.ndc(x, y))
    }

    /// Points one world unit spans across the middle of the view at the fit.
    pub fn unit_points(&self) -> f32 {
        let cam = self.raw(View::default());
        let c = cam.water_at(Vec2::ZERO).unwrap_or(Vec2::ZERO);
        let a = cam.project(Vec3::new(c.x - 0.5, 0.0, c.y));
        let b = cam.project(Vec3::new(c.x + 0.5, 0.0, c.y));
        (b.x - a.x) * self.rect[2].max(1.0)
    }

    /// The water the fit shows through the middle of the view: its width (x) and depth (z).
    pub fn fit_extent(&self) -> Vec2 {
        let cam = self.raw(View::default());
        let at = |x, y| cam.water_at(Vec2::new(x, y)).unwrap_or(Vec2::ZERO);
        Vec2::new(at(1.0, 0.0).x - at(-1.0, 0.0).x, at(0.0, -1.0).y - at(0.0, 1.0).y)
    }

    /// The closest zoom: until a unit spans `MOST_UNIT_POINTS`.
    pub fn max_zoom(&self) -> f32 {
        (MOST_UNIT_POINTS / self.unit_points().max(1e-3)).max(LEAST_MAX_ZOOM)
    }

    fn clamp_zoom(&self, zoom: f32) -> f32 {
        if zoom.is_finite() { zoom.clamp(1.0, self.max_zoom()) } else { 1.0 }
    }

    /// `view` within the limits: no further out than the fit, no closer than `max_zoom`, and
    /// panned no further than keeps the view inside the board's area along each axis (where the
    /// view is wider than the area, no further than shows more of it).
    pub fn clamp(&self, view: View) -> View {
        let zoom = self.clamp_zoom(view.zoom);
        let cam = self.raw(View { zoom, pan: Vec2::ZERO });
        let edge = |x, y| cam.water_at(Vec2::new(x, y));
        let (Some(l), Some(r), Some(t), Some(b)) = (edge(-1.0, 0.0), edge(1.0, 0.0), edge(0.0, 1.0), edge(0.0, -1.0)) else {
            return View { zoom, pan: Vec2::ZERO };
        };
        let ramp = ((zoom - 1.0) / PAN_RAMP).min(1.0);
        let axis = |pan: f32, lo: f32, hi: f32, area: f32| {
            let pan = if pan.is_finite() { pan } else { 0.0 };
            pan.clamp((-area - lo).min(0.0) * ramp, (area - hi).max(0.0) * ramp)
        };
        let pan = Vec2::new(axis(view.pan.x, l.x, r.x, self.area.0), axis(view.pan.y, t.y.min(b.y), t.y.max(b.y), self.area.1));
        View { zoom, pan }
    }

    /// `view` zoomed by `factor` about window point (x, y): the water point under it stays
    /// under it (unless a limit is reached).
    pub fn zoom_about(&self, view: View, x: f32, y: f32, factor: f32) -> View {
        let view = self.clamp(view);
        if !(factor.is_finite() && factor > 0.0) {
            return view;
        }
        let before = self.raw(view).water_at(self.ndc(x, y));
        let mut next = View { zoom: self.clamp_zoom(view.zoom * factor), pan: view.pan };
        // eye and target move together with the pan, so every water point moves by it exactly
        if let (Some(a), Some(b)) = (before, self.raw(next).water_at(self.ndc(x, y))) {
            next.pan += a - b;
        }
        self.clamp(next)
    }

    /// `view` panned so that water point `grab` comes under window point (x, y) (as far as
    /// the limits let it).
    pub fn drag(&self, view: View, grab: Vec2, x: f32, y: f32) -> View {
        let view = self.clamp(view);
        let Some(now) = self.raw(view).water_at(self.ndc(x, y)) else { return view };
        self.clamp(View { zoom: view.zoom, pan: view.pan + grab - now })
    }

    /// `zoom` times closer than the fit, about the middle of the board element.
    pub fn zoomed(&self, zoom: f32) -> View {
        let [bx, by, bw, bh] = self.rect;
        self.zoom_about(View::default(), bx + bw * 0.5, by + bh * 0.5, zoom)
    }

    /// `view` panned to put water point `at` in the middle of the board element.
    pub fn centred(&self, view: View, at: Vec2) -> View {
        let [bx, by, bw, bh] = self.rect;
        self.drag(view, at, bx + bw * 0.5, by + bh * 0.5)
    }
}
