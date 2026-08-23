//! 原版风格设置窗口：三组圆角面板、粉色滑块和自绘开关。
//! 直接读写共享 Settings，变更时保存并通知覆盖层线程重新应用。

use crate::log::log;
use crate::overlay::MSG_SETTINGS_CHANGED;
use crate::settings::Settings;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    PostMessageW, SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
};

const APP_ICON_ICO: &[u8] = include_bytes!("../assets/icon.ico");
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

/// 加载中文字体（Microsoft YaHei）注入 egui。
fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\msyhbd.ttc",
    ];
    for path in candidates.iter() {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("msyh".to_owned(), egui::FontData::from_owned(bytes));
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                f.insert(0, "msyh".to_owned());
            }
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                f.push("msyh".to_owned());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    log("settings_ui: no CJK font found, using default");
}

fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.slider_width = 320.0;

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

fn value_row(ui: &mut egui::Ui, left: &str, right: String) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(left).size(13.0).color(MUTED));
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.label(egui::RichText::new(right).size(13.0).color(TEXT));
            },
        );
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
    let width = ui.available_width().min(320.0);
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::click_and_drag());
    let track = egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 10.0));

    if let Some(pointer) = response.interact_pointer_pos() {
        let progress = ((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0) as f64;
        *value = (min + (max - min) * progress).clamp(min, max);
    }

    let progress = if max > min {
        ((*value - min) / (max - min)).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let thumb_x = egui::lerp(track.left()..=track.right(), progress);
    let fill = egui::Rect::from_min_max(track.left_top(), egui::pos2(thumb_x, track.bottom()));
    let thumb = egui::Rect::from_center_size(
        egui::pos2(thumb_x, rect.center().y),
        egui::vec2(16.0, 26.0),
    );
    let track_stroke = if response.hovered() || response.dragged() {
        egui::Stroke::new(1.0_f32, CONTROL_BORDER)
    } else {
        egui::Stroke::new(1.0_f32, RAIL)
    };
    ui.painter().rect_filled(track, 5.0, SLIDER_TRACK);
    ui.painter().rect_stroke(track, 5.0, track_stroke);
    ui.painter().rect_filled(fill, 5.0, ACCENT);
    ui.painter().rect_filled(thumb, 5.0, ACCENT);
    ui.painter()
        .rect_stroke(
            thumb,
            5.0,
            egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
        );

    if response.hovered() || response.dragged() {
        ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::PointingHand);
    }
}

fn draw_volume(ui: &mut egui::Ui, label: &str, value: &mut f64) {
    value_row(ui, label, format!("{}%", (*value * 100.0).round() as i32));
    draw_slider(ui, value, 0.0..=1.0);
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

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(BACKGROUND)
                    .inner_margin(egui::Margin::same(20.0)),
            )
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("设置")
                        .size(24.0)
                        .strong()
                        .color(ACCENT),
                );
                ui.add_space(18.0);

                section(ui, "光标", |ui| {
                    value_row(ui, "16 - 64", format!("{:.0} px", s.cursor_width));
                    draw_slider(ui, &mut s.cursor_width, 16.0..=64.0);

                    let reset = ui.add_sized(
                        egui::vec2(96.0, 30.0),
                        egui::Button::new(egui::RichText::new("恢复默认").color(TEXT))
                            .fill(ACCENT)
                            .stroke(egui::Stroke::NONE),
                    );
                    if reset.clicked() {
                        s.cursor_width = 30.0;
                    }
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
                    ui.label(
                        egui::RichText::new("选择后添加：该程序全屏时仍显示动画光标")
                            .size(12.0)
                            .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("刷新窗口列表").clicked() {
                            self.refresh_selectable_windows();
                        }

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
                        ui.scope(|ui| {
                            let visuals = &mut ui.style_mut().visuals;
                            visuals.extreme_bg_color = CONTROL_BG;
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
                            visuals.selection.bg_fill = ACCENT;

                            egui::ComboBox::from_id_source("fullscreen_exception_window")
                                .selected_text(selected_label)
                                .width(210.0)
                                .icon(|ui, rect, visuals, is_open, _| {
                                    let center = rect.center();
                                    let direction = if is_open { -1.0 } else { 1.0 };
                                    let left = egui::pos2(center.x - 5.0, center.y - 2.5 * direction);
                                    let middle = egui::pos2(center.x, center.y + 2.5 * direction);
                                    let right = egui::pos2(center.x + 5.0, center.y - 2.5 * direction);
                                    ui.painter().line_segment([left, middle], visuals.fg_stroke);
                                    ui.painter().line_segment([middle, right], visuals.fg_stroke);
                                })
                                .show_ui(ui, |ui| {
                                    for window in &self.selectable_windows {
                                        ui.selectable_value(
                                            &mut self.selected_window_executable,
                                            Some(window.executable.clone()),
                                            &window.label,
                                        );
                                    }
                                });
                        });

                        let can_add = self.selected_window_executable.is_some();
                        if ui
                            .add_enabled(can_add, egui::Button::new("添加"))
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
                .with_resizable(false)
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
