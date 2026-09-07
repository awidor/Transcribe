use anyhow::{bail, Context, Result};
use std::{
    mem::{size_of, zeroed},
    ptr::null_mut,
    thread,
    time::{Duration, Instant},
};
use windows::Win32::{
    System::{
        Com::{
            CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
            COINIT_MULTITHREADED,
        },
        Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound},
    },
    UI::Accessibility::{CUIAutomation, IUIAutomation},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::{DeleteEnhMetaFile, GetEnhMetaFileBits, SetEnhMetaFileBits},
    System::{DataExchange::*, Memory::*, Threading::*},
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

#[derive(Clone)]
pub struct Target {
    hwnd: usize,
    pid: u32,
    focus: usize,
    identity: Option<String>,
    terminal: bool,
}
fn automation_identity() -> Option<String> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let result = (|| -> windows::core::Result<String> {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let e = automation.GetFocusedElement()?;
            // Element identity (not text contents) distinguishes editor panes, tabs and terminal controls.
            let array = e.GetRuntimeId()?;
            let runtime = (|| -> windows::core::Result<Vec<i32>> {
                let mut values = Vec::new();
                for i in SafeArrayGetLBound(array, 1)?..=SafeArrayGetUBound(array, 1)? {
                    let mut value = 0i32;
                    SafeArrayGetElement(array, &i, &mut value as *mut i32 as *mut _)?;
                    values.push(value);
                }
                Ok(values)
            })();
            let _ = SafeArrayDestroy(array);
            Ok(format!(
                "{}|{}|{}|{}|{:?}",
                e.CurrentProcessId()?,
                e.CurrentAutomationId()?,
                e.CurrentClassName()?,
                e.CurrentControlType()?.0,
                runtime?
            ))
        })()
        .ok();
        if initialized {
            CoUninitialize();
        }
        result
    }
}
fn current() -> Result<Target> {
    unsafe {
        let hwnd = GetForegroundWindow();
        anyhow::ensure!(!hwnd.is_null(), "Destination unavailable");
        let mut pid = 0;
        let tid = GetWindowThreadProcessId(hwnd, &mut pid);
        anyhow::ensure!(pid != GetCurrentProcessId(), "Choose a destination");
        let mut info: GUITHREADINFO = zeroed();
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        anyhow::ensure!(
            GetGUIThreadInfo(tid, &mut info) != 0,
            "Destination unavailable"
        );
        let mut name = [0u16; 1024];
        let mut n = name.len() as u32;
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        let exe = if !process.is_null() {
            let ok = QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut n);
            CloseHandle(process);
            if ok != 0 {
                String::from_utf16_lossy(&name[..n as usize]).to_lowercase()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let identity = automation_identity();
        let mut class = [0u16; 256];
        let length = GetClassNameW(hwnd, class.as_mut_ptr(), 256);
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        let terminal = [
            "windowsterminal.exe",
            "wezterm-gui.exe",
            "alacritty.exe",
            "mintty.exe",
            "conhost.exe",
            "cmd.exe",
            "powershell.exe",
            "pwsh.exe",
        ]
        .iter()
        .any(|s| exe.ends_with(s))
            || class == "ConsoleWindowClass"
            || identity
                .as_ref()
                .is_some_and(|i| i.to_lowercase().contains("terminal"));
        Ok(Target {
            hwnd: hwnd as usize,
            pid,
            focus: info.hwndFocus as usize,
            identity,
            terminal,
        })
    }
}
fn valid(target: &Target) -> bool {
    current().is_ok_and(|c| {
        c.hwnd == target.hwnd
            && c.pid == target.pid
            && c.focus == target.focus
            && c.identity == target.identity
    })
}
pub async fn capture() -> Result<Target> {
    tokio::task::spawn_blocking(current).await?
}

struct Window(HWND);
#[derive(Debug)]
struct ClipboardFailure {
    operation: &'static str,
    format: Option<u32>,
    code: u32,
}
impl std::fmt::Display for ClipboardFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Clipboard {} failed", self.operation)?;
        if let Some(format) = self.format {
            write!(f, " (format {format})")?;
        }
        write!(f, ": Windows error {}", self.code)
    }
}
impl std::error::Error for ClipboardFailure {}
fn clipboard_failure(operation: &'static str, format: Option<u32>) -> anyhow::Error {
    ClipboardFailure {
        operation,
        format,
        code: unsafe { GetLastError() },
    }
    .into()
}

impl Window {
    fn new() -> Result<Self> {
        unsafe {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            let w = CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                null_mut(),
                null_mut(),
            );
            if w.is_null() {
                return Err(clipboard_failure("owner window creation", None));
            }
            Ok(Self(w))
        }
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
struct Open;
fn pump_messages() {
    unsafe {
        let mut message: MSG = zeroed();
        while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}
fn wait_for_paste(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        pump_messages();
        thread::sleep(Duration::from_millis(10));
    }
}
impl Open {
    fn new(owner: HWND) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            if Instant::now() > deadline {
                return Err(clipboard_failure("open (busy for 2 seconds)", None));
            }
            pump_messages();
            thread::sleep(Duration::from_millis(15));
        }
    }
}
impl Drop for Open {
    fn drop(&mut self) {
        unsafe {
            CloseClipboard();
        }
    }
}
unsafe fn write(format: u32, bytes: &[u8]) -> Result<()> {
    if format == 14 {
        let h = SetEnhMetaFileBits(bytes.len() as u32, bytes.as_ptr());
        if h.is_null() {
            return Err(clipboard_failure("metafile allocation", Some(format)));
        }
        if SetClipboardData(format, h).is_null() {
            let error = clipboard_failure("write", Some(format));
            DeleteEnhMetaFile(h);
            return Err(error);
        }
        return Ok(());
    }
    let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
    if memory.is_null() {
        return Err(clipboard_failure("allocation", Some(format)));
    }
    let p = GlobalLock(memory);
    if p.is_null() {
        let error = clipboard_failure("write memory lock", Some(format));
        GlobalFree(memory);
        return Err(error);
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    GlobalUnlock(memory);
    if SetClipboardData(format, memory).is_null() {
        let error = clipboard_failure("write", Some(format));
        GlobalFree(memory);
        return Err(error);
    }
    Ok(())
}
fn snapshot() -> Result<Vec<(u32, Vec<u8>)>> {
    let mut result = Vec::new();
    let mut total = 0;
    unsafe {
        let mut f = 0;
        loop {
            SetLastError(0);
            f = EnumClipboardFormats(f);
            if f == 0 {
                if GetLastError() != 0 {
                    return Err(clipboard_failure("format enumeration", None));
                }
                break;
            }
            // Windows regenerates CF_BITMAP from a restored DIB, including its color table.
            if f == 2 && (IsClipboardFormatAvailable(8) != 0 || IsClipboardFormatAvailable(17) != 0)
            {
                continue;
            }
            if f == 14 {
                let h = GetClipboardData(f);
                if h.is_null() {
                    return Err(clipboard_failure("read", Some(f)));
                }
                let size = GetEnhMetaFileBits(h, 0, null_mut());
                if size == 0 {
                    return Err(clipboard_failure("metafile size", Some(f)));
                }
                total += size as usize;
                anyhow::ensure!(total <= 64 * 1024 * 1024, "Clipboard too large");
                let mut bytes = vec![0; size as usize];
                if GetEnhMetaFileBits(h, size, bytes.as_mut_ptr()) != size {
                    return Err(clipboard_failure("metafile read", Some(f)));
                }
                result.push((f, bytes));
                continue;
            }
            // Refuse remaining handle formats instead of destroying a clipboard we cannot restore.
            if [2, 3, 9, 0x80, 0x81, 0x82, 0x83, 0x8e].contains(&f) {
                bail!("Clipboard format {f} cannot be preserved");
            }
            let h = GetClipboardData(f);
            if h.is_null() {
                return Err(clipboard_failure("read", Some(f)));
            }
            let size = GlobalSize(h);
            total += size;
            anyhow::ensure!(
                size > 0 && total <= 64 * 1024 * 1024,
                "Clipboard format {f} cannot be preserved (size {size}, total {total})"
            );
            let p = GlobalLock(h);
            if p.is_null() {
                return Err(clipboard_failure("read memory lock", Some(f)));
            }
            let bytes = std::slice::from_raw_parts(p as *const u8, size).to_vec();
            GlobalUnlock(h);
            result.push((f, bytes));
        }
    }
    Ok(result)
}
type ClipboardSnapshot = Vec<(u32, Vec<u8>)>;

fn retry_snapshot<T>(mut attempt: impl FnMut() -> Result<T>, budget: Duration) -> Result<T> {
    let deadline = Instant::now() + budget;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) => {
                // Only retry native preparation failures. Unsupported formats
                // must remain intact; neither writes nor dispatch run here.
                if error.downcast_ref::<ClipboardFailure>().is_none() || Instant::now() >= deadline
                {
                    return Err(error);
                }
                pump_messages();
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn open_snapshot(owner: HWND) -> Result<(Open, ClipboardSnapshot)> {
    retry_snapshot(
        || {
            let open = Open::new(owner)?;
            // A failed read drops Open before retrying, allowing a delayed-rendering
            // owner to finish publishing. Every retry starts a fresh full snapshot.
            let saved = snapshot()?;
            Ok((open, saved))
        },
        Duration::from_secs(2),
    )
}

unsafe fn clear() -> Result<()> {
    if EmptyClipboard() == 0 {
        return Err(clipboard_failure("clear", None));
    }
    Ok(())
}

unsafe fn restore(old: &ClipboardSnapshot) -> Result<()> {
    clear()?;
    let mut errors = Vec::new();
    // Attempt every saved format even if restoring one fails.
    for (format, bytes) in old {
        if let Err(error) = write(*format, bytes) {
            errors.push(error.to_string());
        }
    }
    if !errors.is_empty() {
        bail!("Clipboard restore failed: {}", errors.join("; "));
    }
    Ok(())
}
fn text_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_ne_bytes)
        .collect()
}
unsafe fn suppress_history() -> Result<()> {
    for name in ["CanIncludeInClipboardHistory", "CanUploadToCloudClipboard"] {
        let s: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let format = RegisterClipboardFormatW(s.as_ptr());
        if format == 0 {
            return Err(clipboard_failure("privacy format registration", None));
        }
        write(format, &0u32.to_ne_bytes())?;
    }
    Ok(())
}
fn modifiers_released() -> bool {
    [VK_CONTROL, VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN]
        .iter()
        .all(|v| unsafe { GetAsyncKeyState(*v as i32) } >= 0)
}
fn paste_sync(target: Target, text: String) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !modifiers_released() {
        if Instant::now() > deadline {
            bail!("Shortcut still held");
        }
        thread::sleep(Duration::from_millis(15));
    }
    anyhow::ensure!(valid(&target), "Destination changed");
    let window = Window::new()?;
    let old;
    {
        let (_open, saved) =
            open_snapshot(window.0).context("Could not preserve clipboard before paste")?;
        old = saved;
        unsafe {
            clear()?;
            if let Err(e) = write(
                13,
                &text_bytes(&super::prepare_text(&text, target.terminal)),
            )
            .and_then(|_| suppress_history())
            {
                restore(&old).with_context(|| format!("{e}; rollback also failed"))?;
                return Err(e);
            }
        }
    }
    // CloseClipboard can synthesize compatible formats and increment the sequence.
    let sequence = unsafe { GetClipboardSequenceNumber() };
    let dispatched = (|| -> Result<()> {
        anyhow::ensure!(
            valid(&target) && modifiers_released(),
            "Destination changed"
        );
        anyhow::ensure!(
            unsafe { GetClipboardOwner() == window.0 && GetClipboardSequenceNumber() == sequence },
            "Clipboard changed before paste; transcript saved"
        );
        let keys: Vec<(u16, bool)> = if target.terminal {
            vec![
                (VK_CONTROL, false),
                (VK_SHIFT, false),
                (0x56, false),
                (0x56, true),
                (VK_SHIFT, true),
                (VK_CONTROL, true),
            ]
        } else {
            vec![
                (VK_CONTROL, false),
                (0x56, false),
                (0x56, true),
                (VK_CONTROL, true),
            ]
        };
        let events: Vec<INPUT> = keys
            .iter()
            .map(|(key, up)| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: *key,
                        wScan: 0,
                        dwFlags: if *up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            })
            .collect();
        let count = unsafe {
            SendInput(
                events.len() as u32,
                events.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        if count != events.len() as u32 {
            // Always release our synthetic modifiers, even after a partial dispatch.
            let releases: Vec<INPUT> = events
                .iter()
                .copied()
                .filter(|e| unsafe { e.Anonymous.ki.dwFlags & KEYEVENTF_KEYUP != 0 })
                .collect();
            unsafe {
                SendInput(
                    releases.len() as u32,
                    releases.as_ptr(),
                    size_of::<INPUT>() as i32,
                );
            }
            bail!("Paste could not be confirmed");
        }
        // SendInput acknowledges dispatch, not insertion. Keep history status unverified.
        // Clipboard ownership messages must remain serviced while the destination reads.
        wait_for_paste(Duration::from_millis(1200));
        Ok(())
    })();
    {
        let _open = Open::new(window.0).context("Paste unverified; clipboard busy")?;
        unsafe {
            if GetClipboardOwner() == window.0 && GetClipboardSequenceNumber() == sequence {
                restore(&old).context("Paste unverified; clipboard restore failed")?;
            }
        }
    }
    dispatched
}
pub async fn paste(target: Target, text: String) -> Result<()> {
    tokio::task::spawn_blocking(move || paste_sync(target, text))
        .await?
        .map_err(|error| anyhow::anyhow!("{error:#}"))
}
pub async fn copy(text: String) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        let window = Window::new()?;
        let _open = Open::new(window.0)?;
        unsafe {
            clear()?;
            write(13, &text_bytes(&text))
        }
    })
    .await?
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    #[test]
    fn snapshot_retries_transient_reads_but_not_unsupported_formats() {
        let mut reads = 0;
        let saved = retry_snapshot(
            || {
                reads += 1;
                if reads == 1 {
                    Err(ClipboardFailure {
                        operation: "read",
                        format: Some(49373),
                        code: 0,
                    }
                    .into())
                } else {
                    Ok(vec![(13, text_bytes("preserved"))])
                }
            },
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(reads, 2);
        assert_eq!(saved[0].1, text_bytes("preserved"));

        let mut attempts = 0;
        let result = retry_snapshot::<()>(
            || {
                attempts += 1;
                bail!("Clipboard format 3 cannot be preserved")
            },
            Duration::from_secs(1),
        );
        assert!(result.is_err());
        assert_eq!(attempts, 1);

        let error = retry_snapshot::<()>(
            || {
                Err(ClipboardFailure {
                    operation: "read",
                    format: Some(49373),
                    code: 5,
                }
                .into())
            },
            Duration::ZERO,
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Clipboard read failed (format 49373): Windows error 5"
        );
    }

    const PREPARE_DELAYED_CLIPBOARD: u32 = WM_APP + 7;
    static RENDER_REQUESTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "system" fn fixture_proc(
        hwnd: HWND,
        message: u32,
        w: WPARAM,
        l: LPARAM,
    ) -> LRESULT {
        if message == PREPARE_DELAYED_CLIPBOARD {
            let _open = Open::new(hwnd).unwrap();
            clear().unwrap();
            write(13, &text_bytes("clipboard sentinel")).unwrap();
            let format = RegisterClipboardFormatW(wide("Transcribe delayed fixture").as_ptr());
            RENDER_REQUESTS.store(0, std::sync::atomic::Ordering::SeqCst);
            SetClipboardData(format, null_mut());
            return 0;
        }
        if message == WM_RENDERFORMAT {
            // Model an owner whose advertised format is temporarily unavailable.
            if RENDER_REQUESTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                write(w as u32, b"delayed clipboard payload\0").unwrap();
            }
            return 0;
        }
        DefWindowProcW(hwnd, message, w, l)
    }
    unsafe fn read_text(hwnd: HWND) -> String {
        let mut text = vec![0u16; 4096];
        let n = SendMessageW(hwnd, WM_GETTEXT, text.len(), text.as_mut_ptr() as isize);
        String::from_utf16_lossy(&text[..n.max(0) as usize])
    }
    #[test]
    #[ignore = "Child process used by native_roundtrip"]
    fn fixture() {
        let Ok(path) = std::env::var("TRANSCRIBE_FIXTURE") else {
            return;
        };
        unsafe {
            let class_name = wide("Transcribe delayed clipboard fixture");
            let class = WNDCLASSW {
                lpfnWndProc: Some(fixture_proc),
                lpszClassName: class_name.as_ptr(),
                ..zeroed()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let top = CreateWindowExW(
                0,
                class_name.as_ptr(),
                wide("Transcribe paste test").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                60,
                60,
                480,
                220,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
            );
            assert!(!top.is_null());
            let edit = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_BORDER | 0x0004,
                12,
                12,
                430,
                70,
                top,
                null_mut(),
                null_mut(),
                null_mut(),
            );
            let second = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide("").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_BORDER,
                12,
                95,
                430,
                30,
                top,
                null_mut(),
                null_mut(),
                null_mut(),
            );
            SetForegroundWindow(top);
            SetFocus(edit);
            std::fs::write(
                path,
                format!("{} {} {}", top as usize, edit as usize, second as usize),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(45);
            while Instant::now() < deadline && IsWindow(top) != 0 {
                let mut message: MSG = zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                thread::sleep(Duration::from_millis(5));
            }
            DestroyWindow(top);
        }
    }
    #[test]
    #[ignore = "Interactive native fixture; run explicitly on an unlocked desktop"]
    fn native_roundtrip() {
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let path = std::env::temp_dir().join(format!("transcribe-probe-{}", uuid::Uuid::new_v4()));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "insertion::windows::native_tests::fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("TRANSCRIBE_FIXTURE", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        let _child = Child(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(Instant::now() < deadline, "Fixture did not start");
            thread::sleep(Duration::from_millis(20));
        }
        let handles: Vec<usize> = std::fs::read_to_string(&path)
            .unwrap()
            .split_whitespace()
            .map(|v| v.parse().unwrap())
            .collect();
        let _ = std::fs::remove_file(path);
        let top = handles[0] as HWND;
        let edit = handles[1] as HWND;
        unsafe {
            // Test harness only: join the foreground input queue to activate our fixture.
            // Production insertion never changes the foreground application.
            let foreground_thread = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
            let thread = GetCurrentThreadId();
            AttachThreadInput(thread, foreground_thread, 1);
            SetForegroundWindow(top);
            AttachThreadInput(thread, foreground_thread, 0);
        }
        thread::sleep(Duration::from_millis(150));
        let window = Window::new().unwrap();
        let original = {
            let _open = Open::new(window.0).unwrap();
            snapshot().unwrap()
        };
        struct Restore {
            window: Window,
            old: Vec<(u32, Vec<u8>)>,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Ok(_open) = Open::new(self.window.0) {
                    if snapshot().ok().is_some_and(|items| {
                        items.iter().any(|(f, b)| {
                            *f == 13
                                && (b.starts_with(&text_bytes("clipboard sentinel"))
                                    || b.starts_with(&text_bytes("external clipboard sentinel")))
                        })
                    }) {
                        unsafe {
                            EmptyClipboard();
                            for (f, b) in &self.old {
                                let _ = write(*f, b);
                            }
                        }
                    }
                }
            }
        }
        let saved = Restore {
            window,
            old: original,
        };
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        dib.extend_from_slice(&[10, 80, 40, 255]);
        {
            let _open = Open::new(saved.window.0).unwrap();
            unsafe {
                EmptyClipboard();
                write(13, &text_bytes("clipboard sentinel")).unwrap();
                let html = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
                write(html, b"<b>sentinel</b>\0").unwrap();
                write(8, &dib).unwrap();
            }
        }
        for _ in 0..10 {
            unsafe {
                let own = GetCurrentThreadId();
                let foreground = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
                let fixture = GetWindowThreadProcessId(top, null_mut());
                AttachThreadInput(own, foreground, 1);
                AttachThreadInput(own, fixture, 1);
                SetForegroundWindow(top);
                SetFocus(edit);
                AttachThreadInput(own, fixture, 0);
                AttachThreadInput(own, foreground, 0);
            }
            thread::sleep(Duration::from_millis(50));
            if unsafe { GetForegroundWindow() } == top {
                break;
            }
        }
        let target = current().unwrap();
        assert_eq!(target.hwnd, top as usize);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        runtime
            .block_on(super::super::insert(&id, "Grüße 世界 👋"))
            .unwrap();
        assert!(runtime
            .block_on(super::super::insert(&id, "duplicate"))
            .is_err());
        assert_eq!(unsafe { read_text(edit) }, "Grüße 世界 👋");
        {
            let _open = Open::new(saved.window.0).unwrap();
            let restored = snapshot().unwrap();
            assert!(restored
                .iter()
                .any(|(f, b)| *f == 13 && b.starts_with(&text_bytes("clipboard sentinel"))));
            assert!(restored
                .iter()
                .any(|(_, b)| b.starts_with(b"<b>sentinel</b>\0")));
            assert!(restored.iter().any(|(f, b)| *f == 8 && b.starts_with(&dib)));
        }
        unsafe {
            SendMessageW(top, PREPARE_DELAYED_CLIPBOARD, 0, 0);
            SendMessageW(edit, WM_SETTEXT, 0, wide("").as_ptr() as isize);
        }
        runtime
            .block_on(super::super::insert(
                &uuid::Uuid::new_v4().to_string(),
                "delayed format recovered",
            ))
            .unwrap();
        assert_eq!(unsafe { read_text(edit) }, "delayed format recovered");
        {
            let _open = Open::new(saved.window.0).unwrap();
            let restored = snapshot().unwrap();
            assert!(restored
                .iter()
                .any(|(_, b)| b.starts_with(b"delayed clipboard payload\0")));
            assert!(restored
                .iter()
                .any(|(f, b)| *f == 13 && b.starts_with(&text_bytes("clipboard sentinel"))));
        }
        unsafe {
            SendMessageW(edit, WM_SETTEXT, 0, wide("").as_ptr() as isize);
        }
        let mut terminal = current().unwrap();
        terminal.terminal = true;
        paste_sync(terminal, "echo hello\nsecond line".into()).unwrap();
        assert_eq!(unsafe { read_text(edit) }, "echo hello second line");
        // An independent clipboard writer must win over this transaction's restoration.
        let newer = thread::spawn(|| {
            thread::sleep(Duration::from_millis(350));
            let w = Window::new().unwrap();
            let _open = Open::new(w.0).unwrap();
            unsafe {
                EmptyClipboard();
                write(13, &text_bytes("external clipboard sentinel")).unwrap();
            }
        });
        paste_sync(current().unwrap(), " appended".into()).unwrap();
        newer.join().unwrap();
        {
            let _open = Open::new(saved.window.0).unwrap();
            assert!(snapshot().unwrap().iter().any(
                |(f, b)| *f == 13 && b.starts_with(&text_bytes("external clipboard sentinel"))
            ));
        }
        // Switching foreground windows must reject delivery before changing the clipboard.
        unsafe {
            ShowWindow(top, SW_MINIMIZE);
        }
        thread::sleep(Duration::from_millis(100));
        assert!(paste_sync(target, "must not appear".into()).is_err());
        assert_eq!(
            unsafe { read_text(edit) },
            "echo hello second line appended"
        );
    }
}
