//! Windows 单实例守卫。
//!
//! 命名互斥量保证同一登录会话里只有一个 LEXIS 进程；命名事件让后来启动的
//! 进程通知已有实例恢复并前置窗口，然后后来者立即退出。

use std::io;

pub struct InstanceGuard {
    #[cfg(target_os = "windows")]
    mutex: isize,
    #[cfg(target_os = "windows")]
    activation_event: isize,
}

pub fn acquire() -> io::Result<Option<InstanceGuard>> {
    #[cfg(target_os = "windows")]
    {
        acquire_named(
            "Local\\LEXIS_WORD_ATLAS_SINGLE_INSTANCE_V1",
            "Local\\LEXIS_WORD_ATLAS_ACTIVATE_V1",
        )
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(Some(InstanceGuard {}))
    }
}

impl InstanceGuard {
    /// 开始监听后来实例的激活请求。线程持有守卫直到进程退出。
    pub fn listen_for_activation(self, ctx: egui::Context) {
        #[cfg(target_os = "windows")]
        {
            // 不能只把 Copy 的事件句柄捕获进闭包，否则 `self` 会在这里析构，
            // 连带关闭互斥量。显式在闭包末尾消费 guard，强制线程拥有整个守卫。
            let guard = self;
            std::thread::Builder::new()
                .name("single-instance-activation".into())
                .spawn(move || {
                    use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
                    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};

                    loop {
                        let result = unsafe {
                            WaitForSingleObject(guard.activation_event as HANDLE, INFINITE)
                        };
                        if result != WAIT_OBJECT_0 {
                            break;
                        }
                        crate::hotkey::show();
                        ctx.request_repaint();
                    }
                    drop(guard);
                })
                .expect("failed to start the single-instance activation listener");
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = (self, ctx);
        }
    }
}

#[cfg(target_os = "windows")]
fn acquire_named(mutex_name: &str, event_name: &str) -> io::Result<Option<InstanceGuard>> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, SetEvent};

    let wide = |value: &str| {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let event_name = wide(event_name);
    let mutex_name = wide(mutex_name);

    // 先创建事件，避免两个进程几乎同时启动时，后来者找不到激活通道。
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }

    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
    if mutex.is_null() {
        let error = io::Error::last_os_error();
        unsafe { CloseHandle(event) };
        return Err(error);
    }
    let already_running = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

    if already_running {
        unsafe {
            SetEvent(event);
            CloseHandle(mutex);
            CloseHandle(event);
        }
        return Ok(None);
    }

    Ok(Some(InstanceGuard {
        mutex: mutex as isize,
        activation_event: event as isize,
    }))
}

#[cfg(target_os = "windows")]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

        unsafe {
            CloseHandle(self.activation_event as HANDLE);
            CloseHandle(self.mutex as HANDLE);
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::acquire_named;

    #[test]
    fn a_named_guard_rejects_a_second_instance_until_dropped() {
        let suffix = std::process::id();
        let mutex = format!("Local\\LEXIS_TEST_MUTEX_{suffix}");
        let event = format!("Local\\LEXIS_TEST_EVENT_{suffix}");

        let first = acquire_named(&mutex, &event).unwrap().unwrap();
        assert!(acquire_named(&mutex, &event).unwrap().is_none());
        drop(first);
        assert!(acquire_named(&mutex, &event).unwrap().is_some());
    }
}
