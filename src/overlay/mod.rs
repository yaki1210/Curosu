//! 覆盖层窗口：跟随鼠标的 160px 点击穿透置顶窗口 + 帧循环 + 悬停检测 + 任务栏预览修复。
//! 移植自 C# MainWindow.cs。

pub mod anim;
pub mod hook;
pub mod render;

use crate::audio::TapPlayer;
use crate::log::log;
use crate::settings::Settings;
use crate::system_cursor;
use anim::{CursorAnim, CursorGeometry};
use render::{decode_png, Compositor, CursorTextures};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{MonitorFromPoint, HMONITOR, MONITOR_DEFAULTTONEAREST};
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FindWindowW, GetAncestor, GetClassNameW,
    GetCursorInfo, GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindow,
    GetWindowLongPtrW, GetWindowRect, IsWindowVisible, KillTimer, LoadIconW, PostQuitMessage,
    RegisterClassW, SetTimer, SetWindowPos, ShowWindow, TranslateMessage, WindowFromPoint,
    CS_HREDRAW, CS_VREDRAW, CURSORINFO, GA_ROOT, GWL_STYLE, GW_HWNDNEXT, HWND_TOPMOST, MSG,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WM_APP,
    WM_CONTEXTMENU, WM_CREATE, WM_DESTROY, WM_DPICHANGED, WM_LBUTTONUP, WM_PAINT, WM_RBUTTONUP,
    WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};

const CURSOR_PNG: &[u8] = include_bytes!("../../assets/cursor.png");
const ADDITIVE_PNG: &[u8] = include_bytes!("../../assets/cursor-additive.png");

pub const WM_TRAY: u32 = WM_APP;
pub const MSG_TOGGLE_CURSOR: u32 = WM_APP + 1;
pub const MSG_OPEN_SETTINGS: u32 = WM_APP + 2;
pub const MSG_EXIT: u32 = WM_APP + 3;
pub const MSG_SETTINGS_CHANGED: u32 = WM_APP + 4;

const FRAME_MS: u32 = 8;

struct Overlay {
    hwnd: HWND,
    compositor: Compositor,
    textures: CursorTextures,
    geom: CursorGeometry,
    anim: CursorAnim,
    settings: Arc<Mutex<Settings>>,
    tap: TapPlayer,
    hover: TapPlayer,

    cursor_enabled: bool,
    force_topmost: bool,
    dpi_scale: f64,
    dpi_monitor: HMONITOR,
    down_start: (i32, i32),
    last_cursor_handle: *mut core::ffi::c_void,
    baseline_normal_handle: *mut core::ffi::c_void,
    was_hovering: bool,
    was_hover_candidate: bool,
    was_resize_prompt: bool,
    last_hover_sound_s: f64,
    last_frame_time: f64,
    last_window: (i32, i32, i32, i32),
    last_foreground: HWND,
    last_z_order_refresh_s: f64,
    last_preview_refresh_s: f64,
    frame_ready: bool,
    mouse_hook_active: bool,
    hook_events: u64,
    hook_alive_s: f64,
    health_pos: (i32, i32),
}

static OVERLAY: Mutex<Option<usize>> = Mutex::new(None);

extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                return 0;
            }
            WM_TIMER => {
                if let Some(o) = overlay_ptr() {
                    (*o).frame();
                }
                return 0;
            }
            WM_PAINT => {
                if let Some(o) = overlay_ptr() {
                    (*o).frame();
                }
                return 0;
            }
            WM_DPICHANGED => {
                // DPI 每帧按光标所在显示器求值（update_dpi），不依赖此事件
                // （分层 + NOACTIVATE 窗口该事件触发不可靠）。
                if let Some(o) = overlay_ptr() {
                    (*o).force_topmost = true;
                }
                return 0;
            }
            WM_TRAY => {
                if let Some(o) = overlay_ptr() {
                    (*o).handle_tray(lparam as u32);
                }
                return 0;
            }
            MSG_TOGGLE_CURSOR => {
                if let Some(o) = overlay_ptr() {
                    let enabled = !(*o).cursor_enabled;
                    (*o).toggle_enabled(enabled);
                }
                return 0;
            }
            MSG_OPEN_SETTINGS => {
                if let Some(o) = overlay_ptr() {
                    (*o).open_settings();
                }
                return 0;
            }
            MSG_EXIT => {
                PostQuitMessage(0);
                return 0;
            }
            MSG_SETTINGS_CHANGED => {
                if let Some(o) = overlay_ptr() {
                    (*o).reapply_settings();
                }
                return 0;
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                return 0;
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn overlay_ptr() -> Option<*mut Overlay> {
    let g = OVERLAY.lock().unwrap_or_else(|e| e.into_inner());
    g.filter(|p| *p != 0).map(|p| p as *mut Overlay)
}

/// 主入口：创建覆盖层并运行消息循环，直到收到退出。
pub fn run(settings: Arc<Mutex<Settings>>, tap: TapPlayer, hover: TapPlayer) {
    unsafe {
        // 显式把本线程设为 Per-Monitor-V2：无论 manifest/兼容性覆盖如何，
        // 本线程所有坐标（GetCursorPos/SetWindowPos/钩子 pt）与 DPI 均按
        // 物理像素处理，避免定位与渲染比例不一致导致的右下偏移。
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        // 提高系统计时器精度到 1ms：SetTimer(8ms) 默认受 15.6ms 计时器周期
        // 限制（实际 ~64fps），会拖累弹性回弹快扫段的渲染帧数。与原版
        // WPF 渲染时钟（vsync 60~144Hz）拉齐后转圈动画更流畅。
        timeBeginPeriod(1);

        let hinst = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        let class_name: Vec<u16> = "CurosuOverlay\0".encode_utf16().collect();
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: LoadIconW(hinst, 1 as *const u16),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        RegisterClassW(&wc);

        let geom = {
            let s = settings.lock().unwrap_or_else(|e| e.into_inner());
            anim::geometry_for_width(s.cursor_width)
        };
        let win_w = geom.window_size.ceil() as i32;
        let win_h = geom.window_size.ceil() as i32;

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            0,
            0,
            win_w,
            win_h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            hinst,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            log("overlay: CreateWindowExW failed");
            return;
        }

        let textures = CursorTextures {
            cursor: decode_png(CURSOR_PNG).expect("cursor.png"),
            additive: decode_png(ADDITIVE_PNG).expect("cursor-additive.png"),
        };
        let Some(compositor) = Compositor::new(win_w as u32, win_h as u32) else {
            log("overlay: compositor failed");
            return;
        };

        let mut overlay = Box::new(Overlay {
            hwnd,
            compositor,
            textures,
            geom,
            anim: CursorAnim::default(),
            settings,
            tap,
            hover,
            cursor_enabled: true,
            force_topmost: true,
            dpi_scale: 1.0,
            dpi_monitor: std::ptr::null_mut(),
            down_start: (0, 0),
            last_cursor_handle: std::ptr::null_mut(),
            baseline_normal_handle: std::ptr::null_mut(),
            was_hovering: false,
            was_hover_candidate: false,
            was_resize_prompt: false,
            last_hover_sound_s: f64::NEG_INFINITY,
            last_frame_time: 0.0,
            last_window: (i32::MIN, i32::MIN, 0, 0),
            last_foreground: std::ptr::null_mut(),
            last_z_order_refresh_s: f64::NEG_INFINITY,
            last_preview_refresh_s: f64::NEG_INFINITY,
            frame_ready: false,
            mouse_hook_active: false,
            hook_events: 0,
            hook_alive_s: f64::NEG_INFINITY,
            health_pos: (i32::MIN, i32::MIN),
        });

        // 共享指针给 WndProc（存为 usize 以满足 Send）
        *OVERLAY.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(&mut *overlay as *mut Overlay as usize);

        // 托盘图标
        crate::tray::add(hwnd);

        // 安装系统光标替换 + 鼠标钩子（钩子失败时帧循环自动回退轮询）
        if system_cursor::install() {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            overlay.mouse_hook_active = hook::install();
            hook::init_position();
            overlay.hook_events = hook::event_count();
            overlay.hook_alive_s = now_seconds();
        }
        SetTimer(hwnd, 1, FRAME_MS, None);
        overlay.force_topmost = true;

        // 首次启动自动打开设置
        if !crate::settings::exists() {
            overlay.open_settings();
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // 清理
        KillTimer(hwnd, 1);
        timeEndPeriod(1);
        crate::settings_ui::quit(); // 通知设置线程退出
        hook::uninstall();
        system_cursor::restore();
        crate::tray::remove(hwnd);
        *OVERLAY.lock().unwrap_or_else(|e| e.into_inner()) = None;
        drop(overlay);
    }
}

impl Overlay {
    fn frame(&mut self) {
        if !self.cursor_enabled {
            return;
        }
        let now = now_seconds();
        let mut dt = now - self.last_frame_time;
        self.last_frame_time = now;
        if dt <= 0.0 || dt > 0.1 {
            dt = 1.0 / 60.0;
        }
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground != self.last_foreground {
                self.last_foreground = foreground;
                self.force_topmost = true;
            }
            // 开始菜单、任务栏预览和部分全屏窗口会重新整理 Z 序。
            // 定期补一次 TOPMOST，解决光标停住时自绘层被遮住的问题。
            if now - self.last_z_order_refresh_s >= 0.20 || IsWindowVisible(self.hwnd) == 0 {
                self.force_topmost = true;
            }
        }
        // 位置与钩子存活解耦：钩子被系统静默摘除时位置仍每帧刷新。
        hook::poll_position();
        // 按光标所在显示器实时求 DPI（修复跨显示器/事件丢失时的右下偏移）。
        self.update_dpi();
        self.update_mouse_state();
        // 钩子健康监测：超时无回调且光标在动 → 自动重装。
        self.check_hook_health(now);
        let (cx, cy) = hook::cursor_pos();
        let (dx, dy) = (cx - self.down_start.0, cy - self.down_start.1);
        let previous_anim = self.anim;
        self.anim.update(dt, dx as f64, dy as f64);
        self.render_frame(self.anim.visual_changed_from(&previous_anim), now);
    }

    /// 按光标所在显示器求有效 DPI，按 HMONITOR 缓存。
    /// 窗口物理定位与合成器尺寸共用该 scale，跨显示器一帧内自愈。
    fn update_dpi(&mut self) {
        let (cx, cy) = hook::cursor_pos();
        unsafe {
            let mon = MonitorFromPoint(
                windows_sys::Win32::Foundation::POINT { x: cx, y: cy },
                MONITOR_DEFAULTTONEAREST,
            );
            if !mon.is_null() && mon != self.dpi_monitor {
                let mut dx: u32 = 96;
                let mut dy: u32 = 96;
                if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) == 0 {
                    let next_scale = dx as f64 / 96.0;
                    if (next_scale - self.dpi_scale).abs() > 0.001 {
                        self.dpi_scale = next_scale;
                        self.frame_ready = false;
                        self.force_topmost = true;
                    }
                    self.dpi_monitor = mon;
                }
            }
        }
    }

    /// 钩子被系统静默摘除（LowLevelHooksTimeout 超时）后不自愈会导致
    /// 点击事件丢失。这里检测"光标在动但超过 20 秒无任何钩子回调"并重装。
    fn check_hook_health(&mut self, now: f64) {
        let count = hook::event_count();
        if count != self.hook_events {
            self.hook_events = count;
            self.hook_alive_s = now;
        }
        let (cx, cy) = hook::cursor_pos();
        let moved = (cx, cy) != self.health_pos;
        self.health_pos = (cx, cy);

        if self.mouse_hook_active && moved && now - self.hook_alive_s > 20.0 {
            log("mouse hook stale >20s while cursor moved, reinstalling");
            hook::uninstall();
            self.mouse_hook_active = hook::install();
            hook::init_position();
            self.hook_events = hook::event_count();
            self.hook_alive_s = now;
        }
    }

    fn update_mouse_state(&mut self) {
        if self.mouse_hook_active {
            if hook::take_press() {
                self.begin_press();
            }
            if hook::take_release() {
                self.end_press();
            }
        } else {
            // 回退：轮询 GetCursorInfo + GetAsyncKeyState
            let pressed = unsafe { (GetAsyncKeyState(0x01) as i32 & 0x8000) != 0 };
            if pressed && !self.anim.mouse_down {
                self.begin_press();
            } else if !pressed && self.anim.mouse_down {
                self.end_press();
            }
        }

        // Win 键按下强制置顶
        let win_pressed = unsafe {
            (GetAsyncKeyState(0x5B) as i32 & 0x8000) != 0
                || (GetAsyncKeyState(0x5C) as i32 & 0x8000) != 0
        };
        if win_pressed {
            self.force_topmost = true;
        }

        self.update_drag();
        self.update_hover();
    }

    fn begin_press(&mut self) {
        self.anim.begin_press();
        // 钩子激活时用事件级坐标（对齐 C# BeginPress(data.pt)）；
        // 回退轮询模式没有按下事件，只能取当前光标位置。
        let (cx, cy) = if self.mouse_hook_active {
            hook::press_pos()
        } else {
            hook::cursor_pos()
        };
        self.down_start = (cx, cy);
        self.force_topmost = true;
        self.play_tap(1.0);
    }

    fn end_press(&mut self) {
        self.anim.end_press();
        self.play_tap(0.8);
        self.force_topmost = true;
    }

    fn update_drag(&mut self) {
        if self.anim.mouse_down && !self.anim.drag_active {
            let (cx, cy) = hook::cursor_pos();
            let dx = cx - self.down_start.0;
            let dy = cy - self.down_start.1;
            let threshold = self.geom.cursor_width * self.dpi_scale;
            if (dx * dx + dy * dy) as f64 > threshold * threshold {
                self.anim.drag_active = true;
            }
        }
    }

    fn update_hover(&mut self) {
        let mut info: CURSORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        if unsafe { GetCursorInfo(&mut info) } == 0 {
            return;
        }
        let normal_handle = system_cursor::get_blank_handle(system_cursor::OCR_NORMAL);
        let hand_handle = system_cursor::get_blank_handle(system_cursor::OCR_HAND);

        if info.hCursor != self.last_cursor_handle {
            self.last_cursor_handle = info.hCursor;
            self.force_topmost = true;
        }

        // 与原版一致：只有被替换后的 OCR_HAND 句柄触发悬停样式。
        let pointer_hover = !info.hCursor.is_null() && info.hCursor == hand_handle;
        self.anim.pointer_hover = pointer_hover;

        let resize_prompt_mode = {
            let g = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            g.hover_sound_as_resize_prompt
        };
        if resize_prompt_mode {
            let resize = self.is_resize_cursor(info.ptScreenPos.x, info.ptScreenPos.y);
            if resize && !self.was_resize_prompt && !self.anim.mouse_down {
                self.play_hover();
            }
            self.was_resize_prompt = resize;
        } else {
            if self.baseline_normal_handle.is_null()
                && !info.hCursor.is_null()
                && info.hCursor != hand_handle
            {
                self.baseline_normal_handle = info.hCursor;
            }
            let is_hover_candidate = pointer_hover
                || (!info.hCursor.is_null()
                    && info.hCursor != normal_handle
                    && info.hCursor != self.baseline_normal_handle);
            if is_hover_candidate && !self.was_hover_candidate && !self.anim.mouse_down {
                self.play_hover();
            }
            if !is_hover_candidate {
                self.baseline_normal_handle = if info.hCursor == normal_handle {
                    normal_handle
                } else {
                    info.hCursor
                };
            }
            self.was_hover_candidate = is_hover_candidate;
            self.was_hovering = pointer_hover;
        }
    }

    fn is_resize_cursor(&self, px: i32, py: i32) -> bool {
        unsafe {
            let window = WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: px, y: py });
            if window.is_null() {
                return false;
            }
            let root = GetAncestor(window, GA_ROOT);
            if root.is_null() || root == self.hwnd {
                return false;
            }
            let style = GetWindowLongPtrW(root, GWL_STYLE);
            let ws_maximize: isize = 0x01000000;
            let ws_thickframe: isize = 0x00040000;
            if (style & ws_maximize) != 0 || (style & ws_thickframe) == 0 {
                return false;
            }
            let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
            if GetWindowRect(root, &mut rect) == 0 {
                return false;
            }
            let border_x = GetSystemMetrics(32).max(1);
            let border_y = GetSystemMetrics(33).max(1);
            px <= rect.left + border_x
                || px >= rect.right - border_x
                || py <= rect.top + border_y
                || py >= rect.bottom - border_y
        }
    }

    fn play_tap(&self, base_freq: f64) {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        if !settings.tap_sound_enabled || settings.tap_sound_volume <= 0.0 {
            return;
        }
        let freq = base_freq - 0.01 + rand_f() * 0.02;
        let volume = base_freq * settings.tap_sound_volume;
        let balance = self.get_balance();
        self.tap.play(freq, volume, balance);
    }

    fn play_hover(&mut self) {
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        if !settings.hover_sound_enabled || settings.hover_sound_volume <= 0.0 {
            return;
        }
        let now = now_seconds();
        if now - self.last_hover_sound_s < 0.02 {
            return;
        }
        self.last_hover_sound_s = now;
        let freq = 0.99 + rand_f() * 0.02;
        let balance = self.get_balance();
        self.hover.play(freq, settings.hover_sound_volume, balance);
    }

    fn get_balance(&self) -> f64 {
        // 虚拟屏幕宽度内做声像
        let (cx, _) = hook::cursor_pos();
        let vleft = unsafe { GetSystemMetrics(76) };
        let vwidth = unsafe { GetSystemMetrics(78) }.max(1);
        let x_dip = cx as f64 / self.dpi_scale;
        (((x_dip - vleft as f64) / vwidth as f64) * 2.0 - 1.0).clamp(-0.6, 0.6)
    }

    fn render_frame(&mut self, visual_changed: bool, now: f64) {
        let ps = self.geom.window_size * self.dpi_scale;
        let win_w = ps.ceil() as u32;
        let win_h = ps.ceil() as u32;
        let mut redraw = visual_changed;
        if self.compositor.w != win_w || self.compositor.h != win_h {
            if let Some(c) = Compositor::new(win_w, win_h) {
                self.compositor = c;
                redraw = true;
            }
        }
        let pgeom = CursorGeometry {
            cursor_width: self.geom.cursor_width * self.dpi_scale,
            cursor_height: self.geom.cursor_height * self.dpi_scale,
            window_size: ps,
            window_margin: self.geom.window_margin * self.dpi_scale,
        };
        let (cx, cy) = hook::cursor_pos();
        let x = cx - (self.geom.window_margin * self.dpi_scale).round() as i32;
        let y = cy - (self.geom.window_margin * self.dpi_scale).round() as i32;
        let cur = (x, y, win_w as i32, win_h as i32);
        if self.force_topmost || cur != self.last_window {
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    win_w as i32,
                    win_h as i32,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            self.last_window = cur;
            if self.force_topmost {
                self.last_z_order_refresh_s = now;
            }
            self.force_topmost = false;
        }
        if redraw || !self.frame_ready {
            self.compositor.draw(&pgeom, &self.anim, &self.textures);
            self.compositor.present(self.hwnd);
            self.frame_ready = true;
        }
        self.try_bring_above_taskbar_preview(now);
    }

    fn try_bring_above_taskbar_preview(&mut self, now: f64) {
        // 节流到 250ms：原版 C# 用 250ms 定时器调用，而这里每帧（8ms）调用时，
        // 命中缩略图会每帧 SetWindowPos 插入其上方，与 DWM 实时预览互相拉扯，
        // 造成卡顿且遮挡不稳定。
        if now - self.last_preview_refresh_s < 0.25 {
            return;
        }
        self.last_preview_refresh_s = now;
        unsafe {
            let (cx, cy) = hook::cursor_pos();
            let preview = WindowFromPoint(windows_sys::Win32::Foundation::POINT { x: cx, y: cy });
            let root = if preview.is_null() {
                std::ptr::null_mut()
            } else {
                GetAncestor(preview, GA_ROOT)
            };
            if is_task_list_thumbnail(root) {
                SetWindowPos(
                    self.hwnd,
                    root,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
                return;
            }
            let name: Vec<u16> = "TaskListThumbnailWnd\0".encode_utf16().collect();
            let mut found = FindWindowW(name.as_ptr(), std::ptr::null());
            while !found.is_null() {
                let mut rect: windows_sys::Win32::Foundation::RECT = std::mem::zeroed();
                if GetWindowRect(found, &mut rect) != 0
                    && cx >= rect.left
                    && cx < rect.right
                    && cy >= rect.top
                    && cy < rect.bottom
                {
                    SetWindowPos(
                        self.hwnd,
                        found,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                    return;
                }
                found = GetWindow(found, GW_HWNDNEXT);
            }
        }
    }

    fn toggle_enabled(&mut self, enabled: bool) {
        if self.cursor_enabled == enabled {
            return;
        }
        self.cursor_enabled = enabled;
        if enabled {
            if !system_cursor::install() {
                self.cursor_enabled = false;
                return;
            }
            self.mouse_hook_active = hook::install();
            hook::init_position();
            self.hook_events = hook::event_count();
            self.hook_alive_s = now_seconds();
            self.health_pos = hook::cursor_pos();
            self.force_topmost = true;
            self.frame_ready = false;
            unsafe { ShowWindow(self.hwnd, SW_SHOWNOACTIVATE) };
        } else {
            hook::uninstall();
            self.mouse_hook_active = false;
            system_cursor::restore();
            unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    fn open_settings(&mut self) {
        // 幂等：常驻设置线程只启动一次，之后通过命令通道显示窗口。
        crate::settings_ui::ensure_started(self.settings.clone(), self.hwnd);
        crate::settings_ui::show();
    }

    fn reapply_settings(&mut self) {
        let s = {
            let g = self.settings.lock().unwrap_or_else(|e| e.into_inner());
            g.clone()
        };
        self.tap.set_enabled(s.tap_sound_enabled);
        self.tap.set_volume(s.tap_sound_volume);
        self.hover.set_enabled(s.hover_sound_enabled);
        self.hover.set_volume(s.hover_sound_volume);
        crate::autostart::apply(s.auto_start);
        // 光标尺寸变更
        let g = anim::geometry_for_width(s.cursor_width);
        if (g.cursor_width - self.geom.cursor_width).abs() > 0.001 {
            self.geom = g;
            self.force_topmost = true;
        }
    }

    fn handle_tray(&mut self, lparam: u32) {
        if lparam == WM_RBUTTONUP as u32 || lparam == WM_CONTEXTMENU as u32 {
            crate::tray::show_menu(self.hwnd, self.cursor_enabled);
        } else if lparam == WM_LBUTTONUP as u32 {
            self.open_settings();
        }
    }
}

fn now_seconds() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn rand_f() -> f64 {
    // 轻量伪随机 0..1
    let t = now_seconds() * 1_000_000_000.0;
    let frac = (t * 2654435761.0).fract();
    frac.abs()
}

unsafe fn is_task_list_thumbnail(hwnd: HWND) -> bool {
    if hwnd.is_null() {
        return false;
    }
    let mut buf = [0u16; 256];
    let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    name == "TaskListThumbnailWnd"
}
