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

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateRoundRectRgn, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint,
        MonitorFromWindow, SetWindowRgn, HDC, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        MONITOR_DEFAULTTONULL,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, SetActiveWindow, MOD_CONTROL, MOD_NOREPEAT,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetMessageW, GetWindow, GetWindowRect, GetWindowTextLengthW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow, SetWindowPos,
        ShowWindow, GW_OWNER, MSG, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_RESTORE, SW_SHOW,
        WM_HOTKEY,
    };

    /// 缓存主窗口句柄。HWND 不是 Send，所以按整数存。
    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);

    const HOTKEY_ID: i32 = 0x5749;
    const VK_9: u32 = 0x39;

    #[derive(Clone, Copy)]
    struct Monitor {
        handle: HMONITOR,
        rect: RECT,
        work: RECT,
    }

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

    unsafe extern "system" fn enum_monitor(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let monitors = unsafe { &mut *(data as *mut Vec<Monitor>) };
        let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(hmonitor, &mut mi) } != 0 {
            monitors.push(Monitor {
                handle: hmonitor,
                rect: mi.rcMonitor,
                work: mi.rcWork,
            });
        }
        1
    }

    fn monitors() -> Vec<Monitor> {
        let mut out = Vec::new();
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(enum_monitor),
                &mut out as *mut _ as LPARAM,
            );
        }
        out
    }

    fn monitor_info(handle: HMONITOR) -> Option<Monitor> {
        if handle.is_null() {
            return None;
        }
        let mut mi: MONITORINFO = unsafe { std::mem::zeroed() };
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(handle, &mut mi) } == 0 {
            return None;
        }
        Some(Monitor {
            handle,
            rect: mi.rcMonitor,
            work: mi.rcWork,
        })
    }

    fn rect_width(r: RECT) -> i32 {
        r.right - r.left
    }

    fn rect_height(r: RECT) -> i32 {
        r.bottom - r.top
    }

    fn intersection_area(a: RECT, b: RECT) -> i64 {
        let left = a.left.max(b.left);
        let top = a.top.max(b.top);
        let right = a.right.min(b.right);
        let bottom = a.bottom.min(b.bottom);
        let w = (right - left).max(0) as i64;
        let h = (bottom - top).max(0) as i64;
        w * h
    }

    fn dist_to_rect2(p: POINT, r: RECT) -> i64 {
        let dx = if p.x < r.left {
            r.left - p.x
        } else if p.x >= r.right {
            p.x - r.right + 1
        } else {
            0
        } as i64;
        let dy = if p.y < r.top {
            r.top - p.y
        } else if p.y >= r.bottom {
            p.y - r.bottom + 1
        } else {
            0
        } as i64;
        dx * dx + dy * dy
    }

    /// 选窗口“视觉上所在”的显示器。Win+Shift+方向键把无边框窗口搬到异形多屏
    /// 布局时，窗口可能短暂横跨两块屏；这时如果它和上一块屏、另一块屏都有
    /// 明显重叠，就认为用户正在把它移向另一块屏。
    fn monitor_for_window(hwnd: HWND, r: RECT, previous: Option<HMONITOR>) -> Option<Monitor> {
        let all = monitors();
        let window_area = (rect_width(r).max(0) as i64) * (rect_height(r).max(0) as i64);
        let mut hits: Vec<(Monitor, i64)> = all
            .iter()
            .copied()
            .map(|m| (m, intersection_area(r, m.rect)))
            .filter(|(_, area)| *area > 0)
            .collect();
        hits.sort_by_key(|(_, area)| -*area);

        if let Some(prev) = previous {
            let moved_to_other = hits
                .iter()
                .find(|(m, area)| m.handle != prev && *area * 10 >= window_area)
                .map(|(m, _)| *m);
            if moved_to_other.is_some() {
                return moved_to_other;
            }
        }

        if let Some((mon, _)) = hits.first() {
            return Some(*mon);
        }

        let center = POINT {
            x: r.left + rect_width(r) / 2,
            y: r.top + rect_height(r) / 2,
        };
        let by_point = unsafe { MonitorFromPoint(center, MONITOR_DEFAULTTONULL) };
        if let Some(mon) = monitor_info(by_point) {
            return Some(mon);
        }

        let mut all = all;
        all.sort_by_key(|m| dist_to_rect2(center, m.rect));
        all.into_iter()
            .next()
            .or_else(|| monitor_info(unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) }))
    }

    fn snap_rect_to_monitor_top_center(hwnd: HWND, mon: Monitor, win_w: i32, win_h: i32) -> bool {
        if win_w <= 0 || win_h <= 0 {
            return false;
        }
        let work = mon.work;
        let x = work.left + (rect_width(work) - win_w) / 2;
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                work.top,
                win_w,
                win_h,
                SWP_NOZORDER | SWP_NOACTIVATE,
            ) != 0
        }
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
            let key =
                ((w as u64) << 40) | ((h as u64 & 0xFF_FFFF) << 16) | (radius_px as u64 & 0xFFFF);
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

    /// 把窗口摆到「它当前所在那块显示器」的顶端居中。成功返回 true。
    ///
    /// 必须走 Win32 的显示器 API，不能靠 egui 的 `monitor_size` 自己算：
    /// egui 只给得出当前显示器的**尺寸**，给不出它的**原点**。多屏布局里
    /// 各屏尺寸不同、原点还可能是负数（副屏摆在主屏左边就是负的），
    /// 光有尺寸根本推不出该把窗口放到哪个绝对坐标上。
    /// `GetMonitorInfoW` 直接给出这块屏的工作区矩形，还自动避开任务栏，
    /// 而且全程物理像素，不掺和 DPI 换算。
    pub fn snap_top_center() -> bool {
        let hwnd = main_hwnd();
        if hwnd.is_null() {
            return false;
        }
        // SAFETY: 查询自己的窗口矩形；后续显示器选择走系统枚举结果。
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) == 0 {
                return false;
            }
            let Some(mon) = monitor_for_window(hwnd, r, None) else {
                return false;
            };
            snap_rect_to_monitor_top_center(hwnd, mon, r.right - r.left, r.bottom - r.top)
        }
    }

    pub fn snap_top_center_on(monitor_id: Option<isize>) -> bool {
        let hwnd = main_hwnd();
        if hwnd.is_null() {
            return false;
        }
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) == 0 {
                return false;
            }
            let mon = monitor_id
                .and_then(|id| monitor_info(id as HMONITOR))
                .or_else(|| monitor_for_window(hwnd, r, None));
            let Some(mon) = mon else {
                return false;
            };
            snap_rect_to_monitor_top_center(hwnd, mon, r.right - r.left, r.bottom - r.top)
        }
    }

    /// 当前窗口所在的显示器。只用来判断窗口是不是被系统快捷键挪到另一块屏了。
    pub fn current_monitor_id() -> Option<isize> {
        current_monitor_id_away_from(None)
    }

    pub fn current_monitor_id_away_from(previous: Option<isize>) -> Option<isize> {
        let hwnd = main_hwnd();
        if hwnd.is_null() {
            return None;
        }
        // SAFETY: 查询自己的窗口矩形；显示器句柄值不解引用，只作比较。
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) == 0 {
                return None;
            }
            monitor_for_window(hwnd, r, previous.map(|id| id as HMONITOR))
                .map(|m| m.handle as isize)
        }
    }

    /// 把窗口强行恢复成给定的物理尺寸，并摆回当前显示器顶端居中。
    ///
    /// 走 Win32 而不是 `ViewportCommand::InnerSize` —— 实测窗口塌成几个像素
    /// 之后，eframe 那条路不起作用，窗口再也回不来。这条是最后的救命通道，
    /// 不能依赖上层框架。
    pub fn restore_size(w: i32, h: i32) -> bool {
        let hwnd = main_hwnd();
        if hwnd.is_null() || w <= 0 || h <= 0 {
            return false;
        }
        // SAFETY: 常规 Win32 调用；显示器工作区来自系统 API。
        unsafe {
            let mut r: RECT = std::mem::zeroed();
            let mon = if GetWindowRect(hwnd, &mut r) != 0 {
                monitor_for_window(hwnd, r, None)
            } else {
                monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST))
            };
            if let Some(mon) = mon {
                return snap_rect_to_monitor_top_center(hwnd, mon, w, h);
            }
            false
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

#[cfg(target_os = "macos")]
mod imp {
    //! macOS 版：用 AppKit 操作 NSWindow / NSScreen，全局热键走 Carbon。
    //!
    //! 和 Windows 的两大差异：
    //!
    //! 1. **点击穿透是白送的**。macOS 对非透明窗口里全透明（alpha=0）的像素
    //!    会自动把鼠标事件透传给下层窗口，所以圆角外那圈只要画成透明就已经
    //!    能点穿，不需要像 Windows 那样 `SetWindowRgn`。`apply_window_shape`
    //!    因此基本是空的。
    //!
    //! 2. **不用单开线程等热键**。Carbon 的 `RegisterEventHotKey` 把热键事件
    //!    投递到主线程的事件派发器上，即便窗口隐藏、winit 事件循环 park 住，
    //!    run loop 仍会把它派发过来。所以处理器直接在主线程里显隐窗口，
    //!    AppKit 调用天然安全，不必像 Win32 那样在别的线程调 ShowWindow。

    use std::ffi::c_void;
    use std::sync::atomic::Ordering;

    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSScreen, NSWindow};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    // ------------------------------------------------------------ 找主窗口

    /// winit 只建一个顶层窗口。全程在主线程访问 AppKit，所以不缓存裸指针，
    /// 每次按需从 NSApp 里取：优先 key/main 窗口，隐藏时它俩为空就退回第一个。
    fn main_window(mtm: MainThreadMarker) -> Option<Retained<NSWindow>> {
        let app = NSApplication::sharedApplication(mtm);
        app.keyWindow()
            .or_else(|| app.mainWindow())
            .or_else(|| app.windows().firstObject())
    }

    /// 拿到主窗口后在主线程上跑一段逻辑。不在主线程、或还没有窗口时返回 None。
    fn with_window<R>(f: impl FnOnce(&NSWindow, MainThreadMarker) -> R) -> Option<R> {
        let mtm = MainThreadMarker::new()?;
        let win = main_window(mtm)?;
        Some(f(&win, mtm))
    }

    // ------------------------------------------------------------ 显示器

    /// 用 NSScreen 对象的地址当作显示器 id。同一会话里 AppKit 复用同一批
    /// NSScreen 实例，指针稳定，够用来判断「窗口是不是换了块屏」。
    fn screen_id(screen: &NSScreen) -> isize {
        (screen as *const NSScreen) as isize
    }

    fn find_screen(id: isize, mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
        let screens = NSScreen::screens(mtm);
        (0..screens.count())
            .map(|i| screens.objectAtIndex(i))
            .find(|s| screen_id(s) == id)
    }

    /// 把窗口摆到给定屏幕的可视区（避开菜单栏和 Dock）顶端、水平居中。
    ///
    /// macOS 坐标原点在左下角、y 向上，所以「顶边」是 `origin.y + height`，
    /// 窗口原点要减去自身高度才能让上沿贴着可视区顶部。
    fn place_top_center(w: &NSWindow, screen: Option<Retained<NSScreen>>) -> bool {
        let Some(screen) = screen else {
            return false;
        };
        let vis = screen.visibleFrame();
        let frame = w.frame();
        let x = vis.origin.x + (vis.size.width - frame.size.width) * 0.5;
        let top = vis.origin.y + vis.size.height;
        let y = top - frame.size.height;
        w.setFrameOrigin(NSPoint { x, y });
        true
    }

    // ------------------------------------------------------------ 显隐

    pub fn is_visible() -> bool {
        // 认不出窗口时（启动前几帧）当作可见，别被当成隐藏而不画东西
        with_window(|w, _| w.isVisible()).unwrap_or(true)
    }

    pub fn hide() {
        let _ = with_window(|w, _| w.orderOut(None));
    }

    #[allow(deprecated)] // activateIgnoringOtherApps 自 macOS 14 起弃用，但仍是把
    // 后台窗口抢到前台最稳的做法，替代 API 依赖更高的部署目标
    fn show() {
        let _ = with_window(|w, mtm| {
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            if w.isMiniaturized() {
                w.deminiaturize(None);
            }
            w.makeKeyAndOrderFront(None);
        });
    }

    // ------------------------------------------------------------ 窗口摆位

    /// macOS 不裁窗口形状：透明像素自动放行点击，圆角外无需额外处理。
    pub fn apply_window_shape(_radius_px: i32) {}

    pub fn snap_top_center() -> bool {
        with_window(|w, mtm| place_top_center(w, w.screen().or_else(|| NSScreen::mainScreen(mtm))))
            .unwrap_or(false)
    }

    pub fn snap_top_center_on(monitor_id: Option<isize>) -> bool {
        with_window(|w, mtm| {
            let screen = monitor_id
                .and_then(|id| find_screen(id, mtm))
                .or_else(|| w.screen())
                .or_else(|| NSScreen::mainScreen(mtm));
            place_top_center(w, screen)
        })
        .unwrap_or(false)
    }

    pub fn current_monitor_id() -> Option<isize> {
        with_window(|w, _| w.screen().map(|s| screen_id(&s))).flatten()
    }

    /// macOS 没有 Win+Shift+方向键那种系统级「甩到另一块屏」，窗口换屏只会是
    /// 用户手动拖动的结果 —— 那时不该把它再抢回顶端居中，否则会和用户较劲。
    /// 所以这里从不报告变化：始终沿用上一次的值，让上层的跨屏重排逻辑保持静默；
    /// 只有首帧 previous 为空时才给出真实所在屏，好让上层的 last_monitor 有初值。
    pub fn current_monitor_id_away_from(previous: Option<isize>) -> Option<isize> {
        previous.or_else(current_monitor_id)
    }

    /// 把窗口强行恢复成给定的**物理像素**尺寸，再摆回顶端居中。
    /// 上层按物理像素传进来（已乘过 DPI），而 AppKit 的 frame 用的是「点」，
    /// 所以这里要除以 backingScaleFactor 换算回去，否则高 DPI 屏上会大一倍。
    pub fn restore_size(w_px: i32, h_px: i32) -> bool {
        with_window(|w, mtm| {
            let scale = w.backingScaleFactor();
            let scale = if scale > 0.0 { scale } else { 1.0 };
            let frame = w.frame();
            let new = NSRect {
                origin: frame.origin,
                size: NSSize {
                    width: w_px as f64 / scale,
                    height: h_px as f64 / scale,
                },
            };
            w.setFrame_display(new, true);
            place_top_center(w, w.screen().or_else(|| NSScreen::mainScreen(mtm)))
        })
        .unwrap_or(false)
    }

    // ------------------------------------------------------------ 全局热键

    /// 传给 C 回调长期持有的一点上下文。泄漏一份，进程活多久它活多久。
    struct HotkeyCtx {
        ctx: egui::Context,
        signal: super::Signal,
    }

    /// Carbon 在主线程的事件派发里调这个。既然在主线程，显隐窗口直接做即可。
    unsafe extern "C" fn hotkey_handler(
        _call: carbon::EventHandlerCallRef,
        _event: carbon::EventRef,
        user: *mut c_void,
    ) -> carbon::OSStatus {
        let hk = unsafe { &*(user as *const HotkeyCtx) };
        if is_visible() {
            hide();
        } else {
            show();
            hk.signal.store(true, Ordering::SeqCst);
        }
        // 窗口 park 住时也要把 winit 事件循环叫醒来重绘
        hk.ctx.request_repaint();
        0 // noErr
    }

    pub fn spawn(ctx: egui::Context, signal: super::Signal) -> bool {
        // Carbon 事件目标和热键都要在主线程（run loop 所在线程）上注册
        if MainThreadMarker::new().is_none() {
            return false;
        }
        let boxed = Box::into_raw(Box::new(HotkeyCtx { ctx, signal }));
        // SAFETY: 都是标准 Carbon 调用；boxed 指向的上下文被故意泄漏，
        // 在整个进程生命周期内有效，回调解引用它是安全的。
        unsafe {
            let target = carbon::GetApplicationEventTarget();
            let spec = carbon::EventTypeSpec {
                event_class: carbon::K_EVENT_CLASS_KEYBOARD,
                event_kind: carbon::K_EVENT_HOTKEY_PRESSED,
            };
            let mut handler_ref: carbon::EventHandlerRef = std::ptr::null_mut();
            let installed = carbon::InstallEventHandler(
                target,
                Some(hotkey_handler),
                1,
                &spec,
                boxed as *mut c_void,
                &mut handler_ref,
            ) == 0;
            if !installed {
                drop(Box::from_raw(boxed));
                return false;
            }

            // Cmd+9：和窗口内 `modifiers.command + 9` 保持一致（mac 上 command 即 ⌘）。
            let hotkey_id = carbon::EventHotKeyID {
                signature: u32::from_be_bytes(*b"watl"),
                id: 1,
            };
            let mut hk_ref: carbon::EventHotKeyRef = std::ptr::null_mut();
            carbon::RegisterEventHotKey(
                carbon::KVK_ANSI_9,
                carbon::CMD_KEY,
                hotkey_id,
                target,
                0,
                &mut hk_ref,
            ) == 0
        }
    }

    /// Carbon / HIToolbox 里注册全局热键要用到的一小撮 FFI 声明。
    mod carbon {
        use std::ffi::c_void;

        pub type OSStatus = i32;
        pub type OSType = u32;
        pub type EventTargetRef = *mut c_void;
        pub type EventHotKeyRef = *mut c_void;
        pub type EventHandlerRef = *mut c_void;
        pub type EventHandlerCallRef = *mut c_void;
        pub type EventRef = *mut c_void;

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct EventHotKeyID {
            pub signature: OSType,
            pub id: u32,
        }

        #[repr(C)]
        #[derive(Clone, Copy)]
        pub struct EventTypeSpec {
            pub event_class: OSType,
            pub event_kind: u32,
        }

        pub type EventHandlerUPP = Option<
            unsafe extern "C" fn(EventHandlerCallRef, EventRef, *mut c_void) -> OSStatus,
        >;

        // Events.h 里的修饰键掩码与键码
        pub const CMD_KEY: u32 = 0x0100;
        pub const KVK_ANSI_9: u32 = 0x19;
        // 'keyb' 四字符码；kEventHotKeyPressed = 6
        pub const K_EVENT_CLASS_KEYBOARD: OSType = u32::from_be_bytes(*b"keyb");
        pub const K_EVENT_HOTKEY_PRESSED: u32 = 6;

        #[link(name = "Carbon", kind = "framework")]
        extern "C" {
            pub fn GetApplicationEventTarget() -> EventTargetRef;
            pub fn RegisterEventHotKey(
                in_hot_key_code: u32,
                in_hot_key_modifiers: u32,
                in_hot_key_id: EventHotKeyID,
                in_target: EventTargetRef,
                in_options: u32,
                out_ref: *mut EventHotKeyRef,
            ) -> OSStatus;
            pub fn InstallEventHandler(
                in_target: EventTargetRef,
                in_handler: EventHandlerUPP,
                in_num_types: usize,
                in_list: *const EventTypeSpec,
                in_user_data: *mut c_void,
                out_ref: *mut EventHandlerRef,
            ) -> OSStatus;
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    pub fn spawn(_ctx: egui::Context, _signal: super::Signal) -> bool {
        false // 其它平台暂时只有窗口内的 Ctrl+9
    }
    pub fn is_visible() -> bool {
        true
    }
    pub fn hide() {}
    /// 其它平台暂时不裁窗口形状，圆角只是画出来的
    pub fn apply_window_shape(_radius_px: i32) {}
    pub fn snap_top_center() -> bool {
        false
    }
    pub fn snap_top_center_on(_monitor_id: Option<isize>) -> bool {
        false
    }
    pub fn current_monitor_id() -> Option<isize> {
        None
    }
    pub fn current_monitor_id_away_from(_previous: Option<isize>) -> Option<isize> {
        None
    }
    pub fn restore_size(_w: i32, _h: i32) -> bool {
        false
    }
}

pub use imp::{
    apply_window_shape, current_monitor_id, current_monitor_id_away_from, hide, is_visible,
    restore_size, snap_top_center, snap_top_center_on, spawn,
};
