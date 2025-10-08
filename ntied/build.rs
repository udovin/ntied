use std::fs;
use std::path::Path;

use image::ImageEncoder as _;

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

    if need_png || need_ico {
        // For now, we'll create placeholder files
        // In production, you would use a proper SVG rasterizer

        if need_png && !png_path.exists() {
            // Create a simple placeholder PNG
            create_placeholder_png(png_path);
            println!("cargo::warning=Created placeholder PNG icon at assets/ntied-icon.png");
        }

        if need_ico && !ico_path.exists() {
            // Create a simple placeholder ICO
            create_placeholder_ico(ico_path);
            println!("cargo::warning=Created placeholder ICO icon at assets/ntied-icon.ico");
        }
    }

    println!("cargo::rerun-if-changed=assets/ntied-icon.ico");
    println!("cargo::rerun-if-changed=assets/ntied-icon.png");
    println!("cargo::rerun-if-changed=assets/ntied-icon.svg");
}

fn create_placeholder_png(path: &Path) {
    // Create a simple 512x512 blue gradient PNG as placeholder
    use image::{ImageBuffer, Rgba};

    let img = ImageBuffer::from_fn(512, 512, |x, y| {
        // Create a blue gradient similar to the SVG colors
        let r = (47 + (x * 48 / 512)) as u8;
        let g = (110 + (y * 58 / 512)) as u8;
        let b = (220 - ((x + y) * 35 / 1024)) as u8;
        Rgba([r, g, b, 255])
    });

    img.save(path).expect("Failed to save placeholder PNG");
}

fn create_placeholder_ico(path: &Path) {
    // Create a simple 256x256 ICO as placeholder
    use image::{ImageBuffer, Rgba};

    let img = ImageBuffer::from_fn(256, 256, |x, y| {
        // Create a blue gradient similar to the SVG colors
        let r = (47 + (x * 48 / 256)) as u8;
        let g = (110 + (y * 58 / 256)) as u8;
        let b = (220 - ((x + y) * 35 / 512)) as u8;
        Rgba([r, g, b, 255])
    });

    // Save as ICO
    let mut ico_data = Vec::new();
    {
        let encoder = image::codecs::ico::IcoEncoder::new(&mut ico_data);
        encoder
            .write_image(img.as_raw(), 256, 256, image::ColorType::Rgba8)
            .expect("Failed to encode ICO");
    }

    fs::write(path, ico_data).expect("Failed to save placeholder ICO");
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
