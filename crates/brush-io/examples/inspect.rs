//! Prints what Painter imports from a brush file.
//!
//! ```text
//! cargo run -p painter-brush-io --example inspect -- brushes.abr [--cached] [--extras KEY,KEY]
//! ```
//! `--cached` goes through the on-disk brush cache, like the app.
//! `--extras` adds the listed raw source parameters (e.g. `sut.TextureScale2`) to each line.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: inspect <file.sut|file.abr|file.rustbrush|tools.todb> [--cached] [--extras KEY,KEY]");
        std::process::exit(2);
    };
    let cached = args.iter().any(|a| a == "--cached");
    let extra_keys: Vec<&str> = args
        .iter()
        .position(|a| a == "--extras")
        .and_then(|i| args.get(i + 1))
        .map_or(Vec::new(), |keys| keys.split(',').collect());
    let path = std::path::Path::new(path);
    let started = std::time::Instant::now();
    let report = if cached {
        brush_io::cache::import_cached(path, &brush_io::cache::default_dir())
    } else {
        brush_io::import_report(path)
    };
    let report = match report {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "{} presets in {:.0} ms{}",
        report.presets.len(),
        started.elapsed().as_secs_f64() * 1000.0,
        if report.from_cache { " (from cache)" } else { "" }
    );
    for p in &report.presets {
        let s = &p.settings;
        let mut notes = Vec::new();
        if s.mode == painter_core::BrushMode::Erase {
            notes.push("ERASE".to_string());
        }
        if s.pressure_size {
            notes.push("size←pressure".into());
        }
        if s.pressure_density {
            notes.push("flow←pressure".into());
        }
        if let Some(t) = &p.tip {
            let source = p.extras.get("sut.tip_source").and_then(|v| v.as_str()).unwrap_or("bitmap");
            notes.push(format!("tip {}×{} ({source})", t.width, t.height));
        }
        if let Some(t) = &p.texture {
            notes.push(format!(
                "texture {}×{} {:.0}% density {:.0}% {:?}{}",
                t.image.width,
                t.image.height,
                t.scale,
                t.density * 100.0,
                t.mode,
                if t.each_dab { "" } else { " (whole stroke)" }
            ));
        }
        if let Some(d) = &p.dual {
            notes.push(format!("dual {}×{} ×{:.2} {:?}", d.tip.width, d.tip.height, d.scale, d.mode));
        }
        if s.blend != painter_core::BrushBlend::Normal {
            notes.push(format!("blend {}", s.blend.name()));
        }
        if s.scatter.is_on() {
            notes.push(format!("scatter {:.2}×{}", s.scatter.amount, s.scatter.count));
        }
        if s.taper_start > 0.0 || s.taper_end > 0.0 {
            notes.push(format!("taper {:.0}/{:.0}px", s.taper_start, s.taper_end));
        }
        if s.color_jitter.is_on() {
            notes.push(format!("colour jitter{}", if s.color_jitter.per_dab { " per dab" } else { "" }));
        }
        let dynamic = |name: &str, d: &painter_core::brush::InputDynamics| {
            let mut parts = Vec::new();
            if d.pressure.is_some() {
                parts.push("pressure");
            }
            if d.tilt.is_some() {
                parts.push("tilt");
            }
            if d.velocity.is_some() {
                parts.push("speed");
            }
            if d.jitter > 0.0 {
                parts.push("random");
            }
            if d.fade > 0 {
                parts.push("fade");
            }
            (!parts.is_empty()).then(|| format!("{name}←{}", parts.join("+")))
        };
        notes.extend(dynamic("size", &s.size_dynamics));
        notes.extend(dynamic("opacity", &s.opacity_dynamics));
        notes.extend(dynamic("thickness", &s.thickness_dynamics));
        for (on, what) in [(s.water_edge.is_some(), "water edge"), (s.noise, "noise"), (s.build_up, "build-up")] {
            if on {
                notes.push(what.into());
            }
        }
        let unsupported = p.unsupported_features();
        if !unsupported.is_empty() {
            notes.push(format!("[not applied: {}]", unsupported.join(", ")));
        }
        for key in &extra_keys {
            if let Some(value) = p.extras.get(*key) {
                notes.push(format!("{key}={value}"));
            }
        }
        println!(
            "{:28} {:>6.1}px op {:>3.0}% flow {:>3.0}% hard {:>3.0}% space {:>3.0}% {}",
            s.name,
            s.size,
            s.opacity * 100.0,
            s.density * 100.0,
            s.hardness * 100.0,
            s.spacing * 100.0,
            notes.join(" "),
        );
    }
    if !report.warnings.is_empty() {
        println!("\n{} fallbacks:", report.warnings.len());
        for w in &report.warnings {
            println!("  - {w}");
        }
    }
}
