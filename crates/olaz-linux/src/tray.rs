//! Pure-Rust D-Bus `StatusNotifierItem` tray via `ksni` (`0.3`).
//!
//! Zero GTK3/libappindicator C dependency — renders natively on both Omarchy's
//! Quickshell `Tray.qml` and Ubuntu GNOME's `ubuntu-appindicators`.

use std::path::Path;
use std::sync::Arc;

use ksni::menu::{StandardItem, SubMenu};
use ksni::{Category, Icon, MenuItem, Status, ToolTip, Tray, TrayMethods};
use tokio::sync::mpsc;

/// Menu item labels displayed in the StatusNotifierItem DBusMenu.
pub const LABEL_SHOW_OLAZ: &str = "Mở Zalo / Open Zalo";
pub const LABEL_STATUS_MENU: &str = "Trạng thái / Status";
pub const LABEL_STATUS_ONLINE: &str = "Đang hoạt động / Online";
pub const LABEL_STATUS_BUSY: &str = "Bận / Busy";
pub const LABEL_SCREENSHOT: &str = "Chụp màn hình / Screenshot";
pub const LABEL_QUIT: &str = "Thoát / Quit";

/// Events emitted by the Olaz StatusNotifierItem tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OlazTrayEvent {
    /// Left-click on tray icon: toggle main window visibility.
    ToggleWindow { activation_token: Option<String> },
    /// "Mở Zalo / Open Zalo" menu action.
    ShowWindow { activation_token: Option<String> },
    /// Switch Zalo user presence to Online (`change-user-mode-online`).
    SetUserStatusOnline,
    /// Switch Zalo user presence to Busy (`change-user-mode-busy`).
    SetUserStatusBusy,
    /// "Chụp màn hình / Screenshot" menu action.
    Screenshot,
    /// "Thoát / Quit" menu action.
    Quit,
}

/// Pure-Rust anti-aliased signed-distance-field rasterizer for the Olaz tray icon
/// in ARGB32 network byte order (`[A, R, G, B]`), with optional unread dot overlay.
pub fn render_tray_icon_argb32(size: u32, has_unread: bool) -> Icon {
    let s = size.max(16) as f32;
    let mut data = Vec::with_capacity((size * size * 4) as usize);

    // Rounded squircle geometry in normalized [0, 1] coordinates.
    let margin = 0.06 * s;
    let half = 0.5 * s - margin;
    let corner_r = 0.24 * s;
    let cx = 0.5 * s;
    let cy = 0.5 * s;

    // Unread dot center & radius (top-right corner).
    let dot_cx = 0.78 * s;
    let dot_cy = 0.22 * s;
    let dot_r = 0.175 * s;
    let halo_r = dot_r + (0.045 * s).max(1.0);

    for py in 0..size {
        for px in 0..size {
            let x = px as f32 + 0.5;
            let y = py as f32 + 0.5;

            // Signed distance to rounded box.
            let qx = (x - cx).abs() - (half - corner_r);
            let qy = (y - cy).abs() - (half - corner_r);
            let out_dist = (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - corner_r;
            let bg_cov = (0.5 - out_dist).clamp(0.0, 1.0);

            // Base Zalo blue gradient (#0A74FF top -> #005BE6 bottom).
            let t = (y / s).clamp(0.0, 1.0);
            let mut r = 10.0 * (1.0 - t);
            let mut g = 116.0 * (1.0 - t) + 91.0 * t;
            let mut b = 255.0 * (1.0 - t) + 230.0 * t;
            let mut a = bg_cov;

            // Inner white speech-bubble mark: ellipse + bottom-left tail triangle.
            if bg_cov > 0.0 {
                let bx = (x - cx) / (0.27 * s);
                let by = (y - (cy - 0.02 * s)) / (0.215 * s);
                let bubble_dist = (bx * bx + by * by).sqrt() - 1.0;
                let bubble_px_dist = bubble_dist * (0.23 * s);

                // Tail pointing down-left from bubble.
                let tx = (x - cx) / s;
                let ty = (y - cy) / s;
                let in_tail = tx >= -0.22
                    && tx <= -0.02
                    && ty >= 0.08
                    && ty <= 0.25
                    && (ty - 0.08) <= (tx + 0.22) * 1.15;
                let tail_cov = if in_tail { 1.0_f32 } else { 0.0_f32 };
                let mark_cov = (0.5 - bubble_px_dist).clamp(0.0, 1.0).max(tail_cov);

                if mark_cov > 0.0 {
                    r = r * (1.0 - mark_cov) + 255.0 * mark_cov;
                    g = g * (1.0 - mark_cov) + 255.0 * mark_cov;
                    b = b * (1.0 - mark_cov) + 255.0 * mark_cov;
                }
            }

            // Optional unread notification dot (#FF3B30 with dark contrast halo).
            if has_unread {
                let d = (x - dot_cx).hypot(y - dot_cy);
                let halo_cov = (0.5 - (d - halo_r)).clamp(0.0, 1.0);
                let dot_cov = (0.5 - (d - dot_r)).clamp(0.0, 1.0);

                if halo_cov > 0.0 {
                    // Blend dark halo (#0B1324) over existing Premultiplied-like color.
                    let out_a = halo_cov + a * (1.0 - halo_cov);
                    if out_a > 0.0 {
                        r = (11.0 * halo_cov + r * a * (1.0 - halo_cov)) / out_a;
                        g = (19.0 * halo_cov + g * a * (1.0 - halo_cov)) / out_a;
                        b = (36.0 * halo_cov + b * a * (1.0 - halo_cov)) / out_a;
                    }
                    a = out_a;
                }
                if dot_cov > 0.0 {
                    let out_a = dot_cov + a * (1.0 - dot_cov);
                    if out_a > 0.0 {
                        r = (255.0 * dot_cov + r * a * (1.0 - dot_cov)) / out_a;
                        g = (59.0 * dot_cov + g * a * (1.0 - dot_cov)) / out_a;
                        b = (48.0 * dot_cov + b * a * (1.0 - dot_cov)) / out_a;
                    }
                    a = out_a;
                }
            }

            let alpha_u8 = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
            if alpha_u8 == 0 {
                data.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                data.extend_from_slice(&[
                    alpha_u8,
                    r.clamp(0.0, 255.0).round() as u8,
                    g.clamp(0.0, 255.0).round() as u8,
                    b.clamp(0.0, 255.0).round() as u8,
                ]);
            }
        }
    }

    Icon {
        width: size as i32,
        height: size as i32,
        data,
    }
}

/// Renders multi-resolution StatusNotifierItem pixmaps (`24x24`, `32x32`, `48x48`).
pub fn render_tray_pixmaps(has_unread: bool) -> Vec<Icon> {
    vec![
        render_tray_icon_argb32(24, has_unread),
        render_tray_icon_argb32(32, has_unread),
        render_tray_icon_argb32(48, has_unread),
    ]
}

/// `ksni::Tray` implementation for Olaz.
pub struct OlazTray {
    unread_count: u64,
    on_event: Arc<dyn Fn(OlazTrayEvent) + Send + Sync + 'static>,
}

impl OlazTray {
    pub fn new(on_event: impl Fn(OlazTrayEvent) + Send + Sync + 'static) -> Self {
        Self {
            unread_count: 0,
            on_event: Arc::new(on_event),
        }
    }

    pub fn unread_count(&self) -> u64 {
        self.unread_count
    }

    pub fn has_unread(&self) -> bool {
        self.unread_count > 0
    }

    pub fn set_unread_count(&mut self, count: u64) {
        self.unread_count = count;
    }

    fn emit(&self, event: OlazTrayEvent) {
        (self.on_event)(event);
    }
}

impl Tray for OlazTray {
    const MENU_ON_ACTIVATE: bool = false;

    fn id(&self) -> String {
        "olaz".to_string()
    }

    fn title(&self) -> String {
        if self.unread_count > 0 {
            format!("Olaz ({})", self.unread_count)
        } else {
            "Olaz".to_string()
        }
    }

    fn category(&self) -> Category {
        Category::Communications
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        render_tray_pixmaps(self.has_unread())
    }

    fn tool_tip(&self) -> ToolTip {
        let description = if self.unread_count > 0 {
            format!("{} tin nhắn chưa đọc / unread", self.unread_count)
        } else {
            "Zalo cho Linux".to_string()
        };
        ToolTip {
            icon_name: String::new(),
            icon_pixmap: render_tray_pixmaps(self.has_unread()),
            title: "Olaz".to_string(),
            description,
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let token = ksni::current_xdg_activation_token();
        self.emit(OlazTrayEvent::ToggleWindow {
            activation_token: token,
        });
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: LABEL_SHOW_OLAZ.to_string(),
                icon_name: "window-new-symbolic".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let token = ksni::current_xdg_activation_token();
                    tray.emit(OlazTrayEvent::ShowWindow {
                        activation_token: token,
                    });
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: LABEL_STATUS_MENU.to_string(),
                icon_name: "user-available-symbolic".to_string(),
                submenu: vec![
                    StandardItem {
                        label: LABEL_STATUS_ONLINE.to_string(),
                        icon_name: "user-available-symbolic".to_string(),
                        activate: Box::new(|tray: &mut Self| {
                            tray.emit(OlazTrayEvent::SetUserStatusOnline);
                        }),
                        ..Default::default()
                    }
                    .into(),
                    StandardItem {
                        label: LABEL_STATUS_BUSY.to_string(),
                        icon_name: "user-busy-symbolic".to_string(),
                        activate: Box::new(|tray: &mut Self| {
                            tray.emit(OlazTrayEvent::SetUserStatusBusy);
                        }),
                        ..Default::default()
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: LABEL_SCREENSHOT.to_string(),
                icon_name: "applets-screenshooter-symbolic".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    tray.emit(OlazTrayEvent::Screenshot);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: LABEL_QUIT.to_string(),
                icon_name: "application-exit-symbolic".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    tray.emit(OlazTrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Async handle controlling a spawned `OlazTray` StatusNotifierItem service.
#[derive(Clone)]
pub struct OlazTrayHandle {
    inner: ksni::Handle<OlazTray>,
}

impl OlazTrayHandle {
    /// Updates the unread count and refreshes the tray icon (unread dot) & tooltip over D-Bus.
    pub async fn set_unread_count(&self, unread_count: u64) {
        let _ = self
            .inner
            .update(move |tray| {
                tray.set_unread_count(unread_count);
            })
            .await;
    }

    /// Toggles the unread dot directly as a boolean (`1` vs `0` when count is binary).
    pub async fn set_has_unread(&self, has_unread: bool) {
        self.set_unread_count(u64::from(has_unread)).await;
    }

    /// Cleanly unregisters and shuts down the StatusNotifierItem service.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await;
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

/// Spawns the pure-Rust `StatusNotifierItem` tray on the session D-Bus and returns
/// an [`OlazTrayHandle`] plus an unbounded event receiver.
pub async fn spawn_tray() -> anyhow::Result<(OlazTrayHandle, mpsc::UnboundedReceiver<OlazTrayEvent>)>
{
    let (tx, rx) = mpsc::unbounded_channel();
    let tray = OlazTray::new(move |evt| {
        let _ = tx.send(evt);
    });

    let is_flatpak = Path::new("/.flatpak-info").exists();
    let handle = tray
        .disable_dbus_name(is_flatpak)
        .assume_sni_available(true)
        .spawn()
        .await?;

    Ok((OlazTrayHandle { inner: handle }, rx))
}
