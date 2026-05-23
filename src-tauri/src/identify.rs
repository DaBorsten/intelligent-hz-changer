use crate::display::MonitorInfoExtended;

pub fn show_overlays(monitors: Vec<MonitorInfoExtended>, is_light: bool) {
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
    std::thread::spawn(move || unsafe {
        #[cfg(windows)]
        inner::run(monitors, labels, is_light);
    });
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
        *LABELS.lock().unwrap() = labels;

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
                let label = LABELS.lock().unwrap().get(idx).cloned().unwrap_or_else(|| (idx + 1).to_string());
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
