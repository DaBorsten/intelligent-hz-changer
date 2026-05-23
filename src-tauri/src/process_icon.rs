use base64::Engine;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
use windows::Win32::{
    Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    },
    UI::Shell::ExtractIconExW,
    UI::WindowsAndMessaging::{
        DestroyIcon, GetIconInfo, HICON, ICONINFO,
    },
};

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Extract the first icon from an exe path and return it as a PNG base64 string.
pub fn extract_icon_base64(exe_path: &str) -> Option<String> {
    unsafe { extract_icon_base64_inner(exe_path) }
}

#[cfg(windows)]
unsafe fn extract_icon_base64_inner(exe_path: &str) -> Option<String> {
    let path_wide = wide(exe_path);
    let mut large: HICON = HICON::default();
    let mut small: HICON = HICON::default();

    let count = ExtractIconExW(
        windows::core::PCWSTR(path_wide.as_ptr()),
        0,
        Some(&mut large),
        Some(&mut small),
        1,
    );
    if count == 0 {
        return None;
    }

    let icon = if !large.is_invalid() { large } else { small };
    if icon.is_invalid() {
        return None;
    }

    let result = icon_to_png_base64(icon);

    if !large.is_invalid() { let _ = DestroyIcon(large); }
    if !small.is_invalid() { let _ = DestroyIcon(small); }

    result
}

#[cfg(not(windows))]
unsafe fn extract_icon_base64_inner(_exe_path: &str) -> Option<String> {
    None
}

#[cfg(windows)]
unsafe fn icon_to_png_base64(icon: HICON) -> Option<String> {
    let mut info = ICONINFO::default();
    GetIconInfo(icon, &mut info).ok()?;

    let hbm_color = HBITMAP(info.hbmColor.0);
    let hbm_mask = HBITMAP(info.hbmMask.0);

    let hdc = CreateCompatibleDC(None);
    let old = SelectObject(hdc, hbm_color);

    let mut bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: 32,
            biHeight: -32, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [Default::default()],
    };

    let mut pixels = vec![0u8; 32 * 32 * 4];
    let lines = GetDIBits(
        hdc,
        hbm_color,
        0,
        32,
        Some(pixels.as_mut_ptr() as *mut _),
        &mut bmi,
        DIB_RGB_COLORS,
    );

    SelectObject(hdc, old);
    let _ = DeleteDC(hdc);
    let _ = DeleteObject(hbm_color);
    let _ = DeleteObject(hbm_mask);

    if lines == 0 {
        return None;
    }

    // Windows returns BGRA, convert to RGBA
    for chunk in pixels.chunks_mut(4) {
        chunk.swap(0, 2);
    }

    // Encode as PNG using a simple approach via image crate — but we don't have it.
    // Instead, encode as raw RGBA and wrap in a minimal PNG manually.
    let png_bytes = encode_rgba_as_png(&pixels, 32, 32);
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
    ))
}

/// Minimal PNG encoder for RGBA 32x32 images (no external crate needed).
fn encode_rgba_as_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {

    fn adler32(data: &[u8]) -> u32 {
        let mut s1: u32 = 1;
        let mut s2: u32 = 0;
        for &b in data {
            s1 = (s1 + b as u32) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        (s2 << 16) | s1
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc: u32 = 0xFFFF_FFFF;
        for &b in data {
            let mut v = crc ^ (b as u32);
            for _ in 0..8 {
                if v & 1 != 0 { v = (v >> 1) ^ 0xEDB8_8320; } else { v >>= 1; }
            }
            crc = v;
        }
        crc ^ 0xFFFF_FFFF
    }

    fn write_chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let mut crc_data = Vec::with_capacity(4 + data.len());
        crc_data.extend_from_slice(tag);
        crc_data.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_data).to_be_bytes());
    }

    let mut out = Vec::new();
    // PNG signature
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    // IHDR
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);  // bit depth
    ihdr.push(6);  // color type: RGBA
    ihdr.push(0);  // compression
    ihdr.push(0);  // filter
    ihdr.push(0);  // interlace
    write_chunk(&mut out, b"IHDR", &ihdr);

    // Build raw scanlines with filter byte 0
    let mut raw = Vec::with_capacity((width * 4 + 1) as usize * height as usize);
    for row in 0..height as usize {
        raw.push(0); // filter type None
        raw.extend_from_slice(&rgba[row * width as usize * 4..(row + 1) * width as usize * 4]);
    }

    // Deflate (uncompressed blocks, max 65535 bytes per block)
    let mut deflated = Vec::new();
    deflated.push(0x78); // zlib CMF
    deflated.push(0x01); // zlib FLG (no dict, check bits)
    let chunks = raw.chunks(65535);
    let total = chunks.len();
    for (i, chunk) in raw.chunks(65535).enumerate() {
        let last = i == total - 1;
        deflated.push(if last { 1 } else { 0 });
        let len = chunk.len() as u16;
        deflated.extend_from_slice(&len.to_le_bytes());
        deflated.extend_from_slice(&(!len).to_le_bytes());
        deflated.extend_from_slice(chunk);
    }
    let checksum = adler32(&raw);
    deflated.extend_from_slice(&checksum.to_be_bytes());

    write_chunk(&mut out, b"IDAT", &deflated);
    write_chunk(&mut out, b"IEND", b"");
    out
}
