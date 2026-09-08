use anyhow::{bail, Result};
use std::{
    ffi::{c_char, c_void, CString},
    sync::Arc,
};
unsafe extern "C" {
    fn tc_capture(status: *mut i32) -> *mut c_void;
    fn tc_release(target: *mut c_void);
    fn tc_terminal(target: *mut c_void) -> bool;
    fn tc_paste(target: *mut c_void, text: *const c_char) -> i32;
    fn tc_copy(text: *const c_char) -> bool;
}
struct Native(usize);
impl Drop for Native {
    fn drop(&mut self) {
        unsafe {
            tc_release(self.0 as *mut c_void);
        }
    }
}
#[derive(Clone)]
pub struct Target(Arc<Native>);
pub async fn capture() -> Result<Target> {
    tokio::task::spawn_blocking(|| {
        let mut status = -1;
        let p = unsafe { tc_capture(&mut status) };
        if p.is_null() {
            bail!(match status {
                1 => "Accessibility permission required",
                2 => "No external insertion target",
                3 => "Focused text field unavailable",
                4 => "Destination changed",
                _ => "Insertion target unavailable",
            });
        }
        Ok(Target(Arc::new(Native(p as usize))))
    })
    .await?
}
pub async fn paste(target: Target, text: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let terminal = unsafe { tc_terminal(target.0 .0 as *mut c_void) };
        let text = CString::new(super::prepare_text(&text, terminal))?;
        match unsafe { tc_paste(target.0 .0 as *mut c_void, text.as_ptr()) } {
            0 => Ok(()),
            1 => bail!("Destination changed"),
            2 => bail!("Accessibility permission required"),
            3 => bail!("Shortcut still held"),
            _ => bail!("Paste could not be confirmed"),
        }
    })
    .await?
}
pub async fn copy(text: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let text = CString::new(text)?;
        anyhow::ensure!(unsafe { tc_copy(text.as_ptr()) }, "Clipboard unavailable");
        Ok(())
    })
    .await?
}
