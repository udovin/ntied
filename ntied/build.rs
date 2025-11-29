use image::ImageEncoder as _;
use std::fs;
use std::path::Path;
use usvg::TreeParsing;

fn main() {
    // Convert SVG to PNG and ICO if needed
    generate_icon_files();
    // Only run icon embedding on Windows
    #[cfg(target_os = "windows")]
    embed_windows_icon();
}

fn generate_icon_files() {
    let svg_path = Path::new("assets/ntied-icon.svg");
    let png_path = Path::new("assets/ntied-icon.png");
    let ico_path = Path::new("assets/ntied-icon.ico");
    // Only process if SVG exists
    if !svg_path.exists() {
        println!("cargo::error=SVG icon not found at assets/ntied-icon.svg");
        return;
    }
    // Check if we need to regenerate based on timestamps
    let svg_metadata = fs::metadata(svg_path).unwrap();
    let svg_modified = svg_metadata.modified().unwrap();
    let need_png = !png_path.exists() || {
        let png_modified = fs::metadata(png_path).unwrap().modified().unwrap();
        svg_modified > png_modified
    };
    let need_ico = !ico_path.exists() || {
        let ico_modified = fs::metadata(ico_path).unwrap().modified().unwrap();
        svg_modified > ico_modified
    };
    if need_png {
        render_icon_png(png_path, svg_path);
        println!("Created placeholder PNG icon at assets/ntied-icon.png");
    }
    if need_ico {
        render_icon_ico(ico_path, svg_path);
        println!("Created placeholder ICO icon at assets/ntied-icon.ico");
    }
    println!("cargo::rerun-if-changed=assets/ntied-icon.ico");
    println!("cargo::rerun-if-changed=assets/ntied-icon.png");
    println!("cargo::rerun-if-changed=assets/ntied-icon.svg");
}

fn render_icon_png(path: &Path, svg_path: &Path) {
    // Render SVG to PNG using resvg
    let svg_data = fs::read(svg_path).expect("Failed to read SVG file");
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(&svg_data, &opt).expect("Failed to parse SVG");
    // Create pixmap at 512x512
    let mut pixmap = tiny_skia::Pixmap::new(512, 512).unwrap();
    // Calculate scale to fit 512x512
    let tree_size = tree.size;
    let scale_x = 512.0 / tree_size.width();
    let scale_y = 512.0 / tree_size.height();
    let scale = f32::min(scale_x, scale_y);
    // Render SVG to pixmap using resvg Tree
    let rtree = resvg::Tree::from_usvg(&tree);
    rtree.render(
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    // Save pixmap as PNG
    pixmap.save_png(path).expect("Failed to save PNG");
}

fn render_icon_ico(path: &Path, svg_path: &Path) {
    // Render SVG to multiple sizes and create ICO
    let svg_data = fs::read(svg_path).expect("Failed to read SVG file");
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(&svg_data, &opt).expect("Failed to parse SVG");
    // Create resvg Tree once
    let rtree = resvg::Tree::from_usvg(&tree);
    // Render at different sizes for ICO (16, 32, 48, 256)
    let sizes = [256, 48, 32, 16];
    let mut images = Vec::new();
    for size in sizes {
        let mut pixmap = tiny_skia::Pixmap::new(size, size).unwrap();
        // Calculate scale
        let tree_size = tree.size;
        let scale = size as f32 / f32::max(tree_size.width(), tree_size.height());
        // Render SVG to pixmap
        rtree.render(
            tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        // Convert pixmap to image::RgbaImage
        let data = pixmap.data();
        let mut rgba_data = Vec::with_capacity(data.len());
        // Convert from BGRA to RGBA
        for chunk in data.chunks_exact(4) {
            rgba_data.push(chunk[0]); // R
            rgba_data.push(chunk[1]); // G
            rgba_data.push(chunk[2]); // B
            rgba_data.push(chunk[3]); // A
        }
        let img =
            image::RgbaImage::from_raw(size, size, rgba_data).expect("Failed to create image");
        images.push(image::DynamicImage::ImageRgba8(img));
    }
    // Create ICO with the largest image (Windows will scale as needed)
    let mut ico_data = Vec::new();
    {
        let encoder = image::codecs::ico::IcoEncoder::new(&mut ico_data);
        let img = images.first().unwrap();
        encoder
            .write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                image::ColorType::Rgba8.into(),
            )
            .expect("Failed to encode ICO");
    }
    fs::write(path, ico_data).expect("Failed to save ICO");
}

#[cfg(target_os = "windows")]
fn embed_windows_icon() {
    use winres::WindowsResource;
    // Set icon from .ico file if it exists
    let ico_path = Path::new("assets/ntied-icon.ico");
    if ico_path.exists() {
        let mut res = WindowsResource::new();
        res.set_icon("assets/ntied-icon.ico");
        // Set additional application metadata
        res.set("ProductName", "NTied");
        res.set("FileDescription", "NTied - Secure Communications");
        res.set("LegalCopyright", "Copyright (c) 2024");
        // Compile the resource
        if let Err(e) = res.compile() {
            println!("cargo:warning=Failed to compile Windows resources: {}", e);
        }
    } else {
        println!("cargo:warning=ICO file not found, skipping Windows icon embedding");
    }
}
