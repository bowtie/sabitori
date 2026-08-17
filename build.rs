use std::fs::File;
use std::path::PathBuf;

use resvg::tiny_skia::Pixmap;

fn main() {
    // Rerun when the SVG source files change.
    println!("cargo:rerun-if-changed=assets/icons/mouse.svg");
    println!("cargo:rerun-if-changed=assets/icons/mouse-off.svg");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    // Render each theme/mode combination into a multi-resolution .ico.
    // Direction Lock uses the plain mouse; Off uses the crossed-out variant.
    let stroke_colors = [("light", "#ffffff"), ("dark", "#1a1a1a")];
    let mode_svg = [
        ("on", "assets/icons/mouse.svg"),
        ("off", "assets/icons/mouse-off.svg"),
    ];
    let sizes = [16, 20, 24, 32, 48, 64];

    for (theme, stroke) in &stroke_colors {
        for (mode, svg_path) in &mode_svg {
            let svg_src = std::fs::read_to_string(svg_path)
                .unwrap_or_else(|e| panic!("failed to read {svg_path}: {e}"));
            // Replace the source stroke color with the theme color.
            let svg = svg_src.replace("stroke=\"#000\"", &format!("stroke=\"{}\"", stroke));

            let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);

            for &size in &sizes {
                let rgba = render_svg_to_rgba(&svg, size);
                let image = ico::IconImage::from_rgba_data(size, size, rgba);
                icon_dir
                    .add_entry(ico::IconDirEntry::encode(&image).unwrap());
            }

            let ico_path = out_dir.join(format!("tray-{}-{}.ico", theme, mode));
            let mut ico_file = File::create(&ico_path)
                .unwrap_or_else(|e| panic!("failed to create {}: {e}", ico_path.display()));
            icon_dir
                .write(&mut ico_file)
                .unwrap_or_else(|e| panic!("failed to write {}: {e}", ico_path.display()));
        }
    }
}

fn render_svg_to_rgba(svg: &str, size: u32) -> Vec<u8> {
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &opts)
        .unwrap_or_else(|e| panic!("failed to parse SVG: {e}"));

    let mut pixmap = Pixmap::new(size, size).expect("failed to allocate pixmap");

    // Fit the SVG's painted bounds into the output with zero margin so the
    // icon is as large as possible and no outer padding is left.
    let transform = {
        const MARGIN: f32 = 0.0;
        let bbox = tree.root().stroke_bounding_box();
        let content_w = bbox.width();
        let content_h = bbox.height();

        if content_w > 0.0 && content_h > 0.0 {
            let target = size as f32 - 2.0 * MARGIN;
            let scale = target / content_w.max(content_h);
            let cx = (bbox.left() + bbox.right()) / 2.0;
            let cy = (bbox.top() + bbox.bottom()) / 2.0;
            let tx = (size as f32 / 2.0) / scale - cx;
            let ty = (size as f32 / 2.0) / scale - cy;
            resvg::tiny_skia::Transform::from_scale(scale, scale).pre_translate(tx, ty)
        } else {
            let scale = size as f32 / tree.size().width().max(tree.size().height());
            resvg::tiny_skia::Transform::from_scale(scale, scale)
        }
    };

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // Demultiply the pixmap into straight-alpha RGBA for the ICO encoder.
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let a = p.alpha();
            if a == 0 {
                [0u8, 0, 0, 0]
            } else {
                let un = |c: u8| ((u16::from(c) * 255) / u16::from(a)).min(255) as u8;
                [un(p.red()), un(p.green()), un(p.blue()), a]
            }
        })
        .collect()
}
