//! 全局热键 Ctrl+9，以及「收进后台」的窗口显隐。
//!
//! 两个坑：
//!
//! 1. egui 只拿得到窗口有焦点时的按键，所以要在别的程序里也能呼出面板，
//!    必须向系统注册热键。Windows 上是 `RegisterHotKey`，且它要求消息循环
//!    跑在注册热键的那个线程里，所以这里单开一个线程死等消息。
//!
//! 2. 窗口一旦隐藏，winit 就不再派发重绘事件，`eframe::App::update` 根本
//!    不会被调用 —— 也就是说主线程这时是睡死的，靠它自己是醒不过来的。
//!    所以显隐必须由热键线程直接调 `ShowWindow` 完成，不能走
//!    `ViewportCommand::Visible`。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 热键被按下的标志位，仅用于把主线程叫醒后清账。
pub type Signal = Arc<AtomicBool>;

pub fn new_signal() -> Signal {
    Arc::new(AtomicBool::new(false))
}

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, SetActiveWindow, MOD_CONTROL, MOD_NOREPEAT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetMessageW, GetWindow, GetWindowRect, GetWindowTextLengthW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow,
        GW_OWNER, MSG, SW_HIDE, SW_RESTORE, SW_SHOW, WM_HOTKEY,
    };

    /// 缓存主窗口句柄。HWND 不是 Send，所以按整数存。
    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

    const HOTKEY_ID: i32 = 0x5749;
    const VK_9: u32 = 0x39;

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        // winit 会在同一个进程里开好几个带标题的顶层窗口（实测有 8 个），
        // 真正那一个的区别是「此刻可见」—— 查找只在窗口还亮着的时候发生，
        // 所以用可见性把辅助窗口筛掉。
        if pid == lparam as u32
            && unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
            && unsafe { GetWindowTextLengthW(hwnd) } > 0
            && unsafe { IsWindowVisible(hwnd) } != 0
        {
            MAIN_HWND.store(hwnd as isize, Ordering::SeqCst);
            return 0; // 找到就停
        }
        1
    }

    /// 找到并记住主窗口。只有在窗口可见时才找得到，找不到就返回 null。
    fn main_hwnd() -> HWND {
        let cached = MAIN_HWND.load(Ordering::SeqCst);
        if cached != 0 {
            return cached as HWND;
        }
        // SAFETY: 常规 Win32 枚举，回调只写一个原子量
        unsafe {
            let pid = GetCurrentProcessId();
            EnumWindows(Some(enum_proc), pid as LPARAM);
        }
        MAIN_HWND.load(Ordering::SeqCst) as HWND
    }

    /// 把窗口裁成圆角矩形。
    ///
    /// 光靠「透明背景 + 只画个圆角矩形」是不够的：四个角外面那些像素仍然
    /// 属于本窗口，点上去会被本窗口吃掉，底下的程序点不到。用 `SetWindowRgn`
    /// 之后窗口在系统层面就真的是这个形状，角外的点击会穿透过去。
    ///
    /// `radius_px` 是物理像素 —— 调用方要自己乘过 DPI 缩放，
    /// 不然高 DPI 屏上圆角会明显偏小。
    pub fn apply_window_shape(radius_px: i32) {
        use std::sync::atomic::AtomicU64;
        // 尺寸和圆角都没变才跳过，省得每帧建一个 region。
        // 用 64 位分段存，别让三个量挤在 32 位里互相串位 ——
        // 串位会导致跨屏换分辨率之后形状没跟着更新。
        static LAST: AtomicU64 = AtomicU64::new(0);

        let hwnd = main_hwnd();
        if hwnd.is_null() {
            return;
        }
        // SAFETY: 都是对自己窗口的常规调用；region 交给 SetWindowRgn 之后
        // 由系统负责释放，这里不能再 DeleteObject
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) == 0 {
                return;
            }
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            if w <= 0 || h <= 0 {
                return;
            }
            let key = ((w as u64) << 40) | ((h as u64 & 0xFF_FFFF) << 16) | (radius_px as u64 & 0xFFFF);
            if LAST.swap(key, Ordering::SeqCst) == key {
                return;
            }
            if radius_px <= 0 {
                // 全屏/最大化时不要圆角，否则屏幕四角会被抠掉一块露出桌面。
                // 传 null 表示「取消自定义形状」，窗口恢复成整块矩形。
                SetWindowRgn(hwnd, std::ptr::null_mut(), 1);
                return;
            }
            // GDI 的圆角是按「直径」给的
            let d = (radius_px * 2).clamp(0, w.min(h));
            let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, d, d);
            if !rgn.is_null() {
                SetWindowRgn(hwnd, rgn, 1);
            }
        }
    }

    pub fn is_visible() -> bool {
        let hwnd = main_hwnd();
        // 还没认出窗口时当作可见，免得启动那几帧被当成隐藏而不画东西
        hwnd.is_null() || unsafe { IsWindowVisible(hwnd) } != 0
    }

    pub fn hide() {
        let hwnd = main_hwnd();
        if !hwnd.is_null() {
            unsafe { ShowWindow(hwnd, SW_HIDE) };
        }
    }

    fn show() {
        let hwnd = main_hwnd();
        if hwnd.is_null() {
            return;
        }
        // SAFETY: 都是对自己进程窗口的常规调用
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            // 热键的注册方有权抢前台，这里正好是被热键触发的
            SetForegroundWindow(hwnd);
            SetActiveWindow(hwnd);
        }
    }

    pub fn spawn(ctx: egui::Context, signal: super::Signal) -> bool {
        use std::sync::mpsc;

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // SAFETY: RegisterHotKey 要求消息循环和注册在同一线程，这里满足；
            // MSG 允许全零初始化，GetMessageW 只写我们自己栈上这份。
            unsafe {
                let ok = RegisterHotKey(
                    std::ptr::null_mut(),
                    HOTKEY_ID,
                    MOD_CONTROL | MOD_NOREPEAT,
                    VK_9,
                ) != 0;
                let _ = tx.send(ok);
                if !ok {
                    // 多半被别的程序占了；窗口内的 Ctrl+9 仍然可用
                    return;
                }

                let mut msg: MSG = std::mem::zeroed();
                while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                    if msg.message == WM_HOTKEY {
                        // 以窗口的真实状态为准，不自己记账，免得和用户点 X 的操作对不上
                        if is_visible() {
                            hide();
                        } else {
                            show();
                            signal.store(true, Ordering::SeqCst);
                            ctx.request_repaint();
                        }
                    }
                }
            }
        });

        rx.recv().unwrap_or(false)
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    pub fn spawn(_ctx: egui::Context, _signal: super::Signal) -> bool {
        false // 其它平台暂时只有窗口内的 Ctrl+9
    }
    pub fn is_visible() -> bool {
        true
    }
    pub fn hide() {}
    /// 非 Windows 平台暂时不裁窗口形状，圆角只是画出来的
    pub fn apply_window_shape(_radius_px: i32) {}
}

pub use imp::{apply_window_shape, hide, is_visible, spawn};
