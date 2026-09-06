//! 原版风格设置窗口：三组圆角面板、粉色滑块和自绘开关。
//! 直接读写共享 Settings，变更时保存并通知覆盖层线程重新应用。

use crate::log::log;
use crate::overlay::MSG_SETTINGS_CHANGED;
use crate::settings::Settings;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, EnumWindows, FindWindowW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, GWLP_WNDPROC, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTLEFT,
    HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, IsWindowVisible, PostMessageW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, SW_HIDE, SW_SHOW, WM_NCHITTEST, WNDPROC,
};

const APP_ICON_ICO: &[u8] = include_bytes!("../assets/icon.ico");
const TORUS_REGULAR_OTF: &[u8] = include_bytes!("../assets/Torus-Regular.otf");
const BACKGROUND: egui::Color32 = egui::Color32::from_rgb(18, 19, 24);
const PANEL: egui::Color32 = egui::Color32::from_rgb(30, 31, 38);
const ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 102, 171);
const TEXT: egui::Color32 = egui::Color32::from_rgb(238, 239, 244);
const MUTED: egui::Color32 = egui::Color32::from_rgb(158, 160, 172);
/// 滑条轨道底色（比面板亮一档，保证轨道在面板上可见）。
const RAIL: egui::Color32 = egui::Color32::from_rgb(100, 103, 113);
const CONTROL_BG: egui::Color32 = egui::Color32::from_rgb(37, 37, 46);
const CONTROL_HOVER: egui::Color32 = egui::Color32::from_rgb(48, 44, 55);
const CONTROL_BORDER: egui::Color32 = egui::Color32::from_rgb(255, 116, 181);
const SLIDER_TRACK: egui::Color32 = egui::Color32::from_rgb(29, 29, 36);
const SLIDER_FILL: egui::Color32 = egui::Color32::from_rgb(205, 77, 162);
const SLIDER_HIGHLIGHT: egui::Color32 = egui::Color32::from_rgb(255, 151, 212);
/// 光标与音量滑条左侧的固定数值列，保证所有滑条起点对齐。
const SLIDER_VALUE_COLUMN_WIDTH: f32 = 75.0;
/// 下拉列表滚动条的灰紫把手，贴近 osu! 的宽胶囊形滚动条。
const SCROLL_HANDLE: egui::Color32 = egui::Color32::from_rgb(185, 179, 197);
/// 标题栏按钮/图标距窗口顶的视觉边距（并入按钮/拖拽交互区）。
const TITLEBAR_TOP_PAD: f32 = 12.0;
/// 标题栏整条交互高度（含顶部边距；下方 40px 为视觉按钮区）。
const TITLEBAR_H: f32 = TITLEBAR_TOP_PAD + 40.0;

/// 使用内嵌 Torus 作为拉丁文字体，并以系统微软雅黑补齐中文字符。
fn setup_fonts(ctx: &egui::Context) {
    let yahei_candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\msyhbd.ttc",
    ];

    let mut fonts = egui::FontDefinitions::default();
    let mut has_yahei = false;
    fonts.font_data.insert(
        "torus".to_owned(),
        egui::FontData::from_static(TORUS_REGULAR_OTF),
    );
    for path in yahei_candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("msyh".to_owned(), egui::FontData::from_owned(bytes));
            has_yahei = true;
            break;
        }
    }

    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        if has_yahei {
            family.insert(0, "msyh".to_owned());
        }
        family.insert(0, "torus".to_owned());
    }
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
        if has_yahei {
            family.insert(0, "msyh".to_owned());
        }
        family.insert(0, "torus".to_owned());
    }

    ctx.set_fonts(fonts);
    if has_yahei {
        log("settings_ui: loaded embedded Torus with Microsoft YaHei fallback");
    } else {
        log("settings_ui: loaded embedded Torus; Microsoft YaHei fallback unavailable");
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.slider_width = 320.0;
    // ComboBox 的弹出层由 Context 样式创建，不会继承局部 ui.scope。
    // 因此把滚动条规格放在全局样式，保证窗口列表也使用 osu! 风格的宽圆角把手。
    let scroll = &mut style.spacing.scroll;
    scroll.floating = false;
    scroll.bar_width = 20.0;
    scroll.handle_min_length = 44.0;
    scroll.bar_inner_margin = 4.0;
    scroll.bar_outer_margin = 0.0;
    scroll.foreground_color = true;

    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(TEXT);
    // egui 0.28 滑条轨道与圆形滑块底色均由 widgets.inactive.bg_fill 绘制
    // （见 egui slider.rs: rect_filled(rail_rect, ..., inactive.bg_fill)）。
    // 原值 PANEL 与面板底色重合导致轨道隐形，改用亮一档的 RAIL。
    visuals.widgets.inactive.bg_fill = RAIL;
    visuals.widgets.inactive.fg_stroke.color = TEXT;
    visuals.widgets.hovered.bg_fill = RAIL;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
    visuals.widgets.hovered.fg_stroke.color = TEXT;
    visuals.widgets.active.bg_fill = ACCENT;
    visuals.widgets.active.fg_stroke.color = TEXT;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke.color = TEXT;
    visuals.extreme_bg_color = CONTROL_BG;
    visuals.window_fill = CONTROL_BG;
    visuals.window_stroke = egui::Stroke::new(2.0_f32, CONTROL_BORDER);
    visuals.window_rounding = egui::Rounding::same(8.0);
    visuals.menu_rounding = egui::Rounding::same(8.0);
    // ScrollArea 在 foreground_color 模式下以各状态的 fg_stroke 绘制把手。
    // 统一为圆角灰紫色，避免默认白色直角块与 osu! 列表风格冲突。
    for widget in [
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
    ] {
        widget.fg_stroke.color = SCROLL_HANDLE;
        widget.rounding = egui::Rounding::same(10.0);
    }
    // 显示"起点→滑块"的填充段（用 selection.bg_fill=ACCENT 粉色），
    // 对齐原版 WPF 滑条"粉填充 + 灰轨道"外观。
    visuals.slider_trailing_fill = true;
    style.visuals = visuals;
    ctx.set_style(style);
}

/// eframe 默认会显示一个白色的 e。ICO 中的第一项是 PNG，直接解码后
/// 传给 viewport，设置窗口、任务栏和 Alt-Tab 会使用 Curosu 图标。
fn load_app_icon() -> Option<egui::IconData> {
    if APP_ICON_ICO.len() < 22 {
        return None;
    }
    let image_size = u32::from_le_bytes(APP_ICON_ICO[14..18].try_into().ok()?) as usize;
    let image_offset = u32::from_le_bytes(APP_ICON_ICO[18..22].try_into().ok()?) as usize;
    let image_end = image_offset.checked_add(image_size)?;
    let png_bytes = APP_ICON_ICO.get(image_offset..image_end)?;

    let mut decoder = png::Decoder::new(png_bytes);
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    rgba.truncate((info.width as usize) * (info.height as usize) * 4);
    Some(egui::IconData {
        rgba,
        width: info.width,
        height: info.height,
    })
}

fn section(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(PANEL)
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(14.0))
        .show(ui, |ui| {
            // Frame 默认按子控件收缩；选择器一行较短时会让整个面板变窄。
            // 统一最小宽度后所有设置卡片与 osu! 风格的整列面板对齐。
            ui.set_min_width(ui.available_width());
            ui.label(
                egui::RichText::new(title)
                    .size(14.0)
                    .strong()
                    .color(ACCENT),
            );
            ui.add_space(2.0);
            contents(ui);
        });
}

fn draw_switch(ui: &mut egui::Ui, id_source: &str, checked: &mut bool, label: &str) {
    let id = ui.make_persistent_id(id_source);
    let (row, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 28.0),
        egui::Sense::click(),
    );
    if response.clicked() {
        *checked = !*checked;
    }

    let checked_t = ui
        .ctx()
        .animate_bool_with_time(id.with("checked"), *checked, 0.18);
    let hover_t = ui
        .ctx()
        .animate_bool_with_time(id.with("hover"), response.hovered(), 0.12);

    ui.painter().text(
        egui::pos2(row.left(), row.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        TEXT,
    );

    let scale = 1.0 + hover_t * 0.04;
    let track = egui::Rect::from_center_size(
        egui::pos2(row.right() - 25.0, row.center().y),
        egui::vec2(50.0 * scale, 15.0 * scale),
    );
    ui.painter()
        .rect_filled(track, 7.5 * scale, egui::Color32::WHITE);

    let border = 3.0 + checked_t * 3.0;
    let inner = track.shrink(border);
    let alpha = (checked_t * 255.0).round().clamp(0.0, 255.0) as u8;
    let fill = egui::Color32::from_rgba_unmultiplied(
        ACCENT.r(),
        ACCENT.g(),
        ACCENT.b(),
        alpha,
    );
    ui.painter().rect_filled(inner, 5.0 * scale, fill);

    if response.hovered() {
        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
    }
}

fn draw_slider(ui: &mut egui::Ui, value: &mut f64, range: std::ops::RangeInclusive<f64>) {
    let min = *range.start();
    let max = *range.end();
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, 52.0),
        egui::Sense::click_and_drag(),
    );
    let track = rect.shrink2(egui::vec2(0.0, 3.0));

    if let Some(pointer) = response.interact_pointer_pos() {
        let progress = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64;
        *value = (min + (max - min) * progress).clamp(min, max);
    }

    let progress = if max > min {
        ((*value - min) / (max - min)).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    // 把手宽 18px，中心点必须留出两端各半个把手的空间，否则 0%/100% 时会
    // 伸出滑轨。输入仍按完整轨道映射，方便点击两端直接设为最小/最大值。
    let thumb_half_width = 9.0;
    let thumb_x = egui::lerp(
        (track.left() + thumb_half_width)..=(track.right() - thumb_half_width),
        progress,
    );
    let fill = egui::Rect::from_min_max(
        track.left_top(),
        egui::pos2((thumb_x + 8.0).min(track.right()), track.bottom()),
    );
    let thumb = egui::Rect::from_center_size(
        egui::pos2(thumb_x, rect.center().y),
        egui::vec2(18.0, 40.0),
    );
    let track_stroke = if response.hovered() || response.dragged() {
        egui::Stroke::new(1.5_f32, CONTROL_BORDER)
    } else {
        egui::Stroke::NONE
    };
    ui.painter().rect_filled(track, 8.0, SLIDER_TRACK);
    ui.painter().rect_stroke(track, 8.0, track_stroke);
    ui.painter().rect_filled(fill, 8.0, SLIDER_FILL);
    ui.painter().rect_filled(thumb, 7.0, SLIDER_FILL);
    let highlight = egui::Rect::from_center_size(
        egui::pos2(thumb.right() - 5.0, thumb.center().y),
        egui::vec2(4.0, 28.0),
    );
    ui.painter().rect_filled(highlight, 2.0, SLIDER_HIGHLIGHT);

    if response.hovered() || response.dragged() {
        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
    }
}

/// `allocate_ui_with_layout` 会按内容收缩，不能保证横向布局预留固定宽度。
/// 此处先占用精确矩形，再在其中绘制数值，确保后续滑条严格对齐。
fn slider_value_column(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(SLIDER_VALUE_COLUMN_WIDTH, 52.0),
        egui::Sense::hover(),
    );
    let mut column = ui.child_ui(rect, egui::Layout::top_down(egui::Align::Min), None);
    contents(&mut column);
}

fn draw_volume(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    ui.horizontal(|ui| {
        slider_value_column(ui, |ui| {
            ui.label(egui::RichText::new(label).size(13.0).color(MUTED));
            ui.label(
                egui::RichText::new(format!("{}%", (*value * 100.0).round() as i32))
                    .size(18.0)
                    .color(TEXT),
            );
        });
        ui.add_space(14.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 52.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| draw_slider(ui, value, 0.0..=1.0),
        );
    });
}

fn accent_button(ui: &mut egui::Ui, label: &str, size: egui::Vec2) -> egui::Response {
    ui.add_sized(
        size,
        egui::Button::new(egui::RichText::new(label).color(TEXT))
            .fill(ACCENT)
            .stroke(egui::Stroke::NONE),
    )
}

fn reset_icon_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(36.0, 52.0), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, CONTROL_HOVER);
        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
    }

    // 不依赖字体的回转箭头，避免 Microsoft YaHei 缺少 ↶ 字形而显示方块。
    // 两段贝塞尔曲线保持圆弧平滑，避免折线在小尺寸下看起来像损坏的图标。
    // 所有 x 坐标关于按钮中心水平镜像，箭头指向左上（左右翻转后的恢复默认）。
    let center = rect.center();
    let stroke = egui::Stroke::new(2.2_f32, ACCENT);
    ui.painter().add(egui::Shape::CubicBezier(
        egui::epaint::CubicBezierShape::from_points_stroke(
            [
                egui::pos2(center.x - 8.0, center.y + 3.0),
                egui::pos2(center.x - 8.0, center.y + 8.0),
                egui::pos2(center.x + 8.0, center.y + 9.0),
                egui::pos2(center.x + 8.0, center.y),
            ],
            false,
            egui::Color32::TRANSPARENT,
            stroke,
        ),
    ));
    ui.painter().add(egui::Shape::CubicBezier(
        egui::epaint::CubicBezierShape::from_points_stroke(
            [
                egui::pos2(center.x + 8.0, center.y),
                egui::pos2(center.x + 8.0, center.y - 8.0),
                egui::pos2(center.x - 1.0, center.y - 10.0),
                egui::pos2(center.x - 6.0, center.y - 6.0),
            ],
            false,
            egui::Color32::TRANSPARENT,
            stroke,
        ),
    ));
    // 实心、加大的箭头让 36px 按钮内的图标一眼能辨认为“恢复默认”。
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x - 9.5, center.y - 7.0),
            egui::pos2(center.x - 2.0, center.y - 11.0),
            egui::pos2(center.x - 2.0, center.y - 3.0),
        ],
        ACCENT,
        egui::Stroke::NONE,
    ));
    response.on_hover_text("恢复默认")
}

/// 自绘标题栏按钮（最小化/关闭）。图标用 painter 线段绘制，不依赖字体字形。
/// 悬停底色：最小化 CONTROL_HOVER，关闭 SLIDER_FILL（滑条填充粉红）。
fn titlebar_button(ui: &mut egui::Ui, close: bool) -> egui::Response {
    // 尺寸覆盖整条标题栏（含顶部 16px 视觉边距）：交互与 hover 填充
    // 由窗口顶 (y=0) 到按钮底整块生效；图标画在顶部边距下方的按钮区中心。
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(44.0, TITLEBAR_H),
        egui::Sense::click(),
    );
    let hovered = response.hovered();
    if hovered {
        let bg = if close { SLIDER_FILL } else { CONTROL_HOVER };
        ui.painter().rect_filled(rect, 0.0, bg);
        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
    }

    let stroke = egui::Stroke::new(2.0_f32, if hovered { TEXT } else { MUTED });
    // 图标垂直中心 = 顶部视觉边距 + 40px 按钮区中心（与"设置"文字对齐）。
    let center = egui::pos2(rect.center().x, rect.top() + TITLEBAR_TOP_PAD + 20.0);
    if close {
        // ×：两条对角线
        let r = 6.0;
        ui.painter().line_segment(
            [
                egui::pos2(center.x - r, center.y - r),
                egui::pos2(center.x + r, center.y + r),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(center.x - r, center.y + r),
                egui::pos2(center.x + r, center.y - r),
            ],
            stroke,
        );
    } else {
        // —：最小化横线
        ui.painter().line_segment(
            [
                egui::pos2(center.x - 6.0, center.y),
                egui::pos2(center.x + 6.0, center.y),
            ],
            stroke,
        );
    }
    response
}

/// 设置线程的控制命令。
pub enum UiCmd {
    /// 显示并聚焦设置窗口（窗口未创建时由 egui 命令兜底）。
    Show,
    /// 请求关闭设置线程（程序退出时）。
    Quit,
}

/// 设置窗口自身的 HWND（首帧由设置线程记录，overlay 用它控制显示/隐藏）。
/// 用原生 ShowWindow 控制显示比 egui 的 ViewportCommand::Visible 更可靠，
/// 避免隐藏后事件循环不再处理命令导致"关了就打不开"。
static SETTINGS_HWND: AtomicUsize = AtomicUsize::new(0);

/// winit 0.29 无装饰窗口不处理 WM_NCHITTEST，全窗口都是客户区，
/// 边缘拖拽无法缩放。子类化窗口过程，在 8 物理像素边缘返回缩放命中码。
static PREV_WNDPROC: AtomicUsize = AtomicUsize::new(0);
/// 缩放热区宽度（物理像素，接近系统默认边框 4+4）。
const RESIZE_BORDER_PX: i32 = 8;

unsafe extern "system" fn settings_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCHITTEST {
        // lparam 为屏幕物理坐标（两个有符号 16 位）
        let x = (lparam as u16) as i16 as i32;
        let y = ((lparam as usize) >> 16) as u16 as i16 as i32;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &mut rect) != 0 {
            let left = x - rect.left;
            let right = rect.right - x;
            let top = y - rect.top;
            let bottom = rect.bottom - y;
            let hit = if top < RESIZE_BORDER_PX {
                if left < RESIZE_BORDER_PX {
                    HTTOPLEFT
                } else if right < RESIZE_BORDER_PX {
                    HTTOPRIGHT
                } else {
                    HTTOP
                }
            } else if bottom < RESIZE_BORDER_PX {
                if left < RESIZE_BORDER_PX {
                    HTBOTTOMLEFT
                } else if right < RESIZE_BORDER_PX {
                    HTBOTTOMRIGHT
                } else {
                    HTBOTTOM
                }
            } else if left < RESIZE_BORDER_PX {
                HTLEFT
            } else if right < RESIZE_BORDER_PX {
                HTRIGHT
            } else {
                0
            };
            if hit != 0 {
                return hit as LRESULT;
            }
        }
    }
    let prev = PREV_WNDPROC.load(Ordering::SeqCst);
    let prev_proc: WNDPROC = Some(std::mem::transmute(prev));
    CallWindowProcW(prev_proc, hwnd, msg, wparam, lparam)
}

/// 子类化（在 winit 事件循环线程内调用，同线程安全）。
unsafe fn install_resize_subclass(hwnd: HWND) {
    let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, settings_wndproc as isize);
    if old != 0 {
        PREV_WNDPROC.store(old as usize, Ordering::SeqCst);
    } else {
        log("settings_ui: resize subclass failed");
    }
}

/// Windows 11+ DWM 圆角；Windows 10 不支持该属性，失败静默忽略（保持方角）。
unsafe fn apply_win11_rounded_corners(hwnd: HWND) {
    let preference: i32 = DWMWCP_ROUND;
    let hr = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &preference as *const i32 as *const core::ffi::c_void,
        std::mem::size_of::<i32>() as u32,
    );
    if hr != 0 {
        log("settings_ui: DWM rounded corners unavailable (pre-Win11)");
    }
}

struct SettingsApp {
    settings: Arc<Mutex<Settings>>,
    hwnd: HWND,
    rx: Receiver<UiCmd>,
    settings_hwnd_found: bool,
    selectable_windows: Vec<WindowChoice>,
    selected_window_executable: Option<String>,
}

#[derive(Clone)]
struct WindowChoice {
    executable: String,
    label: String,
}

impl SettingsApp {
    fn refresh_selectable_windows(&mut self) {
        self.selectable_windows = enumerate_visible_windows();
        if let Some(selected) = &self.selected_window_executable {
            if !self
                .selectable_windows
                .iter()
                .any(|window| &window.executable == selected)
            {
                self.selected_window_executable = None;
            }
        }
    }

    /// 记录设置窗口自己的 HWND，供 overlay 原生显示/隐藏。
    fn find_own_hwnd(&mut self) {
        if self.settings_hwnd_found {
            return;
        }
        let title: Vec<u16> = "Curosu 设置\0".encode_utf16().collect();
        unsafe {
            let w = FindWindowW(std::ptr::null(), title.as_ptr());
            if !w.is_null() {
                SETTINGS_HWND.store(w as usize, Ordering::SeqCst);
                self.settings_hwnd_found = true;
                apply_win11_rounded_corners(w);
                install_resize_subclass(w);
            }
        }
    }

    /// 处理控制命令；把"关闭"拦截为"隐藏"，窗口常驻可再次打开。
    fn handle_commands(&mut self, ctx: &egui::Context) {
        self.find_own_hwnd();

        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                UiCmd::Show => {
                    let h = SETTINGS_HWND.load(Ordering::SeqCst);
                    if h != 0 {
                        unsafe {
                            ShowWindow(h as HWND, SW_SHOW);
                            SetForegroundWindow(h as HWND);
                        }
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                }
                UiCmd::Quit => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        // 用户点关闭按钮：不销毁窗口/不退出事件循环，改为隐藏，可再次打开。
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            let h = SETTINGS_HWND.load(Ordering::SeqCst);
            if h != 0 {
                unsafe {
                    ShowWindow(h as HWND, SW_HIDE);
                }
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }
        // 隐藏期间事件循环可能没有重绘事件，周期唤醒以处理后续命令。
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
    }
}

impl eframe::App for SettingsApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_commands(ctx);
        if self.selectable_windows.is_empty() {
            self.refresh_selectable_windows();
        }

        let mut s = {
            let g = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            g.clone()
        };
        let before = s.clone();

        // 单个 CentralPanel：标题栏作为面板内首行，与内容区共用同一 fill，
        // 物理上只有一个填充矩形，杜绝面板接缝。左侧标题区按住可拖动窗口，
        // 右侧为最小化/关闭按钮（右缘贴窗口右沿用惯例）。
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BACKGROUND))
            .show(ctx, |ui| {
                // 标题栏：整条（含上方 TITLEBAR_TOP_PAD 视觉边距）为拖拽/按钮交互区，
                // hover 填充覆盖到窗口顶；"设置"文字与图标在顶边距下方居中。
                ui.horizontal(|ui| {
                    // 按钮间零间距，两个按钮无缝紧贴右上角。
                    ui.style_mut().spacing.item_spacing.x = 0.0;
                    let drag_width = (ui.available_width() - 88.0).max(60.0);
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(drag_width, TITLEBAR_H),
                        egui::Sense::click_and_drag(),
                    );
                    // 恢复标题文字（与按钮图标同一水平线 = 顶边距下方的按钮区中心）。
                    let inner_center_y = rect.top() + TITLEBAR_TOP_PAD + 20.0;
                    ui.painter().text(
                        egui::pos2(rect.left() + 20.0, inner_center_y),
                        egui::Align2::LEFT_CENTER,
                        "设置",
                        egui::FontId::proportional(24.0),
                        ACCENT,
                    );
                    if response.drag_started() {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if titlebar_button(ui, false).clicked() {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    if titlebar_button(ui, true).clicked() {
                        // Close 命令下一帧触发 close_requested()，由 handle_commands
                        // 拦截为 CancelClose + SW_HIDE，保持"关闭=隐藏"语义。
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });

                // 抵消行后默认 item_spacing.y(8px)，标题行底直接衔接内容区。
                ui.add_space(-ui.spacing().item_spacing.y);

                // 内容区：内边距由内层透明 Frame 提供，与标题栏共用 BACKGROUND。
                // 左侧 10px、右侧 20px；顶部 8px 使标题行与内容面板之间紧凑而不拥挤。
                egui::Frame::none()
                    .inner_margin(egui::Margin {
                        left: 10.0,
                        right: 20.0,
                        top: 8.0,
                        bottom: 20.0,
                    })
                    .show(ui, |ui| {
                // 例外列表会随着用户添加持续增长；主体必须在设置窗口内
                // 滚动，而不是让底部内容被裁掉。
                egui::ScrollArea::vertical()
                    .id_source("settings_content_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                section(ui, "光标", |ui| {
                    ui.horizontal(|ui| {
                        slider_value_column(ui, |ui| {
                            ui.label(egui::RichText::new("光标大小").size(13.0).color(MUTED));
                            ui.label(
                                egui::RichText::new(format!("{:.0} px", s.cursor_width))
                                    .size(18.0)
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(14.0);
                        let slider_width = (ui.available_width() - 42.0).max(48.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(slider_width, 52.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| draw_slider(ui, &mut s.cursor_width, 16.0..=64.0),
                        );
                        ui.add_space(6.0);
                        if reset_icon_button(ui).clicked() {
                            s.cursor_width = 30.0;
                        }
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        slider_value_column(ui, |ui| {
                            ui.label(egui::RichText::new("光标透明度").size(13.0).color(MUTED));
                            ui.label(
                                egui::RichText::new(format!("{:.0}%", s.cursor_opacity * 100.0))
                                    .size(18.0)
                                    .color(TEXT),
                            );
                        });
                        ui.add_space(14.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 52.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| draw_slider(ui, &mut s.cursor_opacity, 0.4..=1.0),
                        );
                    });
                });
                ui.add_space(12.0);

                section(ui, "音效", |ui| {
                    draw_switch(ui, "tap_sound", &mut s.tap_sound_enabled, "敲击音效");
                    ui.add_space(2.0);
                    ui.add_enabled_ui(s.tap_sound_enabled, |ui| {
                        draw_volume(ui, "音量", &mut s.tap_sound_volume);
                    });

                    ui.add_space(8.0);
                    draw_switch(ui, "hover_sound", &mut s.hover_sound_enabled, "悬停音效");
                    ui.add_space(2.0);
                    ui.add_enabled_ui(s.hover_sound_enabled, |ui| {
                        draw_volume(ui, "悬停音量", &mut s.hover_sound_volume);
                    });

                    ui.add_space(8.0);
                    draw_switch(
                        ui,
                        "resize_sound",
                        &mut s.hover_sound_as_resize_prompt,
                        "窗口拉伸时播放",
                    );
                });
                ui.add_space(12.0);

                section(ui, "系统", |ui| {
                    draw_switch(ui, "auto_start", &mut s.auto_start, "开机自启");
                });
                ui.add_space(12.0);

                section(ui, "全屏例外", |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("选择后添加：该程序全屏时仍显示动画光标")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if accent_button(ui, "刷新窗口列表", egui::vec2(96.0, 28.0)).clicked() {
                                self.refresh_selectable_windows();
                            }
                        });
                    });
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {

                        let selected_label = self
                            .selected_window_executable
                            .as_ref()
                            .and_then(|selected| {
                                self.selectable_windows
                                    .iter()
                                    .find(|window| &window.executable == selected)
                            })
                            .map(|window| window.label.as_str())
                            .unwrap_or("选择窗口…");
                        // ComboBox::width 是最小宽度而不是最大宽度。把它放进固定宽度
                        // 的子 UI 并启用截断，长窗口标题不会再挤掉“添加”按钮。
                        let selector_width =
                            (ui.available_width() - 58.0 - ui.spacing().item_spacing.x).max(80.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(selector_width, 52.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| ui.scope(|ui| {
                            ui.style_mut().spacing.interact_size.y = 52.0;
                            ui.style_mut().spacing.button_padding = egui::vec2(10.0, 8.0);
                            let visuals = &mut ui.style_mut().visuals;
                            visuals.extreme_bg_color = CONTROL_BG;
                            visuals.window_fill = CONTROL_BG;
                            visuals.window_stroke = egui::Stroke::new(2.0_f32, CONTROL_BORDER);
                            visuals.window_rounding = egui::Rounding::same(8.0);
                            visuals.menu_rounding = egui::Rounding::same(8.0);
                            for widget in [
                                &mut visuals.widgets.inactive,
                                &mut visuals.widgets.hovered,
                                &mut visuals.widgets.active,
                                &mut visuals.widgets.open,
                            ] {
                                widget.bg_fill = CONTROL_BG;
                                widget.weak_bg_fill = CONTROL_BG;
                                widget.bg_stroke = egui::Stroke::new(1.5_f32, CONTROL_BORDER);
                                widget.rounding = egui::Rounding::same(6.0);
                                widget.fg_stroke.color = TEXT;
                            }
                            visuals.widgets.hovered.bg_fill = CONTROL_HOVER;
                            visuals.widgets.active.bg_fill = CONTROL_HOVER;
                            visuals.widgets.open.bg_fill = CONTROL_HOVER;
                            // ScrollArea 在 foreground_color 模式下使用 fg_stroke 作把手。
                            // 保持下拉本身的深色背景，同时获得参考图那种浅紫宽把手。
                            visuals.widgets.inactive.fg_stroke.color = SCROLL_HANDLE;
                            visuals.widgets.hovered.fg_stroke.color = SCROLL_HANDLE;
                            visuals.widgets.active.fg_stroke.color = SCROLL_HANDLE;
                            visuals.selection.bg_fill = SLIDER_FILL;
                            visuals.selection.stroke = egui::Stroke::NONE;

                            egui::ComboBox::from_id_source("fullscreen_exception_window")
                                .selected_text(egui::RichText::new(selected_label).color(TEXT))
                                .width(selector_width)
                                .height(260.0)
                                .truncate()
                                .icon(|ui, rect, _visuals, is_open, _| {
                                    let center = rect.center();
                                    let direction = if is_open { -1.0 } else { 1.0 };
                                    let left = egui::pos2(center.x - 5.0, center.y - 2.5 * direction);
                                    let middle = egui::pos2(center.x, center.y + 2.5 * direction);
                                    let right = egui::pos2(center.x + 5.0, center.y - 2.5 * direction);
                                    let stroke = egui::Stroke::new(2.0_f32, TEXT);
                                    ui.painter().line_segment([left, middle], stroke);
                                    ui.painter().line_segment([middle, right], stroke);
                                })
                                .show_ui(ui, |ui| {
                                    // ComboBox 内部默认强制选项文本横向扩展；长窗口标题
                                    // 会把弹出层撑出视口，导致左右描边被裁掉。恢复截断并把
                                    // 每一项限制在触发框宽度内，使弹出层四边完整可见。
                                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                    let item_width = ui.available_width();
                                    ui.set_max_width(item_width);
                                    for window in &self.selectable_windows {
                                        let selected = self
                                            .selected_window_executable
                                            .as_ref()
                                            .is_some_and(|value| value == &window.executable);
                                        let response = ui.add_sized(
                                            egui::vec2(item_width, ui.spacing().interact_size.y),
                                            egui::SelectableLabel::new(selected, &window.label),
                                        );
                                        if response.clicked() {
                                            self.selected_window_executable =
                                                Some(window.executable.clone());
                                        }
                                    }
                                });
                            }),
                        );

                        let can_add = self.selected_window_executable.is_some();
                        if ui
                            .add_enabled_ui(can_add, |ui| {
                                accent_button(ui, "添加", egui::vec2(58.0, 52.0))
                            })
                            .inner
                            .clicked()
                        {
                            let executable = self
                                .selected_window_executable
                                .as_ref()
                                .expect("button is enabled only with a selection");
                            if !s
                                .fullscreen_overlay_exclusion_executables
                                .iter()
                                .any(|item| item.eq_ignore_ascii_case(executable))
                            {
                                s.fullscreen_overlay_exclusion_executables
                                    .push(executable.clone());
                            }
                        }
                    });

                    let mut remove_index = None;
                    for (index, executable) in s
                        .fullscreen_overlay_exclusion_executables
                        .iter()
                        .enumerate()
                    {
                        ui.horizontal(|ui| {
                            ui.label(executable);
                            if ui.small_button("移除").clicked() {
                                remove_index = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove_index {
                        s.fullscreen_overlay_exclusion_executables.remove(index);
                    }
                });
                    });
                    });
            });

        if s != before {
            s.save();
            {
                let mut g = self.settings.lock().unwrap_or_else(|e| e.into_inner());
                *g = s;
            }
            unsafe {
                PostMessageW(self.hwnd, MSG_SETTINGS_CHANGED, 0, 0);
            }
        }
    }
}

fn enumerate_visible_windows() -> Vec<WindowChoice> {
    let mut windows: Vec<WindowChoice> = Vec::new();
    unsafe {
        EnumWindows(
            Some(collect_visible_window),
            &mut windows as *mut Vec<WindowChoice> as LPARAM,
        );
    }
    windows.sort_by(|left, right| left.label.cmp(&right.label));
    windows
}

unsafe extern "system" fn collect_visible_window(hwnd: HWND, lparam: LPARAM) -> i32 {
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut title = [0u16; 256];
    let title_len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
    if title_len <= 0 {
        return 1;
    }
    let Some(executable) = window_executable_name(hwnd) else {
        return 1;
    };
    if executable.eq_ignore_ascii_case("curosu.exe") {
        return 1;
    }

    let windows = &mut *(lparam as *mut Vec<WindowChoice>);
    if windows
        .iter()
        .any(|window| window.executable.eq_ignore_ascii_case(&executable))
    {
        return 1;
    }
    let title = String::from_utf16_lossy(&title[..title_len as usize]);
    windows.push(WindowChoice {
        label: format!("{executable} — {title}"),
        executable,
    });
    1
}

unsafe fn window_executable_name(hwnd: HWND) -> Option<String> {
    let mut process_id = 0;
    GetWindowThreadProcessId(hwnd, &mut process_id);
    if process_id == 0 {
        return None;
    }
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
    if process.is_null() {
        return None;
    }

    let mut path = [0u16; 1024];
    let mut path_len = path.len() as u32;
    let queried = QueryFullProcessImageNameW(
        process,
        PROCESS_NAME_WIN32,
        path.as_mut_ptr(),
        &mut path_len,
    );
    CloseHandle(process);
    if queried == 0 || path_len == 0 {
        return None;
    }
    std::path::Path::new(&String::from_utf16_lossy(&path[..path_len as usize]))
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// 常驻设置线程的命令发送端与启动标记。
/// 设置窗口只在启动时创建一次、run_native 常驻运行；打开/关闭通过命令通道
/// 控制显示/隐藏，窗口从不销毁、事件循环从不退出，彻底避免反复创建/复用
/// winit 事件循环导致的"关闭后打不开 / 关闭卡死"问题。
static CMD_TX: Mutex<Option<Sender<UiCmd>>> = Mutex::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);

/// 幂等地启动常驻设置线程（窗口初始隐藏，收到 Show 后显示）。
pub fn ensure_started(settings: Arc<Mutex<Settings>>, hwnd: HWND) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let hwnd_usize = hwnd as usize; // HWND 非 Send，跨线程用 usize 传递
    let (tx, rx) = channel();
    *CMD_TX.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    std::thread::spawn(move || settings_thread(rx, settings, hwnd_usize));
}

/// 显示设置窗口（幂等）。原生 ShowWindow 优先，可靠且不依赖事件循环唤醒。
pub fn show() {
    let h = SETTINGS_HWND.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            ShowWindow(h as HWND, SW_SHOW);
            SetForegroundWindow(h as HWND);
        }
        return;
    }
    // 首次：窗口尚未创建（设置线程刚启动），发命令由设置线程显示。
    if let Some(tx) = CMD_TX.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(UiCmd::Show);
    }
}

/// 请求关闭设置线程（overlay 退出时调用）。
pub fn quit() {
    if let Some(tx) = CMD_TX.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(UiCmd::Quit);
    }
}

/// 常驻设置线程：创建窗口后 run_native 阻塞运行事件循环，
/// 通过命令通道控制显示/隐藏。
fn settings_thread(rx: Receiver<UiCmd>, settings: Arc<Mutex<Settings>>, hwnd_usize: usize) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let native_options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([460.0, 780.0])
                .with_title("Curosu 设置")
                // 隐藏系统标题栏，改由顶部自绘标题栏接管（拖动/最小化/关闭）。
                .with_decorations(false)
                // 无装饰窗口仍可拖拽边缘缩放：winit 保留 WS_SIZEBOX 缩放边框，
                // 仅通过 WM_NCCALCSIZE 抹掉可见边框。
                .with_resizable(true)
                .with_min_inner_size([380.0, 480.0])
                .with_minimize_button(true)
                .with_maximize_button(false)
                // 初始隐藏，由 open_settings 发送 Show 后再显示。
                .with_visible(false),
            // 主线程已被覆盖层消息循环占用；winit 默认拒绝在
            // 非主线程创建事件循环，需显式 any_thread。
            event_loop_builder: Some(Box::new(|builder| {
                #[cfg(windows)]
                {
                    use winit::platform::windows::EventLoopBuilderExtWindows;
                    builder.with_any_thread(true);
                }
                let _ = builder;
            })),
            ..Default::default()
        };
        let mut native_options = native_options;
        if let Some(icon) = load_app_icon() {
            native_options.viewport = native_options.viewport.with_icon(icon);
        }
        let app_creator = move |cc: &eframe::CreationContext<'_>| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            Ok(Box::new(SettingsApp {
                settings: settings.clone(),
                hwnd: hwnd_usize as HWND,
                rx,
                settings_hwnd_found: false,
                selectable_windows: enumerate_visible_windows(),
                selected_window_executable: None,
            }) as Box<dyn eframe::App>)
        };
        match eframe::run_native("Curosu", native_options, Box::new(app_creator)) {
            Err(e) => log(&format!("settings_ui: run_native error: {e:?}")),
            Ok(()) => {}
        }
    }));
    if let Err(e) = result {
        log(&format!("settings_ui: eframe thread panicked: {e:?}"));
    }
    log("settings_ui: settings thread exited");
}
