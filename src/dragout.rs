//! Drag files out of the app to other programs (Explorer, a browser upload
//! box, QQ, …) on Windows, via the OLE `DoDragDrop` + `CF_HDROP` channel.
//!
//! Hand-written COM vtables: windows 0.52 (this project's pin) ships no
//! `#[implement]` macro, and a previous hand-written `IEnumFORMATETC` whose
//! `Next` never terminated crashed the Shell. Here the data object exposes
//! only `CF_HDROP` and `EnumFormatEtc` returns `E_NOTIMPL` — drop targets
//! call `GetData(CF_HDROP)` directly, so no enumerator is needed and there is
//! no termination bug. The HDROP drag image (file icon) is supplied by the OS.
//!
//! `begin_file_drag` MUST run on the UI thread and OUTSIDE any egui widget
//! callback (we defer it to the top of `update`). `DoDragDrop` runs a modal
//! message loop and blocks until the mouse is released; the UI thread is an
//! STA apartment (see `init_com_sta`), so the window stays responsive.

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use std::sync::atomic::{AtomicI32, Ordering};

    const CF_HDROP: u32 = 15;
    const GMEM_MOVEABLE: u32 = 0x0002;
    const TYMED_HGLOBAL: u32 = 1;
    const DROPEFFECT_COPY: u32 = 1;
    const S_OK: i32 = 0;
    const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;
    const E_NOTIMPL: i32 = 0x8000_4001u32 as i32;
    const DV_E_FORMATETC: i32 = 0x8004_0064u32 as i32;
    const OLE_E_ADVISENOTSUPPORTED: i32 = 0x8004_0003u32 as i32;

    const MK_LBUTTON: u32 = 0x0001;
    const MK_RBUTTON: u32 = 0x0002;
    const COINIT_APARTMENTTHREADED: u32 = 0x0000_0002;
    const DRAGDROP_S_DROP: i32 = 0x0004_0100;
    const DRAGDROP_S_CANCEL: i32 = 0x0004_0101;
    const DRAGDROP_S_USEDEFAULTCURSORS: i32 = 0x0004_0102;

    const IID_IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    const IID_IDATAOBJECT: [u8; 16] =
        [0x0E, 0x01, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
    const IID_IDROPSOURCE: [u8; 16] =
        [0x01, 0x01, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];

    #[derive(Clone, Copy)]
    #[repr(C)]
    struct FORMATETC {
        cf_format: u16,
        ptd: *const (),
        dw_aspect: u32,
        lindex: i32,
        tymed: u32,
    }

    #[repr(C)]
    struct STGMEDIUM {
        tymed: u32,
        h_global: usize,
        p_unk_for_release: usize,
    }

    // IDataObject vtable — IUnknown (3) + IDataObject (9) = 12 entries, in the
    // fixed COM order.
    #[repr(C)]
    struct IDataObjectVtbl {
        query_interface: extern "system" fn(*mut IDataObject, *const u8, *mut *mut ()) -> i32,
        add_ref: extern "system" fn(*mut IDataObject) -> u32,
        release: extern "system" fn(*mut IDataObject) -> u32,
        get_data: extern "system" fn(*mut IDataObject, *const FORMATETC, *mut STGMEDIUM) -> i32,
        get_data_here: extern "system" fn(*mut IDataObject, *const FORMATETC, *mut STGMEDIUM) -> i32,
        query_get_data: extern "system" fn(*mut IDataObject, *const FORMATETC) -> i32,
        get_canonical_format_etc:
            extern "system" fn(*mut IDataObject, *const FORMATETC, *mut FORMATETC) -> i32,
        set_data: extern "system" fn(*mut IDataObject, *const FORMATETC, *mut STGMEDIUM, i32) -> i32,
        enum_format_etc: extern "system" fn(*mut IDataObject, u32, *mut *mut ()) -> i32,
        dadvise: extern "system" fn(*mut IDataObject, *const FORMATETC, u32, usize, *mut u32) -> i32,
        dunadvise: extern "system" fn(*mut IDataObject, u32) -> i32,
        enum_dadvise: extern "system" fn(*mut IDataObject, *mut *mut ()) -> i32,
    }

    #[repr(C)]
    struct IDataObject {
        vtbl: *const IDataObjectVtbl,
        refs: AtomicI32,
        drop_hglobal: usize,
    }

    #[repr(C)]
    struct IDropSourceVtbl {
        query_interface: extern "system" fn(*mut IDropSource, *const u8, *mut *mut ()) -> i32,
        add_ref: extern "system" fn(*mut IDropSource) -> u32,
        release: extern "system" fn(*mut IDropSource) -> u32,
        query_continue_drag: extern "system" fn(*mut IDropSource, i32, u32) -> i32,
        give_feedback: extern "system" fn(*mut IDropSource, u32) -> i32,
    }

    #[repr(C)]
    struct IDropSource {
        vtbl: *const IDropSourceVtbl,
        refs: AtomicI32,
    }

    extern "system" fn obj_query_interface(
        this: *mut IDataObject,
        riid: *const u8,
        out: *mut *mut (),
    ) -> i32 {
        unsafe {
            let id = std::slice::from_raw_parts(riid, 16);
            if id == IID_IUNKNOWN || id == IID_IDATAOBJECT {
                *out = this as *mut ();
                (*this).refs.fetch_add(1, Ordering::Relaxed);
                S_OK
            } else {
                *out = ptr::null_mut();
                E_NOINTERFACE
            }
        }
    }
    extern "system" fn obj_add_ref(this: *mut IDataObject) -> u32 {
        unsafe { ((*this).refs.fetch_add(1, Ordering::Relaxed) as u32) + 1 }
    }
    extern "system" fn obj_release(this: *mut IDataObject) -> u32 {
        unsafe {
            let n = (*this).refs.fetch_sub(1, Ordering::Relaxed) - 1;
            if n == 0 {
                let _ = free_global((*this).drop_hglobal);
                drop(Box::from_raw((*this).vtbl as *mut IDataObjectVtbl));
                drop(Box::from_raw(this));
            }
            n as u32
        }
    }
    extern "system" fn obj_get_data(
        this: *mut IDataObject,
        pfmt: *const FORMATETC,
        pmed: *mut STGMEDIUM,
    ) -> i32 {
        unsafe {
            let fmt = &*pfmt;
            if fmt.cf_format as u32 == CF_HDROP && fmt.tymed & TYMED_HGLOBAL != 0 {
                // Hand the receiver a private copy; we keep `drop_hglobal` and
                // free it in `obj_release`. No double-free either way.
                let dup = dup_global((*this).drop_hglobal);
                if dup.is_null() {
                    return E_NOTIMPL;
                }
                *pmed = STGMEDIUM {
                    tymed: TYMED_HGLOBAL,
                    h_global: dup as usize,
                    p_unk_for_release: 0,
                };
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }
    }
    extern "system" fn obj_get_data_here(
        _this: *mut IDataObject,
        _pfmt: *const FORMATETC,
        _pmed: *mut STGMEDIUM,
    ) -> i32 {
        E_NOTIMPL
    }
    extern "system" fn obj_query_get_data(this: *mut IDataObject, pfmt: *const FORMATETC) -> i32 {
        unsafe {
            let _ = this;
            let fmt = &*pfmt;
            if fmt.cf_format as u32 == CF_HDROP {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }
    }
    extern "system" fn obj_get_canonical_format_etc(
        _this: *mut IDataObject,
        pfmt: *const FORMATETC,
        pfmt_out: *mut FORMATETC,
    ) -> i32 {
        unsafe {
            *pfmt_out = *pfmt;
            S_OK
        }
    }
    extern "system" fn obj_set_data(
        _this: *mut IDataObject,
        _pfmt: *const FORMATETC,
        _pmed: *mut STGMEDIUM,
        _frelease: i32,
    ) -> i32 {
        E_NOTIMPL
    }
    extern "system" fn obj_enum_format_etc(
        _this: *mut IDataObject,
        _dwdirection: u32,
        _ppenum: *mut *mut (),
    ) -> i32 {
        // Drop targets retrieve CF_HDROP via GetData directly; a hand-written
        // enumerator was the crash source (Next never terminated). Not needed.
        E_NOTIMPL
    }
    extern "system" fn obj_dadvise(
        _this: *mut IDataObject,
        _pfmt: *const FORMATETC,
        _advf: u32,
        _sink: usize,
        _pcookie: *mut u32,
    ) -> i32 {
        OLE_E_ADVISENOTSUPPORTED
    }
    extern "system" fn obj_dunadvise(_this: *mut IDataObject, _cookie: u32) -> i32 {
        OLE_E_ADVISENOTSUPPORTED
    }
    extern "system" fn obj_enum_dadvise(_this: *mut IDataObject, _ppenum: *mut *mut ()) -> i32 {
        OLE_E_ADVISENOTSUPPORTED
    }

    unsafe fn make_data_object(drop_hglobal: usize) -> *mut IDataObject {
        let vtbl = IDataObjectVtbl {
            query_interface: obj_query_interface,
            add_ref: obj_add_ref,
            release: obj_release,
            get_data: obj_get_data,
            get_data_here: obj_get_data_here,
            query_get_data: obj_query_get_data,
            get_canonical_format_etc: obj_get_canonical_format_etc,
            set_data: obj_set_data,
            enum_format_etc: obj_enum_format_etc,
            dadvise: obj_dadvise,
            dunadvise: obj_dunadvise,
            enum_dadvise: obj_enum_dadvise,
        };
        let vtbl_box = Box::new(vtbl);
        let obj = Box::new(IDataObject {
            vtbl: Box::into_raw(vtbl_box),
            refs: AtomicI32::new(1),
            drop_hglobal,
        });
        Box::into_raw(obj)
    }

    extern "system" fn src_query_interface(
        this: *mut IDropSource,
        riid: *const u8,
        out: *mut *mut (),
    ) -> i32 {
        unsafe {
            let id = std::slice::from_raw_parts(riid, 16);
            if id == IID_IUNKNOWN || id == IID_IDROPSOURCE {
                *out = this as *mut ();
                (*this).refs.fetch_add(1, Ordering::Relaxed);
                S_OK
            } else {
                *out = ptr::null_mut();
                E_NOINTERFACE
            }
        }
    }
    extern "system" fn src_add_ref(this: *mut IDropSource) -> u32 {
        unsafe { ((*this).refs.fetch_add(1, Ordering::Relaxed) as u32) + 1 }
    }
    extern "system" fn src_release(this: *mut IDropSource) -> u32 {
        unsafe {
            let n = (*this).refs.fetch_sub(1, Ordering::Relaxed) - 1;
            if n == 0 {
                drop(Box::from_raw((*this).vtbl as *mut IDropSourceVtbl));
                drop(Box::from_raw(this));
            }
            n as u32
        }
    }
    extern "system" fn src_query_continue_drag(
        _this: *mut IDropSource,
        f_escape_pressed: i32,
        grf_key_state: u32,
    ) -> i32 {
        if f_escape_pressed != 0 {
            return DRAGDROP_S_CANCEL;
        }
        if (grf_key_state & MK_LBUTTON) == 0 && (grf_key_state & MK_RBUTTON) == 0 {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }
    extern "system" fn src_give_feedback(_this: *mut IDropSource, _dw_effect: u32) -> i32 {
        DRAGDROP_S_USEDEFAULTCURSORS
    }

    unsafe fn make_drop_source() -> *mut IDropSource {
        let vtbl = IDropSourceVtbl {
            query_interface: src_query_interface,
            add_ref: src_add_ref,
            release: src_release,
            query_continue_drag: src_query_continue_drag,
            give_feedback: src_give_feedback,
        };
        let vtbl_box = Box::new(vtbl);
        let obj = Box::new(IDropSource {
            vtbl: Box::into_raw(vtbl_box),
            refs: AtomicI32::new(1),
        });
        Box::into_raw(obj)
    }

    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut u8;
        fn GlobalFree(handle: *mut u8) -> *mut u8;
        fn GlobalLock(handle: *mut u8) -> *mut u8;
        fn GlobalUnlock(handle: *mut u8) -> i32;
        fn GlobalSize(handle: *mut u8) -> usize;
        fn OleInitialize(pv: *const ()) -> i32;
        fn CoInitializeEx(pvReserved: *const (), dwCoInit: u32) -> i32;
        fn DoDragDrop(pdo: *mut (), pdsrc: *mut (), dw_ok_effect: u32, pdw_effect: *mut u32) -> i32;
    }

    unsafe fn free_global(h: usize) {
        if h != 0 {
            GlobalFree(h as *mut u8);
        }
    }
    unsafe fn dup_global(h: usize) -> *mut u8 {
        if h == 0 {
            return ptr::null_mut();
        }
        let p = GlobalLock(h as *mut u8);
        if p.is_null() {
            return ptr::null_mut();
        }
        let n = GlobalSize(h as *mut u8);
        let copy = GlobalAlloc(GMEM_MOVEABLE, n);
        if copy.is_null() {
            GlobalUnlock(h as *mut u8);
            return ptr::null_mut();
        }
        let cp = GlobalLock(copy);
        if cp.is_null() {
            GlobalUnlock(h as *mut u8);
            GlobalFree(copy);
            return ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(p, cp, n);
        GlobalUnlock(h as *mut u8);
        GlobalUnlock(copy);
        copy
    }

    fn build_dropfiles(paths: &[std::path::PathBuf]) -> usize {
        unsafe {
            let mut wide: Vec<u16> = Vec::new();
            for p in paths {
                wide.extend(OsStr::new(p).encode_wide());
                wide.push(0);
            }
            wide.push(0);
            let hdr = 20u32; // sizeof(DROPFILES) for the wide layout we use
            let bytes = hdr as usize + wide.len() * 2;
            let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if h.is_null() {
                return 0;
            }
            #[repr(C)]
            struct Dropfiles {
                p_files: u32,
                pt_x: i32,
                pt_y: i32,
                f_nc: i32,
                f_wide: i32,
            }
            let p = GlobalLock(h);
            if p.is_null() {
                GlobalFree(h);
                return 0;
            }
            let df = Dropfiles {
                p_files: hdr,
                pt_x: 0,
                pt_y: 0,
                f_nc: 0,
                f_wide: 1,
            };
            std::ptr::copy_nonoverlapping(
                &df as *const Dropfiles as *const u8,
                p,
                std::mem::size_of::<Dropfiles>(),
            );
            std::ptr::copy_nonoverlapping(
                wide.as_ptr() as *const u8,
                p.add(std::mem::size_of::<Dropfiles>()),
                wide.len() * 2,
            );
            GlobalUnlock(h);
            h as usize
        }
    }

    /// Start an OLE drag of `paths` (blocking until the mouse is released).
    /// Returns true if the drop landed somewhere. Call on the UI thread,
    /// outside any egui widget callback.
    pub fn begin_file_drag(paths: &[std::path::PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        unsafe {
            let drop_hglobal = build_dropfiles(paths);
            if drop_hglobal == 0 {
                return false;
            }
            let obj = make_data_object(drop_hglobal);
            let src = make_drop_source();
            // UI thread must be STA for DoDragDrop to work (see init_com_sta).
            let _ = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED);
            let _ = OleInitialize(ptr::null());
            let mut effect: u32 = 0;
            let hr = DoDragDrop(obj as *mut (), src as *mut (), DROPEFFECT_COPY, &mut effect);
            let _ = src_release(src);
            let _ = obj_release(obj);
            hr == S_OK
        }
    }

    /// Put the calling (UI) thread into an STA apartment and initialize OLE.
    /// Call once at startup, before any window is created.
    pub fn init_com_sta() {
        unsafe {
            let _ = CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED);
            let _ = OleInitialize(ptr::null());
        }
    }
}

#[cfg(target_os = "windows")]
pub use imp::{begin_file_drag, init_com_sta};

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn begin_file_drag(_paths: &[std::path::PathBuf]) -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn init_com_sta() {}
