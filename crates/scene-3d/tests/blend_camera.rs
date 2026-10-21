//! Checks the Blender camera import against Blender itself.
//!
//! Needs scenes made by Blender with `tests/make_camera_scenes.py`, which also
//! records where Blender projects test points (`world_to_camera_view`):
//!
//! ```text
//! blender --background --factory-startup --python tests/make_camera_scenes.py -- <dir>
//! PAINTER_BLEND_CAMERA_DIR=<dir> cargo test -p painter-scene-3d --test blend_camera
//! ```
//!
//! Without the variable the test does nothing (CI has no Blender).

use scene_3d::blend_loader::{load_camera, BlendFile};
use scene_3d::camera::ViewCamera;
use scene_3d::math::Vec3;

#[test]
fn projections_match_blender() {
    let Some(dir) = std::env::var_os("PAINTER_BLEND_CAMERA_DIR") else {
        eprintln!("PAINTER_BLEND_CAMERA_DIR not set; skipped");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let expected: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("expected.json")).unwrap()).unwrap();
    for case in expected.as_array().unwrap() {
        let file = case["file"].as_str().unwrap();
        let blend = BlendFile::open(std::path::Path::new(file)).unwrap();
        let cam = load_camera(&blend).unwrap_or_else(|| panic!("{file}: no camera"));
        let (w, h) = (case["width"].as_f64().unwrap() as f32, case["height"].as_f64().unwrap() as f32);
        assert_eq!(cam.resolution, [w as u32, h as u32], "{file}: render size");
        let location = case["location"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect::<Vec<_>>();
        assert!((cam.location - Vec3::from_array([location[0], location[1], location[2]])).length() < 1e-3, "{file}: location");
        let clip = case["clip"].as_array().unwrap();
        assert!((cam.clip_start - clip[0].as_f64().unwrap() as f32).abs() < 1e-5, "{file}: clip start");
        assert!((cam.clip_end - clip[1].as_f64().unwrap() as f32).abs() < 1e-3, "{file}: clip end");

        // On a canvas the shape of the render, and on a tall A4 page (frame fitted inside, centred).
        for (cw, ch) in [(w, h), (2480.0, 3508.0)] {
            let view = ViewCamera::from_blend(&cam, cw, ch, None);
            let (fw, fh) = if cw / ch > w / h { (ch * w / h, ch) } else { (cw, cw * h / w) };
            for p in case["points"].as_array().unwrap() {
                let point = p["point"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect::<Vec<_>>();
                let frame = p["frame"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect::<Vec<_>>();
                let want = [(cw - fw) * 0.5 + frame[0] * fw, (ch - fh) * 0.5 + (1.0 - frame[1]) * fh];
                let got = view.to_screen(Vec3::from_array([point[0], point[1], point[2]]), cw, ch).unwrap();
                let err = ((got[0] - want[0]).powi(2) + (got[1] - want[1]).powi(2)).sqrt();
                assert!(err < 0.05 * (cw.max(ch) / 1000.0).max(1.0), "{file} {cw}×{ch} {point:?}: got {got:?}, Blender {want:?}");
            }
            // Vanishing points of the world axes (none for orthographic cameras).
            for (axis, vp) in [Vec3::X, Vec3::Y, Vec3::Z].into_iter().zip(case["vanishing"].as_array().unwrap()) {
                let got = view.vanishing_point(axis, cw, ch);
                match vp.as_array() {
                    None => assert!(got.is_none(), "{file}: {axis:?} shouldn't converge, got {got:?}"),
                    Some(frame) => {
                        let frame: Vec<f32> = frame.iter().map(|v| v.as_f64().unwrap() as f32).collect();
                        let want = [(cw - fw) * 0.5 + frame[0] * fw, (ch - fh) * 0.5 + (1.0 - frame[1]) * fh];
                        let Some(got) = got else {
                            // Only beyond the reach the ruler uses.
                            assert!(
                                want[0].abs().max(want[1].abs()) > 20.0 * cw.max(ch),
                                "{file}: {axis:?} missing, Blender {want:?}"
                            );
                            continue;
                        };
                        let err = ((got[0] - want[0]).powi(2) + (got[1] - want[1]).powi(2)).sqrt();
                        let size = (want[0] - cw * 0.5).hypot(want[1] - ch * 0.5).max(cw.max(ch));
                        assert!(err < 1e-3 * size, "{file} {cw}×{ch} vanishing {axis:?}: got {got:?}, Blender {want:?}");
                    }
                }
            }
        }
    }
}
