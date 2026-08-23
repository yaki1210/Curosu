//! 系统光标替换：把 14 种标准光标替换为 32x32 空白光标，退出时恢复。
//! 移植自 C# CursorReplacer.cs。

use crate::log::log;
use std::ffi::c_void;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CopyIcon, CreateCursor, DestroyCursor, LoadCursorW, LoadImageW, SetSystemCursor,
    SystemParametersInfoW, IMAGE_CURSOR, LR_DEFAULTCOLOR, SPIF_SENDCHANGE, SPI_SETCURSORS,
};

/// 标准光标 ID（与 C# OCR_* 常量一致）。
pub const OCR_NORMAL: u32 = 32512;
pub const OCR_IBEAM: u32 = 32513;
pub const OCR_WAIT: u32 = 32514;
pub const OCR_CROSS: u32 = 32515;
pub const OCR_UP: u32 = 32516;
pub const OCR_SIZENWSE: u32 = 32642;
pub const OCR_SIZENESW: u32 = 32643;
pub const OCR_SIZEWE: u32 = 32644;
pub const OCR_SIZENS: u32 = 32645;
pub const OCR_SIZEALL: u32 = 32646;
pub const OCR_NO: u32 = 32648;
pub const OCR_HAND: u32 = 32649;
pub const OCR_APPSTARTING: u32 = 32650;
pub const OCR_HELP: u32 = 32651;

const CURSOR_IDS: [u32; 14] = [
    OCR_NORMAL,
    OCR_IBEAM,
    OCR_WAIT,
    OCR_CROSS,
    OCR_UP,
    OCR_SIZENWSE,
    OCR_SIZENESW,
    OCR_SIZEWE,
    OCR_SIZENS,
    OCR_SIZEALL,
    OCR_NO,
    OCR_HAND,
    OCR_APPSTARTING,
    OCR_HELP,
];

const UAC_FALLBACK_CURSOR_RESOURCE_ID: u16 = 2;

static mut BLANK_HANDLES: [*mut c_void; 14] = [std::ptr::null_mut(); 14];
static mut INSTALLED: bool = false;
static mut UAC_FALLBACK_INSTALLED: bool = false;

/// 已替换的空白光标句柄是否包含指定 id。
pub fn get_blank_handle(cursor_id: u32) -> *mut c_void {
    unsafe {
        for (i, id) in CURSOR_IDS.iter().enumerate() {
            if *id == cursor_id {
                return BLANK_HANDLES[i];
            }
        }
    }
    std::ptr::null_mut()
}

/// 安装：将所有标准光标替换为空白光标。
pub fn install() -> bool {
    unsafe {
        if INSTALLED {
            return true;
        }
        let mut installed_any = false;
        for (i, id) in CURSOR_IDS.iter().enumerate() {
            let blank = create_blank_cursor();
            if blank.is_null() {
                log(&format!("CreateCursor failed for id={id}"));
                continue;
            }
            if SetSystemCursor(blank, *id) == 0 {
                log(&format!("SetSystemCursor failed for id={id}"));
                DestroyCursor(blank);
                continue;
            }
            log(&format!("Hidden system cursor id={id}"));
            // SetSystemCursor 成功后会销毁 blank；重新取得共享系统句柄，
            // 供 GetCursorInfo 的悬停判断使用，不能保存已销毁的 blank。
            BLANK_HANDLES[i] = LoadCursorW(std::ptr::null_mut(), *id as *const u16);
            if *id == OCR_NORMAL {
                installed_any = true;
            }
        }
        INSTALLED = installed_any;
        log(&format!(
            "system_cursor::install installed={INSTALLED} count={}",
            BLANK_HANDLES.iter().filter(|h| !h.is_null()).count()
        ));
        INSTALLED
    }
}

/// 覆盖层无法绘制的位置（例如 UAC 安全桌面或 DWM 任务栏缩略图）使用的
/// 静态光标。它只替换当前会话的系统光标，调用 `restore` 后会重新载入用户
/// 原来的鼠标方案。资源按覆盖层当前的物理像素宽度加载，保证 UAC 时的
/// 临时指针尺寸与设置中的光标大小一致。
pub fn install_static_fallback(cursor_width_px: f64) -> bool {
    unsafe {
        let hinst = GetModuleHandleW(std::ptr::null());
        let size = cursor_width_px.round().clamp(16.0, 64.0) as i32;
        let source = LoadImageW(
            hinst,
            UAC_FALLBACK_CURSOR_RESOURCE_ID as usize as *const u16,
            IMAGE_CURSOR,
            size,
            size,
            LR_DEFAULTCOLOR,
        );
        if source.is_null() {
            log(&format!("LoadImageW failed for static fallback cursor size={size}"));
            return false;
        }

        let mut installed_normal = false;
        for id in CURSOR_IDS {
            // SetSystemCursor 会销毁传入的句柄，资源句柄必须先复制。
            let cursor = CopyIcon(source);
            if cursor.is_null() {
                log(&format!("CopyIcon failed for static fallback id={id}"));
                continue;
            }
            if SetSystemCursor(cursor, id) == 0 {
                log(&format!(
                    "SetSystemCursor failed for static fallback id={id}"
                ));
                DestroyCursor(cursor);
                continue;
            }
            if id == OCR_NORMAL {
                installed_normal = true;
            }
        }
        DestroyCursor(source);
        log(&format!(
            "system_cursor::install_static_fallback installed={installed_normal} size={size}"
        ));
        UAC_FALLBACK_INSTALLED = installed_normal;
        installed_normal
    }
}

/// 恢复系统光标。
pub fn restore() {
    unsafe {
        if !INSTALLED && !UAC_FALLBACK_INSTALLED {
            return;
        }
        let restored =
            SystemParametersInfoW(SPI_SETCURSORS, 0, std::ptr::null_mut(), SPIF_SENDCHANGE);
        log(&format!("Restore system cursors ok={restored}"));
        if restored == 0 {
            restore_default_cursors();
        }
        for h in BLANK_HANDLES.iter_mut() {
            *h = std::ptr::null_mut();
        }
        INSTALLED = false;
        UAC_FALLBACK_INSTALLED = false;
    }
}

fn restore_default_cursors() {
    unsafe {
        for id in CURSOR_IDS.iter() {
            let original = LoadCursorW(std::ptr::null_mut(), *id as *const u16);
            if original.is_null() {
                continue;
            }
            let copy = CopyIcon(original);
            if !copy.is_null() {
                SetSystemCursor(copy, *id);
            }
        }
        log("Restored cursors from default system cursor handles.");
    }
}

fn create_blank_cursor() -> *mut c_void {
    // and_mask 全 1（所有像素透明），xor_mask 全 0
    let and_mask = [0xFFu8; 128];
    let xor_mask = [0u8; 128];
    unsafe {
        CreateCursor(
            std::ptr::null_mut(),
            0,
            0,
            32,
            32,
            and_mask.as_ptr() as *const c_void,
            xor_mask.as_ptr() as *const c_void,
        )
    }
}
