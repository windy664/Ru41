//! 适配器内部的子进程清理工具；不承载底座协议或业务调度。
pub(super) struct ProcessGroup(pub(super) Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // SAFETY: pid 来自本次新建进程组，不涉及指针。
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
    }
}

// Future 被取消时仍回收直接子进程，避免僵尸累积。
pub(super) struct OwnedChild(pub(super) Option<tokio::process::Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}
