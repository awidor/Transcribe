use anyhow::{bail, Result};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    sync::Arc,
};
unsafe extern "C" {
    fn tc_capture(status: *mut i32) -> *mut c_void;
    fn tc_release(target: *mut c_void);
    fn tc_terminal(target: *mut c_void) -> bool;
    fn tc_role(target: *mut c_void, out: *mut c_char, size: usize) -> bool;
    fn tc_settable(target: *mut c_void) -> bool;
    fn tc_prepare();
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
        let target = Target(Arc::new(Native(p as usize)));
        let mut buffer = [0 as c_char; 64];
        let role = if unsafe { tc_role(p, buffer.as_mut_ptr(), buffer.len()) } {
            unsafe { CStr::from_ptr(buffer.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        };
        let accepts = unsafe { tc_terminal(p) }
            || !super::destination::macos_rejects(&role, unsafe { tc_settable(p) });
        // Nothing is sent and the clipboard is untouched; the widget offers the text.
        anyhow::ensure!(accepts, super::destination::NO_TEXT_FIELD);
        Ok(target)
    })
    .await?
}
pub async fn prepare() {
    let _ = tokio::task::spawn_blocking(|| unsafe { tc_prepare() }).await;
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
