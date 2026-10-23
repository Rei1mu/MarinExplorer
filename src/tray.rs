//! Windows system tray for MarinExplorer.
//!
//! A background thread owns a hidden, message-only window and a notify icon
//! with a three-item menu: 打开 (Open) / 重启 (Restart) / 退出 (Quit). The main
//! window HWND is shared through a static so the tray can show / restart /
//! close it. The icon is taken from this executable's embedded RT_GROUP_ICON.
//!
//! Only compiled on Windows; other platforms get empty stubs so the call
//! sites stay platform-agnostic.

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use std::sync::Mutex;

    static MAIN_HWND: Mutex<Option<isize>> = Mutex::new(None);
    static TRAY_HWND: Mutex<Option<isize>> = Mutex::new(None);

    const WM_USER: u32 = 0x0400;
    const TRAY_CALLBACK: u32 = WM_USER + 1;
    const WM_TRAY_QUIT: u32 = WM_USER + 2;
    const ID_OPEN: u32 = 1;
    const ID_RESTART: u32 = 2;
    const ID_QUIT: u32 = 3;
    /// Standard "please close" for the main window. eframe's window procedure
    /// turns it into the same flow as the X button (which runs `App::save`),
    /// so the tray's 退出 persists settings just like closing by hand.
    const WM_CLOSE: u32 = 0x0010;

    const NIM_ADD: u32 = 0;
    const NIM_DELETE: u32 = 2;
    const NIF_MESSAGE: u32 = 0x0000_0001;
    const NIF_ICON: u32 = 0x0000_0002;
    const NIF_TIP: u32 = 0x0000_0004;
    const MF_STRING: u32 = 0x0000_0000;
    const TPM_RETURNCMD: u32 = 0x0000_0100;

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    #[repr(C)]
    struct WNDCLASSEXW {
        cb_size: u32,
        style: u32,
        lpfn_wnd_proc: usize,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: usize,
        h_icon: usize,
        h_cursor: usize,
        hbr_background: usize,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
        h_icon_sm: usize,
    }

    #[repr(C)]
    struct MSG {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
        lprivate: usize, // extra info pointer — keeps the struct at 48 bytes
    }

    #[repr(C)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct NOTIFYICONDATAW {
        cb_size: u32,
        h_wnd: isize,
        u_id: u32,
        u_flags: u32,
        u_callback_message: u32,
        h_icon: usize,
        sz_tip: [u16; 128],
        dw_state: u32,
        dw_state_mask: u32,
        sz_info: [u16; 256],
        u_version: u32,
        sz_info_title: [u16; 64],
        dw_info_flags: u32,
        guid_item: [u8; 16],
        h_balloon_icon: usize,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(lp: *const WNDCLASSEXW) -> u16;
        fn CreateWindowExW(
            dw_ex_style: u32,
            lp_class_name: *const u16,
            lp_window_name: *const u16,
            dw_style: u32,
            x: i32,
            y: i32,
            n_width: i32,
            n_height: i32,
            h_wnd_parent: isize,
            h_menu: usize,
            h_instance: usize,
            lp_param: *const (),
        ) -> isize;
        fn DefWindowProcW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize;
        fn DestroyWindow(hwnd: isize) -> i32;
        fn GetMessageW(lp: *mut MSG, hwnd: isize, msg_filter_min: u32, msg_filter_max: u32) -> i32;
        fn TranslateMessage(lp: *const MSG) -> i32;
        fn DispatchMessageW(lp: *const MSG) -> isize;
        fn PostQuitMessage() -> ();
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
        fn GetCursorPos(lp: *mut POINT) -> i32;
        fn CreatePopupMenu() -> usize;
        fn AppendMenuW(hmenu: usize, flags: u32, id: usize, text: *const u16) -> i32;
        fn TrackPopupMenu(
            hmenu: usize,
            flags: u32,
            x: i32,
            y: i32,
            _reserved: i32,
            hwnd: isize,
            _rect: *const (),
        ) -> u32;
        fn DestroyMenu(hmenu: usize) -> i32;
        fn Shell_NotifyIconW(dw_message: u32, nid: *const NOTIFYICONDATAW) -> i32;
        fn ExtractIconW(hinst: usize, lpz_file: *const u16, n_icon_index: u32) -> *mut ();
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(lp: *const u16) -> usize;
        fn GetModuleFileNameW(hmod: usize, lp: *mut u16, nsize: u32) -> u32;
        fn CreateProcessW(
            lp_application_name: *const u16,
            lp_command_line: *mut u16,
            lp_process_attributes: *const (),
            lp_thread_attributes: *const (),
            b_inherit_handles: i32,
            dw_creation_flags: u32,
            lp_environment: *const (),
            lp_current_directory: *const u16,
            lp_startup_info: *const STARTUPINFOW,
            lp_process_information: *mut PROCESS_INFORMATION,
        ) -> i32;
        fn CloseHandle(h: usize) -> i32;
    }

    #[repr(C)]
    struct STARTUPINFOW {
        cb: u32,
        _rest: [u8; 64],
    }

    #[repr(C)]
    struct PROCESS_INFORMATION {
        h_process: usize,
        h_thread: usize,
        dw_process_id: u32,
        dw_thread_id: u32,
    }

    unsafe extern "system" fn window_proc(
        hwnd: isize,
        msg: u32,
        _wparam: usize,
        lparam: isize,
    ) -> isize {
        match msg {
            TRAY_CALLBACK => {
                let m = (lparam & 0xffff) as u32;
                if m == 0x0205 || m == 0x007B {
                    show_menu(hwnd);
                } else if m == 0x0202 {
                    open_main();
                }
                0
            }
            WM_TRAY_QUIT => {
                delete_icon(hwnd);
                destroy_window(hwnd);
                post_quit_message();
                0
            }
            _ => def_window_proc(hwnd, msg, _wparam, lparam),
        }
    }

    fn show_menu(hwnd: isize) {
        unsafe {
            let menu = create_popup_menu();
            if menu == 0 {
                return;
            }
            let open_w = wide("打开");
            let restart_w = wide("重启");
            let quit_w = wide("退出");
            append_menu(menu, MF_STRING, ID_OPEN, open_w.as_ptr());
            append_menu(menu, MF_STRING, ID_RESTART, restart_w.as_ptr());
            append_menu(menu, MF_STRING, ID_QUIT, quit_w.as_ptr());
            set_foreground_window(hwnd);
            let pt = get_cursor_pos();
            let cmd = track_popup_menu(menu, TPM_RETURNCMD, pt.0, pt.1, hwnd);
            destroy_menu(menu);
            // Release the foreground lock Windows imposes on the menu owner.
            post_message(hwnd, 0, 0, 0);
            match cmd {
                ID_OPEN => open_main(),
                ID_RESTART => restart_main(hwnd),
                ID_QUIT => quit_app(hwnd),
                _ => {}
            }
        }
    }

    /// Tray "退出": drop the icon first (a process that dies with a live
    /// notify icon leaves a ghost in the tray until the mouse hovers over it),
    /// then ask the main window to close the normal way so settings flush,
    /// then stop this thread's message loop.
    fn quit_app(tray_hwnd: isize) {
        delete_icon(tray_hwnd);
        if let Some(hwnd) = *MAIN_HWND.lock().unwrap() {
            unsafe {
                post_message(hwnd, WM_CLOSE, 0, 0);
            }
        }
        unsafe {
            post_message(tray_hwnd, WM_TRAY_QUIT, 0, 0);
        }
    }

    fn open_main() {
        if let Some(hwnd) = *MAIN_HWND.lock().unwrap() {
            unsafe {
                show_window(hwnd, 9); // SW_RESTORE
                set_foreground_window(hwnd);
            }
        }
    }

    fn restart_main(tray_hwnd: isize) {
        unsafe {
            if let Some(exe) = current_exe() {
                // --take-over: the new process must *replace* this instance,
                // not activate it — it would activate the window we are
                // about to close and then exit, leaving nothing running.
                // Quoted because the path contains spaces and CreateProcess
                // parses the command line.
                let cmd = format!("\"{}\" --take-over", exe);
                let _ = create_process(&cmd);
            }
            delete_icon(tray_hwnd);
            destroy_window(tray_hwnd);
            post_quit_message();
            if let Some(hwnd) = *MAIN_HWND.lock().unwrap() {
                post_message(hwnd, 0x0010, 0, 0); // WM_CLOSE
            }
        }
    }

    fn current_exe() -> Option<String> {
        unsafe {
            let mut buf = [0u16; 1024];
            let n = get_module_file_name_w(0, buf.as_mut_ptr(), buf.len() as u32);
            if n == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..n as usize]))
        }
    }

    fn delete_icon(hwnd: isize) {
        unsafe {
            let nid = NOTIFYICONDATAW {
                cb_size: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                h_wnd: hwnd,
                u_id: 1,
                u_flags: 0,
                u_callback_message: 0,
                h_icon: 0,
                sz_tip: [0; 128],
                dw_state: 0,
                dw_state_mask: 0,
                sz_info: [0; 256],
                u_version: 0,
                sz_info_title: [0; 64],
                dw_info_flags: 0,
                guid_item: [0; 16],
                h_balloon_icon: 0,
            };
            shell_notify_icon_w(NIM_DELETE, &nid);
        }
    }

    fn add_icon(hwnd: isize) -> bool {
        unsafe {
            let exe = current_exe().unwrap_or_default();
            let exew: Vec<u16> = OsStr::new(&exe).encode_wide().chain(std::iter::once(0)).collect();
            let hicon = extract_icon_w(get_module_handle_w(std::ptr::null()), exew.as_ptr(), 0);
            let tip = wide("MarinExplorer");
            let mut sz_tip = [0u16; 128];
            for (i, &c) in tip.iter().take(127).enumerate() {
                sz_tip[i] = c;
            }
            let nid = NOTIFYICONDATAW {
                cb_size: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                h_wnd: hwnd,
                u_id: 1,
                u_flags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                u_callback_message: TRAY_CALLBACK,
                h_icon: if hicon.is_null() { 0 } else { hicon as usize },
                sz_tip,
                dw_state: 0,
                dw_state_mask: 0,
                sz_info: [0; 256],
                u_version: 0,
                sz_info_title: [0; 64],
                dw_info_flags: 0,
                guid_item: [0; 16],
                h_balloon_icon: 0,
            };
            shell_notify_icon_w(NIM_ADD, &nid) != 0
        }
    }

    fn run() {
        unsafe {
            let class_name = wide("MarinTrayCls");
            let hinst = get_module_handle_w(std::ptr::null());
            let wcex = WNDCLASSEXW {
                cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfn_wnd_proc: window_proc as usize,
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance: hinst,
                h_icon: 0,
                h_cursor: 0,
                hbr_background: 0,
                lpsz_menu_name: ptr::null(),
                lpsz_class_name: class_name.as_ptr(),
                h_icon_sm: 0,
            };
            register_class_ex_w(&wcex);
            let hwnd = create_window_ex_w(
                0,
                class_name.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                -3isize, // HWND_MESSAGE → message-only window
                0,
                hinst,
                ptr::null(),
            );
            if hwnd == 0 {
                return;
            }
            *TRAY_HWND.lock().unwrap() = Some(hwnd);
            // Remove any stale icon from a previous run that used the same id.
            delete_icon(hwnd);
            add_icon(hwnd);

            let mut raw: MSG = std::mem::zeroed();
            loop {
                let r = get_message_w(&mut raw, 0, 0, 0);
                if r == 0 || r == -1 {
                    break;
                }
                translate_message(&raw);
                dispatch_message_w(&raw);
            }
        }
    }

    unsafe fn register_class_ex_w(lp: *const WNDCLASSEXW) -> u16 {
        RegisterClassExW(lp)
    }
    unsafe fn create_window_ex_w(
        a: u32, b: *const u16, c: *const u16, d: u32, e: i32, f: i32, g: i32, h: i32,
        i: isize, j: usize, k: usize, l: *const (),
    ) -> isize {
        CreateWindowExW(a, b, c, d, e, f, g, h, i, j, k, l)
    }
    unsafe fn get_message_w(lp: *mut MSG, hwnd: isize, mn: u32, mx: u32) -> i32 {
        GetMessageW(lp, hwnd, mn, mx)
    }
    unsafe fn translate_message(lp: *const MSG) -> i32 {
        TranslateMessage(lp)
    }
    unsafe fn dispatch_message_w(lp: *const MSG) -> isize {
        DispatchMessageW(lp)
    }
    unsafe fn def_window_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
    unsafe fn destroy_window(hwnd: isize) -> i32 {
        DestroyWindow(hwnd)
    }
    unsafe fn show_window(hwnd: isize, cmd: i32) -> i32 {
        ShowWindow(hwnd, cmd)
    }
    unsafe fn set_foreground_window(hwnd: isize) -> i32 {
        SetForegroundWindow(hwnd)
    }
    unsafe fn post_message(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32 {
        PostMessageW(hwnd, msg, wparam, lparam)
    }
    unsafe fn post_quit_message() {
        PostQuitMessage();
    }
    unsafe fn get_cursor_pos() -> (i32, i32) {
        let mut p = POINT { x: 0, y: 0 };
        GetCursorPos(&mut p);
        (p.x, p.y)
    }
    unsafe fn create_popup_menu() -> usize {
        CreatePopupMenu()
    }
    unsafe fn append_menu(menu: usize, flags: u32, id: u32, text: *const u16) -> i32 {
        AppendMenuW(menu, flags, id as usize, text)
    }
    unsafe fn track_popup_menu(menu: usize, flags: u32, x: i32, y: i32, hwnd: isize) -> u32 {
        TrackPopupMenu(menu, flags, x, y, 0, hwnd, ptr::null())
    }
    unsafe fn destroy_menu(menu: usize) -> i32 {
        DestroyMenu(menu)
    }
    unsafe fn shell_notify_icon_w(dw: u32, nid: *const NOTIFYICONDATAW) -> i32 {
        Shell_NotifyIconW(dw, nid)
    }
    unsafe fn get_module_handle_w(lp: *const u16) -> usize {
        GetModuleHandleW(lp)
    }
    unsafe fn get_module_file_name_w(hmod: usize, lp: *mut u16, nsize: u32) -> u32 {
        GetModuleFileNameW(hmod, lp, nsize)
    }
    unsafe fn create_process(exe: &str) -> bool {
        let mut cmd: Vec<u16> = OsStr::new(exe).encode_wide().chain(std::iter::once(0)).collect();
        let mut si: STARTUPINFOW = std::mem::zeroed();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let r = CreateProcessW(
            ptr::null(),
            cmd.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            0,
            ptr::null(),
            ptr::null(),
            &si,
            &mut pi,
        );
        if r != 0 {
            close_handle(pi.h_process);
            close_handle(pi.h_thread);
            true
        } else {
            false
        }
    }
    unsafe fn close_handle(h: usize) -> i32 {
        if h == 0 {
            0
        } else {
            CloseHandle(h)
        }
    }
    unsafe fn extract_icon_w(hinst: usize, file: *const u16, idx: u32) -> *mut () {
        ExtractIconW(hinst, file, idx)
    }

    pub fn set_main_hwnd(hwnd: isize) {
        *MAIN_HWND.lock().unwrap() = Some(hwnd);
    }

    pub fn start() {
        std::thread::spawn(|| run());
    }
}

#[cfg(target_os = "windows")]
pub use imp::{set_main_hwnd, start};

#[cfg(not(target_os = "windows"))]
pub fn set_main_hwnd(_hwnd: isize) {}
#[cfg(not(target_os = "windows"))]
pub fn start() {}
