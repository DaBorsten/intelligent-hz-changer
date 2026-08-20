#[cfg(windows)]
use base64::Engine;

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

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Extract the first icon from an exe path and return it as a PNG base64 string.
#[cfg(windows)]
pub fn extract_icon_base64(exe_path: &str) -> Option<String> {
    unsafe { extract_icon_base64_inner(exe_path) }
}

/// Linux binaries carry no embedded icon, so the icon has to be looked up
/// indirectly: find the freedesktop `.desktop` entry that launches this
/// executable, then resolve its `Icon=` key against the XDG icon directories.
/// Returns a `data:` URL, or `None` for anything without a desktop entry
/// (Steam/Proton titles, plain binaries) — the UI falls back to its placeholder.
#[cfg(target_os = "linux")]
pub fn lookup_icon_base64(process_name: &str, exe_path: Option<&str>) -> Option<String> {
    let entry = linux::find_desktop_entry(process_name, exe_path)?;
    let icon = linux::desktop_key(&entry, "Icon")?;
    let path = linux::resolve_icon_path(&icon)?;
    linux::encode_icon_file(&path)
}

#[cfg(target_os = "linux")]
mod linux {
    use base64::Engine;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Icon files are inlined into the webview as data URLs, so refuse anything
    /// large enough to bloat the payload — real app icons are a few KB.
    const MAX_ICON_BYTES: u64 = 1024 * 1024;

    /// Preferred icon sizes, best first. `scalable` (SVG) renders sharp at any
    /// size the UI picks, so it leads.
    const ICON_SIZES: [&str; 7] = [
        "scalable", "128x128", "96x96", "64x64", "48x48", "256x256", "32x32",
    ];

    /// XDG base data directories, most specific first, with the Flatpak export
    /// roots appended so Flatpak apps' entries and icons are found too.
    fn data_dirs() -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut dirs = Vec::new();

        if let Some(dir) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            dirs.push(dir);
        } else if let Some(home) = &home {
            dirs.push(home.join(".local/share"));
        }

        let system = std::env::var("XDG_DATA_DIRS")
            .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
        dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));

        if let Some(home) = &home {
            dirs.push(home.join(".local/share/flatpak/exports/share"));
        }
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));

        dirs.dedup();
        dirs
    }

    /// Reads one key from a desktop entry's `[Desktop Entry]` group. Parsing
    /// stops at the next group header so `[Desktop Action …]` blocks — which
    /// repeat `Exec`/`Icon` for their own alternate launchers — can't be
    /// mistaken for the entry's own values.
    pub fn desktop_key(path: &Path, key: &str) -> Option<String> {
        let content = std::fs::read_to_string(path).ok()?;
        let mut in_entry = false;
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some(value) = line.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
        None
    }

    /// The executable a desktop entry actually launches. `Exec` values carry
    /// field codes (`%U`, `%f`) and often an `env VAR=… ` prefix, so the first
    /// token that is neither is taken as the program.
    fn exec_program(exec: &str) -> Option<String> {
        for token in exec.split_whitespace() {
            if token == "env" || token.contains('=') || token.starts_with('%') {
                continue;
            }
            return Path::new(token.trim_matches('"'))
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
        }
        None
    }

    /// Desktop entries keyed by the executable they launch, plus a weaker index
    /// on the entry's own filename (`steam.desktop` for `steam`, including
    /// reverse-DNS ids like `org.kde.okular`).
    struct DesktopIndex {
        by_exec: HashMap<String, PathBuf>,
        by_stem: HashMap<String, PathBuf>,
    }

    /// Opening the process picker resolves an icon per running process, and
    /// every one of those would otherwise re-read a few hundred desktop files.
    /// Building the index once and reusing it collapses that into a single
    /// scan; the TTL is only there to notice newly installed applications.
    static INDEX: Mutex<Option<(Instant, Arc<DesktopIndex>)>> = Mutex::new(None);
    const INDEX_TTL: Duration = Duration::from_secs(60);

    fn index() -> Arc<DesktopIndex> {
        if let Ok(guard) = INDEX.lock() {
            if let Some((built, cached)) = guard.as_ref() {
                if built.elapsed() < INDEX_TTL {
                    return Arc::clone(cached);
                }
            }
        }

        let mut by_exec = HashMap::new();
        let mut by_stem = HashMap::new();
        // Most specific directory first, and first writer wins, so a user's own
        // entry shadows the system one for the same program.
        for dir in data_dirs() {
            for entry in desktop_files(&dir.join("applications")) {
                for key in ["Exec", "TryExec"] {
                    if let Some(prog) = desktop_key(&entry, key).and_then(|v| exec_program(&v)) {
                        by_exec.entry(prog.to_lowercase()).or_insert(entry.clone());
                    }
                }
                let Some(stem) = entry.file_stem().map(|s| s.to_string_lossy().to_lowercase())
                else {
                    continue;
                };
                if let Some(last) = stem.rsplit('.').next() {
                    by_stem.entry(last.to_string()).or_insert(entry.clone());
                }
                by_stem.entry(stem).or_insert(entry);
            }
        }

        let fresh = Arc::new(DesktopIndex { by_exec, by_stem });
        if let Ok(mut guard) = INDEX.lock() {
            *guard = Some((Instant::now(), Arc::clone(&fresh)));
        }
        fresh
    }

    /// Finds the desktop entry launching `process_name` (or the binary at
    /// `exe_path`). The `Exec` match is authoritative; the entry filename is
    /// only consulted when nothing launches the binary by that name.
    pub fn find_desktop_entry(process_name: &str, exe_path: Option<&str>) -> Option<PathBuf> {
        let mut wanted = vec![process_name.to_lowercase()];
        if let Some(base) = exe_path
            .map(Path::new)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_lowercase())
        {
            if !wanted.contains(&base) {
                wanted.push(base);
            }
        }

        let index = index();
        wanted
            .iter()
            .find_map(|w| index.by_exec.get(w))
            .or_else(|| wanted.iter().find_map(|w| index.by_stem.get(w)))
            .cloned()
    }

    /// Desktop entries in `dir`, including one level of subdirectories (some
    /// distros group entries into `applications/kde4/` and similar).
    fn desktop_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "desktop") {
                files.push(path);
            } else if path.is_dir() {
                if let Ok(nested) = std::fs::read_dir(&path) {
                    files.extend(
                        nested
                            .flatten()
                            .map(|e| e.path())
                            .filter(|p| p.extension().is_some_and(|e| e == "desktop")),
                    );
                }
            }
        }
        files
    }

    /// Resolves an `Icon=` value — either an absolute path or a themed icon
    /// name — to a file on disk. Themes are searched with `hicolor` (the
    /// freedesktop fallback theme every app installs into) first, then
    /// `pixmaps` for older apps that never adopted the theme layout.
    pub fn resolve_icon_path(icon: &str) -> Option<PathBuf> {
        if icon.starts_with('/') {
            let path = PathBuf::from(icon);
            return path.is_file().then_some(path);
        }

        for dir in data_dirs() {
            let icons = dir.join("icons");
            let mut themes = vec![icons.join("hicolor")];
            if let Ok(entries) = std::fs::read_dir(&icons) {
                themes.extend(
                    entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir() && !p.ends_with("hicolor")),
                );
            }
            for theme in themes {
                for size in ICON_SIZES {
                    for ext in ["svg", "png"] {
                        let candidate = theme.join(size).join("apps").join(format!("{icon}.{ext}"));
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                }
            }
            for ext in ["png", "svg"] {
                let candidate = dir.join("pixmaps").join(format!("{icon}.{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Reads an icon file into a `data:` URL. XPM and anything else the webview
    /// can't render is rejected rather than shipped as a broken `<img>`.
    pub fn encode_icon_file(path: &Path) -> Option<String> {
        let mime = match path.extension()?.to_string_lossy().to_lowercase().as_str() {
            "png" => "image/png",
            "svg" => "image/svg+xml",
            "jpg" | "jpeg" => "image/jpeg",
            _ => return None,
        };
        if std::fs::metadata(path).ok()?.len() > MAX_ICON_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ))
    }
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
#[cfg(windows)]
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


