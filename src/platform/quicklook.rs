use std::path::Path;

#[cfg(target_os = "macos")]
pub fn open_quicklook(path: &Path) {
    macos_ql::show(path);
}

#[cfg(not(target_os = "macos"))]
pub fn open_quicklook(_path: &Path) {}

// ── macOS implementation ──────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos_ql {
    use std::cell::RefCell;
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{declare_class, msg_send, msg_send_id, ClassType, DeclaredClass};
    use objc2::mutability::InteriorMutable;
    use objc2_foundation::{NSObject, NSString, NSURL};

    // Pull in QuickLookUI so QLPreviewPanel is resolvable at runtime.
    #[link(name = "QuickLookUI", kind = "framework")]
    extern "C" {}

    // ── Shared state ─────────────────────────────────────────────────────────

    thread_local! {
        /// URL of the file/folder currently being previewed.
        static CURRENT_URL: RefCell<Option<Retained<NSURL>>> = RefCell::new(None);
    }

    // ── Data source class ─────────────────────────────────────────────────────
    //
    // Minimal QLPreviewPanelDataSource. We set it directly on the panel via
    // setDataSource: before calling makeKeyAndOrderFront:. As long as nothing
    // in the responder chain claims the panel (acceptsPreviewPanelControl:),
    // QLPreviewPanel keeps our data source and calls these two methods.

    declare_class!(
        struct FEPreviewDataSource;

        unsafe impl ClassType for FEPreviewDataSource {
            type Super = NSObject;
            type Mutability = InteriorMutable;
            const NAME: &'static str = "FEPreviewDataSource";
        }

        impl DeclaredClass for FEPreviewDataSource {
            type Ivars = ();
        }

        unsafe impl FEPreviewDataSource {
            #[method(numberOfPreviewItemsInPreviewPanel:)]
            fn n_items(&self, _panel: &AnyObject) -> isize {
                1
            }

            #[method(previewPanel:previewItemAtIndex:)]
            fn item_at(&self, _panel: &AnyObject, _idx: isize) -> *mut AnyObject {
                // Return +0 pointer; QLPreviewPanel retains it independently.
                CURRENT_URL.with(|cell| {
                    cell.borrow().as_ref().map_or(std::ptr::null_mut(), |url| {
                        let r: &NSURL = url;
                        r as *const NSURL as *mut NSURL as *mut AnyObject
                    })
                })
            }
        }
    );

    thread_local! {
        static DATA_SOURCE: RefCell<Option<Retained<FEPreviewDataSource>>> = RefCell::new(None);
    }

    fn ensure_data_source() {
        DATA_SOURCE.with(|cell| {
            if cell.borrow().is_some() {
                return;
            }
            unsafe {
                let ds: Retained<FEPreviewDataSource> =
                    msg_send_id![FEPreviewDataSource::alloc(), init];
                *cell.borrow_mut() = Some(ds);
            }
        });
    }

    // ── Public entry point ────────────────────────────────────────────────────

    pub fn show(path: &Path) {
        // Update the shared URL before the panel queries the data source.
        CURRENT_URL.with(|cell| {
            let ns_path = NSString::from_str(&path.to_string_lossy());
            let url = unsafe { NSURL::fileURLWithPath(&ns_path) };
            *cell.borrow_mut() = Some(url);
        });

        ensure_data_source();

        unsafe {
            let Some(cls) = AnyClass::get("QLPreviewPanel") else {
                return;
            };
            let panel: *mut AnyObject = msg_send![cls, sharedPreviewPanel];
            if panel.is_null() {
                return;
            }

            // Wire our data source directly — no responder chain manipulation needed.
            DATA_SOURCE.with(|cell| {
                let borrow = cell.borrow();
                if let Some(ds) = borrow.as_ref() {
                    let ds_ptr = &**ds as *const FEPreviewDataSource as *mut AnyObject;
                    let _: () = msg_send![panel, setDataSource: ds_ptr];
                }
            });

            let nil: *const AnyObject = std::ptr::null();
            let is_visible: bool = msg_send![panel, isVisible];

            if is_visible {
                let _: () = msg_send![panel, reloadData];
            } else {
                let _: () = msg_send![panel, makeKeyAndOrderFront: nil];
            }
        }
    }
}
