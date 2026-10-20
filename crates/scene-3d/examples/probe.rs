//! Dumps a `.blend` file: version, block counts, the layout of a struct, and
//! the mesh objects found. Read-only.
//!
//! ```text
//! cargo run -p painter-scene-3d --example probe -- file.blend [StructName]
//! ```

use scene_3d::blend_loader::{load_objects, BlendFile};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let file = BlendFile::open(std::path::Path::new(&args[0])).expect("open");
    println!("version {}, {} blocks, {} structs", file.version, file.blocks.len(), file.sdna.structs.len());
    if let Some(name) = args.get(1) {
        match file.sdna.struct_by_name(name) {
            Some(def) => {
                for f in &def.fields {
                    println!("  {:>5} {:>5}  {} {}", f.offset, f.size, file.sdna.type_name(f), f.name);
                }
            }
            None => println!("no struct {name}"),
        }
    }
    match load_objects(&file) {
        Ok(objects) => {
            for o in objects {
                let m = o.mesh.as_ref().unwrap();
                println!("{}: {} vertices, {} triangles, loc {:?} rot {:?} scale {:?}", o.name, m.vertices.len(), m.indices.len() / 3, o.location, o.rotation, o.scale);
            }
        }
        Err(e) => println!("error: {e}"),
    }
}
