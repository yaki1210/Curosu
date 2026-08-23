//! 托盘图标 + 弹出菜单。移植自 C# 的 NotifyIcon + ContextMenuStrip。

use crate::log::log;
use crate::overlay::{MSG_EXIT, MSG_OPEN_SETTINGS, MSG_TOGGLE_CURSOR, WM_TRAY};
use std::ffi::c_void;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

static mut ICON_LOADED: bool = false;

fn load_tray_icon() -> *mut c_void {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        LoadIconW(hinst, 1 as *const u16)
    }
}

/// 添加托盘图标。返回是否成功。
pub fn add(hwnd: HWND) -> bool {
    unsafe {
        let icon = load_tray_icon();
        let tip: Vec<u16> = "Curosu\0".encode_utf16().collect();
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = icon;
        for (i, c) in tip.iter().enumerate() {
            nid.szTip[i] = *c;
        }
        let ok = Shell_NotifyIconW(NIM_ADD, &nid) != 0;
        if ok {
            ICON_LOADED = true;
            log(&format!("tray: NIM_ADD ok hIcon={icon:?}"));
        } else {
            log("tray: Shell_NotifyIconW NIM_ADD failed");
        }
        ok
    }
}

/// 移除托盘图标。
pub fn remove(hwnd: HWND) {
    unsafe {
        if !ICON_LOADED {
            return;
        }
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        Shell_NotifyIconW(NIM_DELETE, &nid);
        ICON_LOADED = false;
    }
}

/// 弹出右键菜单。菜单项通过 PostMessage 反馈给窗口。
pub fn show_menu(hwnd: HWND, cursor_enabled: bool) {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let s1: Vec<u16> = "设置\0".encode_utf16().collect();
        let toggle_label = if cursor_enabled {
            "关闭光标"
        } else {
            "打开光标"
        };
        let s2: Vec<u16> = format!("{toggle_label}\0").encode_utf16().collect();
        let s3: Vec<u16> = "退出\0".encode_utf16().collect();
        AppendMenuW(menu, MF_STRING, 1001, s1.as_ptr());
        AppendMenuW(menu, MF_STRING, 1002, s2.as_ptr());
        AppendMenuW(menu, MF_STRING, 1003, s3.as_ptr());

        // 在当前鼠标位置弹出（此前传 CW_USEDEFAULT 导致菜单出现在左上角）。
        let mut pt: windows_sys::Win32::Foundation::POINT = std::mem::zeroed();
        if GetCursorPos(&mut pt) == 0 {
            pt.x = 100;
            pt.y = 100;
        }
        // 托盘菜单标准协议：弹菜单前把窗口提前台，菜单消失后补 WM_NULL，
        // 保证点击菜单外任意处能正常关闭。
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        ) as u32;
        PostMessageW(hwnd, WM_NULL, 0, 0);

        match cmd {
            1001 => {
                PostMessageW(hwnd, MSG_OPEN_SETTINGS, 0, 0);
            }
            1002 => {
                PostMessageW(hwnd, MSG_TOGGLE_CURSOR, 0, 0);
            }
            1003 => {
                PostMessageW(hwnd, MSG_EXIT, 0, 0);
            }
            _ => {}
        }
        DestroyMenu(menu);
    }
}
