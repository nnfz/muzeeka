//! Hand a track file to Windows when a drag leaves the window.
//!
//! GPUI only promotes that gesture on macOS and Linux. Here the pointer
//! leaving the window starts an OLE file drag with `DROPEFFECT_COPY`, so
//! Telegram, Explorer, and the rest can take the file without deleting it.

use std::ffi::OsStr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

static OUTGOING: AtomicBool = AtomicBool::new(false);

/// True while the OLE loop is running, including a drop back onto this window.
pub(crate) fn is_outgoing() -> bool {
    OUTGOING.load(Ordering::SeqCst)
}

/// Claim the outgoing drag. A second move must not start another `DoDragDrop`.
pub(crate) fn try_begin() -> bool {
    !OUTGOING.swap(true, Ordering::SeqCst)
}

/// Run the drag already claimed by [`try_begin`]. The flag clears when this returns.
///
/// Call this outside any `App` borrow. `DoDragDrop` pumps the window, and a
/// nested mouse or drop message borrows `App` again.
pub(crate) fn run_armed(paths: &[impl AsRef<Path>]) {
    let _guard = OutgoingGuard;
    let files: Vec<&Path> = paths
        .iter()
        .map(AsRef::as_ref)
        .filter(|path| path.is_file())
        .collect();
    let bytes = hdrop_bytes(&files);
    if bytes.is_empty() {
        return;
    }
    #[cfg(windows)]
    drag_files_win(&bytes);
}

/// `CF_HDROP` payload: a 20-byte `DROPFILES` header plus UTF-16 paths.
pub(crate) fn hdrop_bytes(paths: &[impl AsRef<OsStr>]) -> Vec<u8> {
    let mut wide = Vec::<u16>::new();
    for path in paths {
        let text = path.as_ref();
        if text.is_empty() {
            continue;
        }
        wide.extend(encode_wide(text));
        wide.push(0);
    }
    if wide.is_empty() {
        return Vec::new();
    }
    wide.push(0);

    // DROPFILES is packed: DWORD + POINT + two BOOLs.
    const HEADER: usize = 20;
    let mut bytes = Vec::with_capacity(HEADER + wide.len() * 2);
    bytes.extend_from_slice(&(HEADER as u32).to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&0i32.to_le_bytes());
    bytes.extend_from_slice(&1i32.to_le_bytes());
    for unit in wide {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn encode_wide(text: &OsStr) -> Vec<u16> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        text.encode_wide().collect()
    }
    #[cfg(not(windows))]
    {
        text.to_string_lossy().encode_utf16().collect()
    }
}

struct OutgoingGuard;

impl Drop for OutgoingGuard {
    fn drop(&mut self) {
        OUTGOING.store(false, Ordering::SeqCst);
    }
}

#[cfg(windows)]
mod win {
    use windows::core::{implement, ComObject};
    use windows::Win32::Foundation::{
        DATA_S_SAMEFORMATETC, DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS,
        DV_E_FORMATETC, E_NOTIMPL, E_OUTOFMEMORY, OLE_E_ADVISENOTSUPPORTED, S_OK,
    };
    use windows::Win32::System::Com::{
        IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, DATADIR_GET,
        DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT,
    };
    use windows::Win32::System::Ole::{
        DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, CF_HDROP, DROPEFFECT,
        DROPEFFECT_COPY,
    };
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
    use windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc;

    #[implement(IDataObject, IDropSource)]
    struct FileDrag {
        bytes: Vec<u8>,
    }

    impl IDataObject_Impl for FileDrag_Impl {
        fn GetData(&self, format: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            if !accepts(format) {
                return Err(DV_E_FORMATETC.into());
            }
            let handle = global_hdrop(&self.bytes)?;
            Ok(STGMEDIUM {
                tymed: TYMED_HGLOBAL.0 as u32,
                u: STGMEDIUM_0 { hGlobal: handle },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            })
        }

        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn QueryGetData(&self, format: *const FORMATETC) -> windows::core::HRESULT {
            if accepts(format) {
                S_OK
            } else {
                DV_E_FORMATETC
            }
        }

        fn GetCanonicalFormatEtc(
            &self,
            incoming: *const FORMATETC,
            outgoing: *mut FORMATETC,
        ) -> windows::core::HRESULT {
            if outgoing.is_null() || incoming.is_null() {
                return E_NOTIMPL;
            }
            unsafe {
                *outgoing = *incoming;
                (*outgoing).ptd = std::ptr::null_mut();
            }
            DATA_S_SAMEFORMATETC
        }

        fn SetData(
            &self,
            _: *const FORMATETC,
            _: *const STGMEDIUM,
            _: windows::core::BOOL,
        ) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }

        fn EnumFormatEtc(&self, direction: u32) -> windows::core::Result<IEnumFORMATETC> {
            if direction != DATADIR_GET.0 as u32 {
                return Err(E_NOTIMPL.into());
            }
            unsafe { SHCreateStdEnumFmtEtc(&[hdrop_format()]) }
        }

        fn DAdvise(
            &self,
            _: *const FORMATETC,
            _: u32,
            _: windows::core::Ref<'_, IAdviseSink>,
        ) -> windows::core::Result<u32> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }

        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(OLE_E_ADVISENOTSUPPORTED.into())
        }
    }

    impl IDropSource_Impl for FileDrag_Impl {
        fn QueryContinueDrag(
            &self,
            escape: windows::core::BOOL,
            keys: MODIFIERKEYS_FLAGS,
        ) -> windows::core::HRESULT {
            if escape.as_bool() {
                return DRAGDROP_S_CANCEL;
            }
            if keys.0 & MK_LBUTTON.0 == 0 {
                DRAGDROP_S_DROP
            } else {
                S_OK
            }
        }

        fn GiveFeedback(&self, _: DROPEFFECT) -> windows::core::HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    fn accepts(format: *const FORMATETC) -> bool {
        if format.is_null() {
            return false;
        }
        let format = unsafe { &*format };
        format.cfFormat == CF_HDROP.0
            && format.dwAspect == DVASPECT_CONTENT.0
            && format.tymed & (TYMED_HGLOBAL.0 as u32) != 0
    }

    fn hdrop_format() -> FORMATETC {
        FORMATETC {
            cfFormat: CF_HDROP.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        }
    }

    fn global_hdrop(bytes: &[u8]) -> windows::core::Result<windows::Win32::Foundation::HGLOBAL> {
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes.len())?;
            let locked = GlobalLock(handle);
            if locked.is_null() {
                return Err(E_OUTOFMEMORY.into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked.cast(), bytes.len());
            let _ = GlobalUnlock(handle);
            Ok(handle)
        }
    }

    pub(super) fn drag(bytes: &[u8]) {
        let object = ComObject::new(FileDrag {
            bytes: bytes.to_vec(),
        });
        let data = object.to_interface::<IDataObject>();
        let source = object.to_interface::<IDropSource>();
        let mut effect = DROPEFFECT(0);
        unsafe {
            // Already initialized by the window's drop target. A second call is fine.
            let _ = OleInitialize(None);
            let _ = ReleaseCapture();
            let _ = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
        }
    }
}

#[cfg(windows)]
fn drag_files_win(bytes: &[u8]) {
    win::drag(bytes);
}

#[cfg(test)]
mod tests {
    use super::hdrop_bytes;

    #[test]
    fn hdrop_lists_unicode_paths_after_a_20_byte_header() {
        let bytes = hdrop_bytes(&[r"C:\music\a.mp3", r"D:\b.flac"]);
        assert_eq!(&bytes[..4], &20u32.to_le_bytes());
        assert_eq!(&bytes[16..20], &1i32.to_le_bytes());
        let wide: Vec<u16> = bytes[20..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16_lossy(&wide);
        assert!(text.starts_with("C:\\music\\a.mp3\0D:\\b.flac\0\0"));
    }

    #[test]
    fn hdrop_is_empty_without_paths() {
        let empty: [&str; 0] = [];
        assert!(hdrop_bytes(&empty).is_empty());
    }
}
