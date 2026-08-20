use crate::display::MonitorInfoExtended;

pub fn show_overlays(app: &tauri::AppHandle, monitors: Vec<MonitorInfoExtended>, is_light: bool) {
    let _ = app;
    if monitors.is_empty() {
        return;
    }
    // Group monitors by position to detect duplicated/cloned displays
    let mut pos_map: std::collections::HashMap<(i32, i32), Vec<usize>> =
        std::collections::HashMap::new();
    for (i, mon) in monitors.iter().enumerate() {
        pos_map.entry((mon.x, mon.y)).or_default().push(i + 1);
    }
    // Build label for each monitor: "1" normally, "1|2" when cloned
    let labels: Vec<String> = monitors
        .iter()
        .enumerate()
        .map(|(i, mon)| {
            let group = pos_map.get(&(mon.x, mon.y)).unwrap();
            if group.len() > 1 {
                group.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("|")
            } else {
                (i + 1).to_string()
            }
        })
        .collect();
    #[cfg(windows)]
    std::thread::spawn(move || unsafe {
        inner::run(monitors, labels, is_light);
    });

    #[cfg(target_os = "linux")]
    gtk_overlay::show(app, monitors, labels, is_light);
}

/// Argument that puts a second copy of this binary into overlay mode, see
/// [`gtk_overlay`].
#[cfg(target_os = "linux")]
pub const OVERLAY_ARG: &str = "--hz-identify-overlay";

/// Runs the overlay child process and returns once the badges are gone. Called
/// from `main` before Tauri starts, so the child never becomes a second app
/// instance.
#[cfg(target_os = "linux")]
pub fn run_overlay_process(payload: &str) {
    gtk_overlay::run_child(payload);
}

#[cfg(windows)]
mod inner {
    use super::MonitorInfoExtended;
    use std::sync::atomic::{AtomicI32, Ordering};
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::PCWSTR;

    static WINDOW_COUNT: AtomicI32 = AtomicI32::new(0);

    use std::sync::Mutex;
    static LABELS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    unsafe fn scale_for_monitor(x: i32, y: i32) -> f32 {
        let hmon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut dpi_x = 96u32;
        let mut dpi_y = 96u32;
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        dpi_x as f32 / 96.0
    }

    pub unsafe fn run(monitors: Vec<MonitorInfoExtended>, labels: Vec<String>, is_light: bool) {
        let Ok(hmodule) = GetModuleHandleW(PCWSTR::null()) else {
            return;
        };
        let hinstance: HINSTANCE = hmodule.into();
        let class_name = windows::core::w!("HzIdentifyV1");

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance,
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);

        WINDOW_COUNT.store(0, Ordering::SeqCst);
        *LABELS.lock().unwrap_or_else(|e| e.into_inner()) = labels;

        for (i, mon) in monitors.iter().enumerate() {
            let scale = scale_for_monitor(mon.x, mon.y);
            let size = (380.0 * scale) as i32;
            let margin_left = (50.0 * scale) as i32;
            let margin_bottom = (47.0 * scale) as i32;
            let x = mon.x + margin_left;
            let y = mon.y + mon.height as i32 - size - margin_bottom;
            let w = size;
            let h = size;

            let Ok(hwnd) = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class_name,
                PCWSTR::null(),
                WS_POPUP,
                x, y, w, h,
                None,
                None,
                hinstance,
                None,
            ) else {
                continue;
            };

            // store index (0-based) and is_light flag packed as before
            let packed = ((is_light as isize) << 16) | (i as isize);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, packed);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetTimer(hwnd, 1, 3000, None);
            WINDOW_COUNT.fetch_add(1, Ordering::SeqCst);
        }

        if WINDOW_COUNT.load(Ordering::SeqCst) == 0 {
            return;
        }

        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_TIMER => {
                let _ = KillTimer(hwnd, 1);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let packed = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                let idx = (packed & 0xFFFF) as usize;
                let is_light = (packed >> 16) != 0;
                let label = LABELS.lock().unwrap_or_else(|e| e.into_inner()).get(idx).cloned().unwrap_or_else(|| (idx + 1).to_string());
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                let mut wr = RECT::default();
                let _ = GetClientRect(hwnd, &mut wr);
                let cw = wr.right;
                let ch = wr.bottom;

                let scale = {
                    let mut pt = POINT::default();
                    let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut pt);
                    scale_for_monitor(pt.x, pt.y)
                };

                let (bg_color, text_color, border_color) = if is_light {
                    (0x00_FF_FF_FF_u32, 0x00_11_11_11_u32, 0x00_CC_CC_CC_u32)
                } else {
                    (0x00_0E_0E_0E_u32, 0x00_FF_FF_FF_u32, 0x00_44_44_44_u32)
                };

                let pen = CreatePen(PS_SOLID, 1, COLORREF(border_color));
                let bg_brush = CreateSolidBrush(COLORREF(bg_color));
                let old_pen = SelectObject(hdc, pen);
                let old_brush = SelectObject(hdc, bg_brush);
                let _ = Rectangle(hdc, 0, 0, cw, ch);
                SelectObject(hdc, old_pen);
                SelectObject(hdc, old_brush);
                let _ = DeleteObject(pen);
                let _ = DeleteObject(bg_brush);

                let _ = SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(text_color));

                let font = CreateFontW(
                    (180.0 * scale) as i32, 0, 0, 0, 900, 0, 0, 0, 0, 0, 0, 0, 0,
                    windows::core::w!("Segoe UI"),
                );
                let old = SelectObject(hdc, font);

                let mut text: Vec<u16> = label.encode_utf16().collect();
                let mut rc = RECT { left: 0, top: 0, right: cw, bottom: ch };
                DrawTextW(
                    hdc,
                    &mut text,
                    &mut rc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );

                SelectObject(hdc, old);
                let _ = DeleteObject(font);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                let remaining = WINDOW_COUNT.fetch_sub(1, Ordering::SeqCst);
                if remaining <= 1 {
                    PostQuitMessage(0);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// The Linux overlay.
///
/// Unlike Win32 this cannot draw into the app's own process. On Wayland a
/// client may not place its windows, so the only way to put a badge on a
/// *chosen* display is `fullscreen_on_monitor` — and Mutter renders a
/// fullscreen window without its alpha channel, so the transparent parts come
/// out black (measured: a 1920x1080 fullscreen window is opaque black, the
/// same window one pixel shorter is properly see-through).
///
/// X11 has no such restriction, and XWayland is available in practically every
/// Wayland session, so the badges are drawn by a short-lived child process of
/// this same binary running against the X11 backend. GDK only ever speaks one
/// backend per process — `open_display(":0")` from the Wayland-side app
/// returns `None` — hence a process rather than a second display.
#[cfg(target_os = "linux")]
mod gtk_overlay {
    use super::MonitorInfoExtended;
    use gtk::prelude::*;
    use gtk::{gdk, glib};
    use serde::{Deserialize, Serialize};
    use std::time::Duration;

    /// Same proportions as the Win32 overlay, in logical pixels (GTK scales
    /// them for HiDPI).
    const BADGE: i32 = 380;
    const MARGIN_LEFT: i32 = 50;
    const MARGIN_BOTTOM: i32 = 47;
    const VISIBLE_MS: u64 = 3000;

    #[derive(Serialize, Deserialize)]
    struct Badge {
        label: String,
        /// Origin of the display in the compositor's logical coordinates —
        /// the key that pairs this badge with an X11 monitor.
        x: i32,
        y: i32,
        /// Fallback height for when no X11 monitor matches the origin.
        height: i32,
    }

    #[derive(Serialize, Deserialize)]
    struct Payload {
        badges: Vec<Badge>,
        is_light: bool,
    }

    pub fn show(
        app: &tauri::AppHandle,
        monitors: Vec<MonitorInfoExtended>,
        labels: Vec<String>,
        is_light: bool,
    ) {
        let badges: Vec<Badge> = monitors
            .iter()
            .enumerate()
            .map(|(i, mon)| Badge {
                label: labels.get(i).cloned().unwrap_or_else(|| (i + 1).to_string()),
                x: mon.x,
                y: mon.y,
                height: mon.height as i32,
            })
            .collect();
        let payload = Payload { badges, is_light };
        let Ok(json) = serde_json::to_string(&payload) else { return };

        if std::env::var_os("DISPLAY").is_some() {
            if spawn_child(&json).is_ok() {
                return;
            }
        }
        // No X server to fall back on: draw in-process instead. The badges then
        // land wherever the compositor puts them — one per display is still
        // more useful than nothing.
        unplaced(app, payload);
    }

    fn spawn_child(json: &str) -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        let mut child = std::process::Command::new(exe)
            .arg(super::OVERLAY_ARG)
            .arg(json)
            .env("GDK_BACKEND", "x11")
            .spawn()?;
        // Reap it, so a few identify clicks don't leave zombies behind.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }

    pub fn run_child(payload: &str) {
        let Ok(payload) = serde_json::from_str::<Payload>(payload) else { return };
        if gtk::init().is_err() {
            return;
        }
        let Some(display) = gdk::Display::default() else { return };

        let mut shown = 0;
        for badge in &payload.badges {
            let monitor = monitor_at(&display, badge.x, badge.y);
            let (x, y, height) = match &monitor {
                Some(m) => {
                    let g = m.geometry();
                    (g.x(), g.y(), g.height())
                }
                None => (badge.x, badge.y, badge.height),
            };
            let window = build(&badge.label, payload.is_light);
            window.move_(x + MARGIN_LEFT, y + height - BADGE - MARGIN_BOTTOM);
            present(&window);
            // Reassert the position: some window managers only honour it once
            // the window is mapped.
            window.move_(x + MARGIN_LEFT, y + height - BADGE - MARGIN_BOTTOM);
            shown += 1;
        }
        if shown == 0 {
            return;
        }

        glib::timeout_add_local_once(Duration::from_millis(VISIBLE_MS), || gtk::main_quit());
        gtk::main();
    }

    /// The X11 monitor whose origin matches the compositor coordinates we were
    /// given. XWayland mirrors the Wayland layout, so the origins line up; the
    /// connector names do not (XWayland invents `XWAYLAND0`-style names), which
    /// is why position is the key.
    fn monitor_at(display: &gdk::Display, x: i32, y: i32) -> Option<gdk::Monitor> {
        (0..display.n_monitors())
            .filter_map(|i| display.monitor(i))
            .find(|m| m.geometry().x() == x && m.geometry().y() == y)
    }

    /// Draws the badges in this process, for the rare Wayland session without
    /// XWayland. Cannot place them, so it also cannot use `fullscreen_on_monitor`
    /// (that is the case Mutter paints black) — the compositor decides where
    /// they go.
    fn unplaced(app: &tauri::AppHandle, payload: Payload) {
        let _ = app.run_on_main_thread(move || {
            let windows: Vec<gtk::Window> = payload
                .badges
                .iter()
                .map(|badge| {
                    let window = build(&badge.label, payload.is_light);
                    present(&window);
                    window
                })
                .collect();
            glib::timeout_add_local_once(Duration::from_millis(VISIBLE_MS), move || {
                for window in windows {
                    window.close();
                }
            });
        });
    }

    fn build(label: &str, is_light: bool) -> gtk::Window {
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        if let Some(screen) = WidgetExt::screen(&window) {
            // An alpha channel to be transparent *in*; without one the window
            // is painted opaque black.
            window.set_visual(screen.rgba_visual().as_ref());
        }
        // Styling each widget from its own provider rather than the screen's
        // keeps it scoped to this badge — font size depends on the label — and
        // means nothing outlives the window.
        style(&window.style_context(), "* { background-color: transparent; }");
        window.set_decorated(false);
        window.set_resizable(false);
        window.set_skip_taskbar_hint(true);
        window.set_skip_pager_hint(true);
        window.set_keep_above(true);
        window.set_accept_focus(false);
        window.set_focus_on_map(false);
        // Dock keeps the badge above ordinary windows without the window
        // manager treating it as something the user can focus or move.
        window.set_type_hint(gdk::WindowTypeHint::Dock);
        window.set_default_size(BADGE, BADGE);

        let badge = gtk::Label::new(Some(label));
        style(&badge.style_context(), &css(label, is_light));
        badge.set_size_request(BADGE, BADGE);
        window.add(&badge);
        window
    }

    fn style(context: &gtk::StyleContext, css: &str) {
        let provider = gtk::CssProvider::new();
        if provider.load_from_data(css.as_bytes()).is_ok() {
            context.add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    }

    fn present(window: &gtk::Window) {
        window.show_all();
        // Clicks belong to whatever is underneath.
        if let Some(gdk_window) = window.window() {
            gdk_window.set_pass_through(true);
        }
    }

    fn css(label: &str, is_light: bool) -> String {
        let (bg, fg, border) = if is_light {
            ("#ffffff", "#111111", "#cccccc")
        } else {
            ("#0e0e0e", "#ffffff", "#444444")
        };
        // Cloned displays get a "1|2" label, which at the single-digit size
        // would run past the badge; digits sit at roughly 0.55em apiece.
        let inner = (BADGE - 2 * MARGIN_LEFT) as f64;
        let size = (inner / (0.55 * label.chars().count().max(1) as f64)).min(180.0) as i32;
        format!(
            "* {{\n\
             \x20 background-color: {bg};\n\
             \x20 color: {fg};\n\
             \x20 border: 1px solid {border};\n\
             \x20 border-radius: 16px;\n\
             \x20 font-size: {size}px;\n\
             \x20 font-weight: bold;\n\
             }}\n"
        )
    }
}
