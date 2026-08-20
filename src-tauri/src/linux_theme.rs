//! Linux window-theme handling.
//!
//! WebKitGTK derives `prefers-color-scheme` from the *GTK* theme, which on
//! desktops like GNOME 42+ says nothing about the system-wide dark
//! preference — that lives behind the freedesktop desktop portal (e.g.
//! Ubuntu leaves `gtk-theme-name` on a light theme while the portal reports
//! `prefer-dark`). This module reads the portal directly and makes sure the
//! GTK-drawn title bar actually follows it.

use tauri::{AppHandle, Manager, Theme};

/// `org.freedesktop.appearance color-scheme`: 0 = no preference, 1 = dark,
/// 2 = light. Same source tao's `Window::theme()` reads — asked directly so
/// that forcing a theme on the window doesn't hide the system value from us.
pub fn system_theme() -> Option<Theme> {
    let conn = zbus::blocking::Connection::session().ok()?;
    let reply = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.Settings"),
            "Read",
            &("org.freedesktop.appearance", "color-scheme"),
        )
        .ok()?;
    let value: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;

    // The portal wraps the setting's own value in a second variant layer
    // (confirmed against a live GNOME session: `gdbus call ...` returns
    // `(<<uint32 1>>,)` — two nested variants around the u32).
    let inner: zbus::zvariant::Value = value.into();
    let inner = match inner {
        zbus::zvariant::Value::Value(boxed) => *boxed,
        other => other,
    };

    match u32::try_from(inner).ok()? {
        1 => Some(Theme::Dark),
        2 => Some(Theme::Light),
        _ => None,
    }
}

/// The `gtk-theme-name` the app started with, so light mode can go back to it
/// after dark mode had to replace it.
static ORIGINAL_GTK_THEME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Makes the GTK-drawn window decorations (the title bar) match `dark`.
///
/// tao only flips `gtk-application-prefer-dark-theme`, which picks the *dark
/// variant of the current GTK theme* — a no-op when that theme has no dark
/// variant, or isn't installed at all (GTK then silently falls back to light
/// Adwaita). So this checks what the theme actually resolved to and, if it's
/// on the wrong side, switches this process over to Adwaita, whose dark
/// variant ships inside GTK itself.
pub fn sync_gtk_theme(app: &AppHandle, dark: bool) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        use gtk::prelude::*;

        let Some(settings) = gtk::Settings::default() else {
            return;
        };
        let original =
            ORIGINAL_GTK_THEME.get_or_init(|| settings.gtk_theme_name().map(|s| s.to_string()));

        // Always start from the user's own theme: it may well have a dark
        // variant, and then it's the one they want to see.
        settings.set_gtk_theme_name(original.as_deref());
        settings.set_gtk_application_prefer_dark_theme(dark);

        let Some(window) = handle.get_webview_window("main").and_then(|w| w.gtk_window().ok())
        else {
            return;
        };
        if gtk_style_is_dark(&window) != dark {
            settings.set_gtk_theme_name(Some("Adwaita"));
        }
    });
}

/// Whether the window currently renders dark, judged by the luminance of the
/// theme's background colour.
fn gtk_style_is_dark(window: &gtk::ApplicationWindow) -> bool {
    use gtk::prelude::*;

    let ctx = window.style_context();
    let luminance = |c: gtk::gdk::RGBA| 0.2126 * c.red() + 0.7152 * c.green() + 0.0722 * c.blue();

    match ctx.lookup_color("theme_bg_color") {
        Some(bg) => luminance(bg) < 0.5,
        // No named colours (some minimal themes): fall back to the text
        // colour, which runs the other way around.
        None => luminance(ctx.color(gtk::StateFlags::NORMAL)) > 0.5,
    }
}
