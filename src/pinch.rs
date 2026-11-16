//! Two fingers on a touch screen. Rune at the revision the game pins reads only the first
//! finger of a touch and has no pinch for it, so the page tracks every finger itself
//! (`MINES_TOUCH` in web/index.html) and the app reads that here, every frame and before each
//! press event it handles.

use glam::Vec2;

/// The page's fingers as of the last event it heard.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Touches {
    /// Fingers on the screen.
    pub fingers: u32,
    /// Changes whenever a finger lands or lifts. Centre and spread jump then, so a reading is
    /// only compared with one of the same generation.
    pub generation: u32,
    /// The middle of the first two fingers and how far apart they are, in window points.
    pub centre: Vec2,
    pub spread: f32,
    /// A second finger landed since the touch began: the first finger's press belongs to a
    /// pinch, not to a tap or a hold. Cleared when a fresh touch begins.
    pub pinched: bool,
}

/// What the page knows of the fingers now (nothing outside a browser).
pub fn read() -> Touches {
    #[cfg(target_arch = "wasm32")]
    {
        use js_sys::Reflect;
        use js_sys::wasm_bindgen::JsValue;
        let Ok(page) = Reflect::get(&js_sys::global(), &JsValue::from_str("MINES_TOUCH")) else { return Touches::default() };
        if !page.is_object() {
            return Touches::default();
        }
        let field = |key: &str| Reflect::get(&page, &JsValue::from_str(key)).ok();
        let num = |key: &str| field(key).and_then(|v| v.as_f64()).unwrap_or(0.0);
        Touches {
            fingers: num("fingers") as u32,
            generation: num("generation") as u32,
            centre: Vec2::new(num("x") as f32, num("y") as f32),
            spread: num("spread") as f32,
            pinched: field("pinched").and_then(|v| v.as_bool()).unwrap_or(false),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        Touches::default()
    }
}
