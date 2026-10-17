//! Regenerates `assets/icon.ico` and `assets/icon.png` from the drawing code in `src/art.rs`.
//!
//!     cargo run --example make_icons

use std::fs;

fn png(size: usize) -> Vec<u8> {
    let rgba = mouse_blur::art::app_icon(size, true, false);
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, size as u32, size as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    w.write_image_data(&rgba).expect("png data");
    drop(w);
    out
}

fn main() {
    fs::create_dir_all("assets").unwrap();
    let sizes = [16usize, 20, 24, 32, 40, 48, 64, 128, 256];
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| png(s)).collect();

    // ICO container with PNG-compressed entries (supported since Windows Vista).
    let mut ico = Vec::new();
    ico.extend_from_slice(&[0, 0, 1, 0]);
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len();
    for (size, data) in sizes.iter().zip(&images) {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(data.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for data in &images {
        ico.extend_from_slice(data);
    }
    fs::write("assets/icon.ico", ico).unwrap();
    fs::write("assets/icon.png", png(256)).unwrap();
    println!("wrote assets/icon.ico and assets/icon.png");
}
