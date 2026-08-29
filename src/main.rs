#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![allow(unused_must_use)]

use std::ffi::c_void;
use std::mem::size_of;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use vt100::{Color as VtColor, Parser};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Foundation::{SetHandleInformation, HANDLE_FLAGS, HANDLE_FLAG_INHERIT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, GetStockObject, InvalidateRect, SelectObject,
    SetBkMode, SetTextColor, TextOutW, BLACK_BRUSH, HBRUSH, HDC, HFONT, PAINTSTRUCT, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, HPCON,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{
    GetProcessHeap, HeapAlloc, HeapFree, HEAP_FLAGS, HEAP_ZERO_MEMORY,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, OpenInputDesktop, SetThreadDesktop, SwitchDesktop,
    DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, HDESK,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, ResumeThread,
    TerminateProcess, UpdateProcThreadAttribute, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW, STARTUPINFOW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, RegisterHotKey, SetFocus, UnregisterHotKey, MOD_ALT, MOD_CONTROL, VK_C, VK_DOWN,
    VK_END, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR,
    VK_RIGHT, VK_UP,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, IsWindow,
    LoadCursorW, LoadIconW, MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassW, SetCursor,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, TrackPopupMenu,
    TranslateMessage, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, GWLP_USERDATA, HWND_TOPMOST,
    IDC_ARROW, IDI_APPLICATION, MF_STRING, MSG, SWP_SHOWWINDOW, SW_SHOW, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, WM_APP, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_HOTKEY, WM_KEYDOWN, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WM_SETCURSOR, WM_SIZE, WM_TIMER, WM_USER, WNDCLASSW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOPMOST, WS_POPUP,
};

const WM_TRAY: u32 = WM_USER + 1;
const WM_OUTPUT: u32 = WM_APP + 1;
const WM_ACTIVATE_HOST: u32 = WM_APP + 2;
const ID_EXIT: usize = 1001;
const HOTKEY_BASE: i32 = 1;
const DESKTOP_ACCESS: u32 = 0x0001 | 0x0002 | 0x0040 | 0x0080 | 0x0100;
const CURSOR_TIMER: usize = 1;

#[derive(Clone)]
struct HostShared {
    output: Arc<Mutex<Vec<u8>>>,
    hwnd: Arc<Mutex<Option<usize>>>,
}

struct HostState {
    shared: HostShared,
    input_write: HANDLE,
    output_read: HANDLE,
    process: HANDLE,
    job: HANDLE,
    pty: HPCON,
    font: HFONT,
    parser: Parser,
    dpi: u32,
    cell_width: i32,
    cell_height: i32,
    rows: u16,
    cols: u16,
    cursor_visible: bool,
}

struct HostReady {
    hwnd: usize,
}

struct DesktopState {
    desktop: HDESK,
    host: HostReady,
    join: thread::JoinHandle<()>,
}

struct AppState {
    default_desktop: HDESK,
    desktops: [Option<DesktopState>; 6],
    controller: HWND,
    tray: NOTIFYICONDATAW,
}

impl Drop for HostState {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.process, 0);
            let _ = CloseHandle(self.job);
            let _ = CloseHandle(self.input_write);
            let _ = CloseHandle(self.output_read);
            ClosePseudoConsole(self.pty);
            let _ = DeleteObject(self.font.into());
        }
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn register_class(
    name: PCWSTR,
    proc: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    cursor: windows::Win32::UI::WindowsAndMessaging::HCURSOR,
) {
    let instance = unsafe { GetModuleHandleW(None).unwrap() };
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(proc),
        hInstance: instance.into(),
        hCursor: cursor,
        hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH).0 }),
        lpszClassName: name,
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&class);
    }
}

fn create_pipe_pair() -> windows::core::Result<(HANDLE, HANDLE)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    let mut security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    unsafe {
        windows::Win32::System::Pipes::CreatePipe(&mut read, &mut write, Some(&mut security), 0)?;
    }
    Ok((read, write))
}

fn launch_cmd(
    desktop_name: &str,
) -> windows::core::Result<(HANDLE, HANDLE, HANDLE, HANDLE, HPCON)> {
    let (input_read, input_write) = create_pipe_pair()?;
    let (output_read, output_write) = create_pipe_pair()?;
    unsafe {
        SetHandleInformation(input_write, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
        SetHandleInformation(output_read, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0))?;
    }
    let size = windows::Win32::System::Console::COORD { X: 120, Y: 40 };
    let pty = unsafe { CreatePseudoConsole(size, input_read, output_write, 0)? };
    unsafe {
        CloseHandle(input_read)?;
        CloseHandle(output_write)?;
    }

    let mut attribute_size = 0usize;
    unsafe {
        InitializeProcThreadAttributeList(None, 1, Some(0), &mut attribute_size);
    }
    let heap = unsafe { GetProcessHeap()? };
    let attributes = unsafe { HeapAlloc(heap, HEAP_ZERO_MEMORY, attribute_size) };
    if attributes.is_null() {
        return Err(windows::core::Error::from_win32());
    }
    let attributes = LPPROC_THREAD_ATTRIBUTE_LIST(attributes);
    let result = unsafe {
        InitializeProcThreadAttributeList(Some(attributes), 1, Some(0), &mut attribute_size)
            .and_then(|_| {
                UpdateProcThreadAttribute(
                    attributes,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    // PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE expects the HPCON
                    // handle value, not the address of the local variable.
                    Some(pty.0 as *const c_void),
                    size_of::<HPCON>(),
                    None,
                    None,
                )
            })
    };
    if let Err(error) = result {
        unsafe {
            HeapFree(heap, HEAP_FLAGS(0), Some(attributes.0 as *const c_void));
        }
        return Err(error);
    }

    // Use an explicit system path so process startup does not depend on the
    // working directory or PATH inherited by the virtual Desktop.
    let executable = to_wide(r"C:\Windows\System32\cmd.exe");
    let mut command = to_wide("\"C:\\Windows\\System32\\cmd.exe\" /D /Q");
    let mut desktop_name = to_wide(desktop_name);
    let job = unsafe { CreateJobObjectW(None, None)? };
    let mut job_info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    job_info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &job_info as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )?;
    }
    let mut startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: size_of::<STARTUPINFOEXW>() as u32,
            lpDesktop: PWSTR(desktop_name.as_mut_ptr()),
            ..Default::default()
        },
        lpAttributeList: attributes,
    };
    let mut process_info = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            PCWSTR(executable.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            None,
            None,
            &mut startup.StartupInfo,
            &mut process_info,
        )
    };
    unsafe {
        DeleteProcThreadAttributeList(attributes);
        let _ = HeapFree(heap, HEAP_FLAGS(0), Some(attributes.0 as *const c_void));
    }
    if let Err(error) = created {
        unsafe {
            let _ = CloseHandle(job);
            let _ = CloseHandle(input_write);
            let _ = CloseHandle(output_read);
            ClosePseudoConsole(pty);
        }
        return Err(error);
    }
    unsafe {
        if let Err(error) = AssignProcessToJobObject(job, process_info.hProcess) {
            let _ = TerminateProcess(process_info.hProcess, 1);
            let _ = CloseHandle(process_info.hThread);
            let _ = CloseHandle(process_info.hProcess);
            let _ = CloseHandle(job);
            let _ = CloseHandle(input_write);
            let _ = CloseHandle(output_read);
            ClosePseudoConsole(pty);
            return Err(error);
        }
        ResumeThread(process_info.hThread);
        CloseHandle(process_info.hThread)?;
    }
    Ok((input_write, output_read, process_info.hProcess, job, pty))
}

fn host_thread(desktop: HDESK, desktop_name: String, ready_tx: mpsc::SyncSender<HostReady>) {
    if let Err(error) = unsafe { SetThreadDesktop(desktop) } {
        let text = to_wide(&format!("绑定 Desktop 失败: {error}"));
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(text.as_ptr()),
                w!("Virtual Desktop"),
                windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
        return;
    }
    let (input_write, output_read, process, job, pty) = match launch_cmd(&desktop_name) {
        Ok(value) => value,
        Err(error) => {
            let text = to_wide(&format!("无法启动终端: {error}"));
            unsafe {
                MessageBoxW(
                    None,
                    PCWSTR(text.as_ptr()),
                    w!("Virtual Desktop"),
                    windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                );
            }
            return;
        }
    };
    let output = Arc::new(Mutex::new(Vec::new()));
    let hwnd_slot = Arc::new(Mutex::new(None));
    let shared = HostShared {
        output: output.clone(),
        hwnd: hwnd_slot.clone(),
    };
    let shared_for_reader = shared.clone();
    let output_handle = output_read.0 as usize;
    thread::spawn(move || {
        let output_handle = HANDLE(output_handle as *mut c_void);
        let mut buffer = [0u8; 8192];
        loop {
            let mut read = 0u32;
            let ok = unsafe { ReadFile(output_handle, Some(&mut buffer), Some(&mut read), None) };
            if ok.is_err() || read == 0 {
                break;
            }
            if let Ok(mut pending) = shared_for_reader.output.lock() {
                pending.extend_from_slice(&buffer[..read as usize]);
            }
            if let Ok(hwnd) = shared_for_reader.hwnd.lock() {
                if let Some(hwnd) = *hwnd {
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(hwnd as *mut c_void)),
                            WM_OUTPUT,
                            WPARAM(0),
                            LPARAM(0),
                        );
                    }
                }
            }
        }
    });
    let font = unsafe {
        CreateFontW(
            20,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(0),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(0),
            0,
            w!("Cascadia Mono"),
        )
    };
    let state = Box::new(HostState {
        shared: shared.clone(),
        input_write,
        output_read,
        process,
        job,
        pty,
        font,
        parser: Parser::new(40, 120, 10_000),
        dpi: 96,
        cell_width: 10,
        cell_height: 20,
        rows: 40,
        cols: 120,
        cursor_visible: true,
    });
    let raw_state = Box::into_raw(state);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_APPWINDOW,
            w!("VirtualDesktopTerminal"),
            w!("Virtual Desktop Terminal"),
            WS_POPUP,
            0,
            0,
            GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN),
            GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN),
            None,
            None,
            Some(GetModuleHandleW(None).unwrap().into()),
            Some(raw_state as *const c_void),
        )
        .unwrap()
    };
    if let Ok(mut slot) = hwnd_slot.lock() {
        *slot = Some(hwnd.0 as usize);
    }
    unsafe {
        SetTimer(Some(hwnd), CURSOR_TIMER, 500, None);
    }
    let ready = HostReady {
        hwnd: hwnd.0 as usize,
    };
    let _ = ready_tx.send(ready);
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetFocus(Some(hwnd));
    }
    let mut msg = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 <= 0 {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn terminal_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut HostState;
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        update_font(
            &mut *(create.lpCreateParams as *mut HostState),
            windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd),
        );
        SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CXSCREEN),
            GetSystemMetrics(windows::Win32::UI::WindowsAndMessaging::SM_CYSCREEN),
            SWP_SHOWWINDOW,
        );
        return LRESULT(1);
    }
    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let state = &mut *state_ptr;
    match message {
        WM_OUTPUT => {
            if let Ok(mut bytes) = state.shared.output.lock() {
                let chunk = std::mem::take(&mut *bytes);
                state.parser.process(&chunk);
            }
            InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_ACTIVATE_HOST => {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            SetFocus(Some(hwnd));
            LRESULT(0)
        }
        WM_CHAR => {
            let ch = wparam.0 as u16;
            if ch != 3 {
                send_input(
                    state.input_write,
                    &String::from_utf16_lossy(&[ch]).into_bytes(),
                );
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_C.0 as u16
                && GetKeyState(windows::Win32::UI::Input::KeyboardAndMouse::VK_CONTROL.0 as i32) < 0
            {
                send_input(state.input_write, &[3]);
            } else if let Some(sequence) = key_sequence(key) {
                send_input(state.input_write, sequence);
            }
            LRESULT(0)
        }
        WM_SIZE => {
            let width = (lparam.0 as u32 & 0xffff) as i32;
            let height = ((lparam.0 as u32 >> 16) & 0xffff) as i32;
            let cols = (width / state.cell_width.max(1)).clamp(20, 300) as u16;
            let rows = (height / state.cell_height.max(1)).clamp(5, 120) as u16;
            state.cols = cols;
            state.rows = rows;
            state.parser.set_size(rows, cols);
            let _ = ResizePseudoConsole(
                state.pty,
                windows::Win32::System::Console::COORD {
                    X: cols as i16,
                    Y: rows as i16,
                },
            );
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let dpi = (wparam.0 & 0xffff) as u32;
            update_font(state, dpi);
            InvalidateRect(Some(hwnd), None, true);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == CURSOR_TIMER => {
            state.cursor_visible = !state.cursor_visible;
            InvalidateRect(Some(hwnd), None, false);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SETCURSOR => {
            SetCursor(None);
            LRESULT(1)
        }
        WM_MOUSEACTIVATE | WM_MOUSEMOVE | 0x0201..=0x020e => LRESULT(0),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            // Paint into a memory surface and copy it once to eliminate the
            // visible erase/draw flash caused by cell-by-cell rendering.
            let back = CreateCompatibleDC(Some(hdc));
            let bitmap = CreateCompatibleBitmap(hdc, rect.right.max(1), rect.bottom.max(1));
            let old_bitmap = SelectObject(back, bitmap.into());
            let brush = CreateSolidBrush(COLORREF(0x00000000));
            FillRect(back, &rect, brush);
            DeleteObject(brush.into());
            let old = SelectObject(back, state.font.into());
            SetBkMode(back, TRANSPARENT);
            SetTextColor(back, COLORREF(0x00E6E6E6));
            draw_terminal_screen(back, state, &rect);
            SelectObject(back, old);
            BitBlt(
                hdc,
                0,
                0,
                rect.right,
                rect.bottom,
                Some(back),
                0,
                0,
                SRCCOPY,
            );
            SelectObject(back, old_bitmap);
            DeleteObject(bitmap.into());
            DeleteDC(back);
            EndPaint(hwnd, &paint);
            LRESULT(0)
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = Box::from_raw(state_ptr);
            // This window owns the host thread's message loop. Once the
            // window is destroyed, let that loop return so shutdown join()
            // cannot wait forever.
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn send_input(handle: HANDLE, bytes: &[u8]) {
    let mut written = 0u32;
    let _ = unsafe { WriteFile(handle, Some(bytes), Some(&mut written), None) };
}

fn key_sequence(key: u16) -> Option<&'static [u8]> {
    match key {
        k if k == VK_UP.0 as u16 => Some(b"\x1b[A"),
        k if k == VK_DOWN.0 as u16 => Some(b"\x1b[B"),
        k if k == VK_RIGHT.0 as u16 => Some(b"\x1b[C"),
        k if k == VK_LEFT.0 as u16 => Some(b"\x1b[D"),
        k if k == VK_HOME.0 as u16 => Some(b"\x1b[H"),
        k if k == VK_END.0 as u16 => Some(b"\x1b[F"),
        k if k == VK_PRIOR.0 as u16 => Some(b"\x1b[5~"),
        k if k == VK_NEXT.0 as u16 => Some(b"\x1b[6~"),
        k if k == VK_F1.0 as u16 => Some(b"\x1bOP"),
        k if k == VK_F2.0 as u16 => Some(b"\x1bOQ"),
        k if k == VK_F3.0 as u16 => Some(b"\x1bOR"),
        k if k == VK_F4.0 as u16 => Some(b"\x1bOS"),
        k if k == VK_F5.0 as u16 => Some(b"\x1b[15~"),
        k if k == VK_F6.0 as u16 => Some(b"\x1b[17~"),
        _ => None,
    }
}

fn update_font(state: &mut HostState, dpi: u32) {
    let dpi = dpi.max(96);
    unsafe {
        let _ = DeleteObject(state.font.into());
    }
    state.font = unsafe {
        CreateFontW(
            -(18 * dpi as i32 / 96),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            windows::Win32::Graphics::Gdi::FONT_CHARSET(0),
            windows::Win32::Graphics::Gdi::FONT_OUTPUT_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_CLIP_PRECISION(0),
            windows::Win32::Graphics::Gdi::FONT_QUALITY(0),
            0,
            w!("Cascadia Mono"),
        )
    };
    state.dpi = dpi;
    state.cell_width = 11 * dpi as i32 / 96;
    state.cell_height = 24 * dpi as i32 / 96;
}

fn terminal_color(color: VtColor, foreground: bool) -> COLORREF {
    let default = if foreground {
        (230, 230, 230)
    } else {
        (0, 0, 0)
    };
    let (r, g, b) = match color {
        VtColor::Default => default,
        VtColor::Rgb(r, g, b) => (r, g, b),
        VtColor::Idx(index) => ansi_palette(index),
    };
    COLORREF(r as u32 | ((g as u32) << 8) | ((b as u32) << 16))
}

fn ansi_palette(index: u8) -> (u8, u8, u8) {
    const BASIC: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (205, 49, 49),
        (13, 188, 121),
        (229, 229, 16),
        (36, 114, 200),
        (188, 63, 188),
        (17, 168, 205),
        (229, 229, 229),
        (102, 102, 102),
        (241, 76, 76),
        (35, 209, 139),
        (245, 245, 67),
        (59, 142, 234),
        (214, 112, 214),
        (41, 184, 219),
        (255, 255, 255),
    ];
    if index < 16 {
        return BASIC[index as usize];
    }
    if index >= 232 {
        let v = 8 + (index - 232) * 10;
        return (v, v, v);
    }
    let n = index - 16;
    let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
    (level(n / 36), level((n / 6) % 6), level(n % 6))
}

fn draw_terminal_screen(hdc: HDC, state: &HostState, _rect: &RECT) {
    unsafe {
        let screen = state.parser.screen();
        let (cursor_row, cursor_col) = screen.cursor_position();
        // Backgrounds must be painted before glyphs. A CJK glyph occupies two
        // cells; painting the continuation cell afterwards would erase half
        // of the glyph.
        for row in 0..state.rows {
            for col in 0..state.cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let x = col as i32 * state.cell_width;
                let y = row as i32 * state.cell_height;
                let cell_rect = RECT {
                    left: x,
                    top: y,
                    right: x + state.cell_width,
                    bottom: y + state.cell_height,
                };
                let inverse = cell.inverse();
                let bg_color = if inverse {
                    terminal_color(cell.fgcolor(), true)
                } else {
                    terminal_color(cell.bgcolor(), false)
                };
                let bg = CreateSolidBrush(bg_color);
                FillRect(hdc, &cell_rect, bg);
                DeleteObject(bg.into());
            }
        }
        for row in 0..state.rows {
            for col in 0..state.cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let x = col as i32 * state.cell_width;
                let y = row as i32 * state.cell_height;
                let cell_rect = RECT {
                    left: x,
                    top: y,
                    right: x + state.cell_width,
                    bottom: y + state.cell_height,
                };
                let fg_color = if cell.inverse() {
                    terminal_color(cell.bgcolor(), false)
                } else {
                    terminal_color(cell.fgcolor(), true)
                };
                let text = cell.contents();
                if !text.is_empty() && !cell.is_wide_continuation() {
                    SetTextColor(hdc, fg_color);
                    let wide: Vec<u16> = text.encode_utf16().collect();
                    TextOutW(hdc, x, y, &wide);
                    if cell.underline() {
                        let underline = RECT {
                            left: x,
                            top: y + state.cell_height - 2,
                            right: x + state.cell_width,
                            bottom: y + state.cell_height,
                        };
                        let brush = CreateSolidBrush(fg_color);
                        FillRect(hdc, &underline, brush);
                        DeleteObject(brush.into());
                    }
                }
                if state.cursor_visible
                    && !screen.hide_cursor()
                    && row == cursor_row
                    && col == cursor_col
                {
                    let cursor = CreateSolidBrush(COLORREF(0x00E6E6E6));
                    FillRect(hdc, &cell_rect, cursor);
                    DeleteObject(cursor.into());
                    if !text.is_empty() {
                        SetTextColor(hdc, COLORREF(0x00000000));
                        let wide: Vec<u16> = text.encode_utf16().collect();
                        TextOutW(hdc, x, y, &wide);
                    }
                }
            }
        }
    }
}

unsafe extern "system" fn controller_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut AppState;
    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        return LRESULT(1);
    }
    if state_ptr.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let state = &mut *state_ptr;
    match message {
        WM_HOTKEY => {
            let id = wparam.0 as i32 - HOTKEY_BASE;
            if (0..6).contains(&id) {
                state.switch_to(id as usize);
            }
            LRESULT(0)
        }
        WM_TRAY => {
            if lparam.0 as u32 == windows::Win32::UI::WindowsAndMessaging::WM_RBUTTONUP {
                let mut point = POINT::default();
                GetCursorPos(&mut point);
                let menu = CreatePopupMenu().unwrap();
                AppendMenuW(menu, MF_STRING, ID_EXIT, w!("退出"));
                SetForegroundWindow(hwnd);
                TrackPopupMenu(
                    menu,
                    TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                    point.x,
                    point.y,
                    Some(0),
                    hwnd,
                    None,
                );
                windows::Win32::UI::WindowsAndMessaging::DestroyMenu(menu);
            }
            LRESULT(0)
        }
        WM_COMMAND if wparam.0 as usize == ID_EXIT => {
            DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            for id in 0..6 {
                UnregisterHotKey(Some(hwnd), HOTKEY_BASE + id);
            }
            let mut tray = state.tray;
            Shell_NotifyIconW(NIM_DELETE, &mut tray);
            state.shutdown();
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

impl AppState {
    fn switch_to(&mut self, index: usize) {
        if index == 0 {
            let _ = unsafe { SwitchDesktop(self.default_desktop) };
            return;
        }
        if self.desktops[index].is_none() {
            let name = to_wide(&format!("VirtualDesktop{}", index + 1));
            let desktop = match unsafe {
                CreateDesktopW(
                    PCWSTR(name.as_ptr()),
                    None,
                    None,
                    DESKTOP_CONTROL_FLAGS(0),
                    DESKTOP_ACCESS,
                    None,
                )
            } {
                Ok(value) => value,
                Err(error) => {
                    let text = to_wide(&format!("创建 Desktop {} 失败: {error}", index + 1));
                    unsafe {
                        MessageBoxW(
                            Some(self.controller),
                            PCWSTR(text.as_ptr()),
                            w!("Virtual Desktop"),
                            windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                        );
                    }
                    return;
                }
            };
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let desktop_raw = desktop.0 as usize;
            let desktop_name_for_host = format!("VirtualDesktop{}", index + 1);
            let join = match thread::Builder::new()
                .name(format!("desktop-host-{}", index + 1))
                .spawn(move || {
                    host_thread(
                        HDESK(desktop_raw as *mut c_void),
                        desktop_name_for_host,
                        ready_tx,
                    )
                }) {
                Ok(join) => join,
                Err(_) => {
                    unsafe {
                        CloseDesktop(desktop);
                    }
                    return;
                }
            };
            let host = match ready_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(host) => host,
                Err(_) => {
                    unsafe {
                        CloseDesktop(desktop);
                    }
                    return;
                }
            };
            self.desktops[index] = Some(DesktopState {
                desktop,
                host,
                join,
            });
        }
        if let Some(desktop) = &self.desktops[index] {
            let _ = unsafe { SwitchDesktop(desktop.desktop) };
            unsafe {
                PostMessageW(
                    Some(HWND(desktop.host.hwnd as *mut c_void)),
                    WM_ACTIVATE_HOST,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }

    fn shutdown(&mut self) {
        for entry in self.desktops.iter_mut().filter_map(Option::take) {
            unsafe {
                let hwnd = HWND(entry.host.hwnd as *mut c_void);
                if IsWindow(Some(hwnd)).as_bool() {
                    PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                SwitchDesktop(self.default_desktop);
            }
            let _ = entry.join.join();
            unsafe {
                let _ = CloseDesktop(entry.desktop);
            }
        }
        unsafe {
            CloseDesktop(self.default_desktop);
        }
    }
}

fn add_tray_icon(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut tray = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    tray.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap() };
    let tip = to_wide("Virtual Desktop Terminal");
    let tip_len = tip.len().min(tray.szTip.len());
    tray.szTip[..tip_len].copy_from_slice(&tip[..tip_len]);
    unsafe {
        Shell_NotifyIconW(NIM_ADD, &mut tray);
    }
    tray
}

fn main() -> windows::core::Result<()> {
    let instance = unsafe { GetModuleHandleW(None)? };
    let _ = unsafe {
        windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        )
    };
    register_class(
        w!("VirtualDesktopController"),
        controller_wnd_proc,
        unsafe { LoadCursorW(None, IDC_ARROW)? },
    );
    register_class(w!("VirtualDesktopTerminal"), terminal_wnd_proc, unsafe {
        LoadCursorW(None, IDC_ARROW)?
    });
    let default_desktop = unsafe {
        OpenInputDesktop(
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ACCESS_FLAGS(DESKTOP_ACCESS),
        )?
    };
    let mut app = Box::new(AppState {
        default_desktop,
        desktops: std::array::from_fn(|_| None),
        controller: HWND::default(),
        tray: NOTIFYICONDATAW::default(),
    });
    let ptr = &mut *app as *mut AppState;
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE,
            w!("VirtualDesktopController"),
            w!("Virtual Desktop Controller"),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            Some(ptr as *const c_void),
        )?
    };
    app.controller = hwnd;
    app.tray = add_tray_icon(hwnd);
    for id in 0..6 {
        unsafe {
            RegisterHotKey(
                Some(hwnd),
                HOTKEY_BASE + id,
                MOD_CONTROL | MOD_ALT,
                VK_F1.0 as u32 + id as u32,
            )?;
        }
    }
    let mut message = MSG::default();
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    drop(app);
    Ok(())
}
