//! Lists the objects a glTF / GLB / FBX file holds, as Painter reads them. Read-only.
//!
//! ```text
//! cargo run -p painter-scene-3d --example import_probe -- model.glb
//! ```

use std::path::Path;

fn main() {
    for arg in std::env::args().skip(1) {
        let path = Path::new(&arg);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let read = match ext.as_str() {
            "fbx" => scene_3d::fbx::load(path),
            _ => scene_3d::gltf::load(path),
        };
        println!("{arg}");
        match read {
            Ok(objects) => {
                for o in objects {
                    let b = o.mesh.bounds;
                    println!(
                        "  {}: {} vertices, {} triangles, at {:?}, size {:?}, colour {:?}, image {:?} bytes",
                        o.name,
                        o.mesh.vertices.len(),
                        o.mesh.indices.len() / 3,
                        o.location,
                        b.max - b.min,
                        o.color,
                        o.image.map(|i| i.len())
                    );
                }
            }
            Err(e) => println!("  error: {e}"),
        }
    }
}
