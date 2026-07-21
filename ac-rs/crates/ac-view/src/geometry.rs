//! The one affine map (handoff's "structural rule" section): scene
//! `[0,1]²` coordinates → screen pixel coordinates. This is the whole
//! of `ac-view`'s numeric job and the exact line the project's oldest
//! bug class (the Ember Y-mirror bug) lived on: screen y grows
//! *downward*, scene y grows *upward* (`ac-scene`'s orientation rule).
//! No other file in this crate should need to reason about that flip.

/// A screen-space rectangle, in whatever coordinate system the caller's
/// windowing toolkit uses (top-left origin, y grows down — this is the
/// only file that needs to know that).
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Map a scene-space point (`x=0` left/low-freq, `y=0` bottom/low-level,
/// per `ac-scene`'s orientation contract) to a screen-space point
/// within `viewport`. The only place `1.0 - y` (the sign flip) may
/// appear in this crate.
pub fn scene_to_screen(scene_pt: (f64, f64), viewport: Viewport) -> (f32, f32) {
    let (sx, sy) = scene_pt;
    let screen_x = viewport.x + (sx as f32) * viewport.width;
    // Scene y=0 is the bottom (low level); screen y=0 is the top.
    // Flipping here is the entire point of this module.
    let screen_y = viewport.y + (1.0 - sy as f32) * viewport.height;
    (screen_x, screen_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport {
            x: 100.0,
            y: 50.0,
            width: 400.0,
            height: 200.0,
        }
    }

    // AC2 (the geometry test, mutation-verified at birth): larger scene
    // y (higher level) must map to a *smaller* screen y (higher on
    // screen); larger scene x must map to a *larger* screen x.
    #[test]
    fn orientation_higher_scene_y_is_smaller_screen_y_higher_scene_x_is_larger_screen_x() {
        let low_level = scene_to_screen((0.2, 0.1), vp());
        let high_level = scene_to_screen((0.2, 0.9), vp());
        assert!(
            high_level.1 < low_level.1,
            "higher scene y (louder) must render higher on screen (smaller screen y): \
             low_level={low_level:?} high_level={high_level:?}"
        );

        let low_freq = scene_to_screen((0.1, 0.5), vp());
        let high_freq = scene_to_screen((0.9, 0.5), vp());
        assert!(
            high_freq.0 > low_freq.0,
            "higher scene x (higher freq) must render further right: \
             low_freq={low_freq:?} high_freq={high_freq:?}"
        );
    }

    #[test]
    fn corners_land_exactly_on_viewport_corners() {
        let v = vp();
        let bottom_left = scene_to_screen((0.0, 0.0), v);
        assert!((bottom_left.0 - v.x).abs() < 1e-4);
        assert!((bottom_left.1 - (v.y + v.height)).abs() < 1e-4);

        let top_right = scene_to_screen((1.0, 1.0), v);
        assert!((top_right.0 - (v.x + v.width)).abs() < 1e-4);
        assert!((top_right.1 - v.y).abs() < 1e-4);
    }
}
