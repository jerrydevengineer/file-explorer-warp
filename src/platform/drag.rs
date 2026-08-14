use std::path::Path;

#[cfg(not(target_os = "macos"))]
pub fn begin_external_drag(_paths: &[&Path]) -> Result<(), String> {
    Err("Native external drag is only available on macOS".to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn take_drag_ended_op() -> Option<(usize, Vec<std::path::PathBuf>)> { None }

#[cfg(not(target_os = "macos"))]
pub fn cancel_pending_external_drag() {}

#[cfg(target_os = "macos")]
pub fn begin_external_drag(paths: &[&Path]) -> Result<(), String> {
    macos_drag::begin(paths)
}

#[cfg(target_os = "macos")]
pub fn take_drag_ended_op() -> Option<(usize, Vec<std::path::PathBuf>)> {
    macos_drag::take_ended_op()
}

#[cfg(target_os = "macos")]
pub fn cancel_pending_external_drag() {
    macos_drag::cancel_pending()
}

#[cfg(target_os = "macos")]
pub fn is_drag_active() -> bool {
    macos_drag::is_active()
}

#[cfg(target_os = "macos")]
mod macos_drag {
    use std::cell::RefCell;
    use std::ffi::{c_char, c_void};
    use std::path::{Path, PathBuf};

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject, Sel};
    use objc2::{sel, ClassType};
    use objc2_app_kit::{
        NSApplication, NSDragOperation, NSDraggingItem, NSEvent, NSPasteboardWriting, NSView,
        NSWorkspace,
    };
    use objc2_foundation::{
        CGPoint, CGRect, CGSize, MainThreadMarker, NSArray, NSObject, NSString, NSURL,
    };

    extern "C" {
        fn class_getSuperclass(cls: *const c_void) -> *const c_void;
        fn class_getInstanceMethod(cls: *const c_void, name: Sel) -> *mut c_void;
        fn class_addMethod(
            cls: *mut c_void,
            name: Sel,
            imp: *const c_void,
            types: *const c_char,
        ) -> bool;
        fn method_getImplementation(m: *const c_void) -> *const c_void;
        fn objc_getProtocol(name: *const c_char) -> *const c_void;
        fn class_addProtocol(cls: *mut c_void, protocol: *const c_void) -> bool;
        fn class_conformsToProtocol(cls: *const c_void, protocol: *const c_void) -> bool;

        #[link_name = "objc_msgSend"]
        fn begin_drag_session_raw(
            receiver: *const c_void,
            sel: Sel,
            items: *const c_void,
            event: *const c_void,
            source: *const c_void,
        ) -> *const c_void;
    }

    type OrigFn = unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject);

    unsafe fn add_or_verify_method(
        class: *mut c_void,
        selector: Sel,
        implementation: *const c_void,
        encoding: *const c_char,
    ) -> bool {
        if class_addMethod(class, selector, implementation, encoding) {
            return true;
        }
        let method = class_getInstanceMethod(class, selector);
        !method.is_null() && method_getImplementation(method) == implementation
    }

    thread_local! {
        static PENDING_DRAG: RefCell<Option<Vec<PathBuf>>> = RefCell::new(None);
        static HOOK_INSTALLED: RefCell<bool> = RefCell::new(false);
        static ORIG_MOUSE_DRAGGED: RefCell<Option<OrigFn>> = RefCell::new(None);
        static DRAG_ACTIVE: RefCell<bool> = RefCell::new(false);
        static ACTIVE_DRAG_PATHS: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
        static DRAG_ENDED_OP: RefCell<Option<usize>> = RefCell::new(None);
        static DRAG_ENDED_PATHS: RefCell<Option<Vec<PathBuf>>> = RefCell::new(None);
    }

    pub fn take_ended_op() -> Option<(usize, Vec<PathBuf>)> {
        let op = DRAG_ENDED_OP.with(|c| c.borrow_mut().take())?;
        let paths = DRAG_ENDED_PATHS.with(|c| c.borrow_mut().take()).unwrap_or_default();
        Some((op, paths))
    }

    pub fn is_active() -> bool {
        DRAG_ACTIVE.with(|c| *c.borrow())
    }

    pub fn cancel_pending() {
        if !is_active() {
            PENDING_DRAG.with(|pending| *pending.borrow_mut() = None);
        }
    }

    unsafe extern "C" fn our_drag_source_mask(
        _this: *mut AnyObject,
        _cmd: Sel,
        _session: *mut AnyObject,
        _context: isize,
    ) -> usize {
        // External export is copy-only. This keeps source items available for
        // repeated uploads and avoids Finder moving them out of the browser.
        // Moves inside this app are handled by the egui drag paths instead.
        NSDragOperation::Copy.bits()
    }

    // On arm64: NSPoint (two CGFloat doubles) arrives in float registers.
    // Encoding "v@:@{CGPoint=dd}L" tells the runtime about this layout.
    unsafe extern "C" fn our_drag_session_ended(
        _this: *mut AnyObject,
        _cmd: Sel,
        _session: *mut AnyObject,
        _point_x: f64,
        _point_y: f64,
        _op: usize,
    ) {
        let paths = ACTIVE_DRAG_PATHS.with(|c| c.borrow().clone());
        finish_drag(_op, paths);
        eprintln!("[drag] session ended, op={_op}");
    }

    fn finish_drag(op: usize, paths: Vec<PathBuf>) {
        DRAG_ACTIVE.with(|c| *c.borrow_mut() = false);
        ACTIVE_DRAG_PATHS.with(|c| c.borrow_mut().clear());
        PENDING_DRAG.with(|c| *c.borrow_mut() = None);
        DRAG_ENDED_PATHS.with(|c| *c.borrow_mut() = Some(paths));
        DRAG_ENDED_OP.with(|c| *c.borrow_mut() = Some(op));
    }

    pub fn begin(paths: &[&Path]) -> Result<(), String> {
        if paths.is_empty() {
            return Err("Cannot start an empty drag".to_string());
        }
        if DRAG_ACTIVE.with(|c| *c.borrow())
            || PENDING_DRAG.with(|c| c.borrow().is_some())
        {
            return Err("A native drag is already pending or active".to_string());
        }
        ensure_hooked()?;
        let path_bufs: Vec<PathBuf> = paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| path.to_path_buf())
            .collect();
        if path_bufs.is_empty() {
            return Err("None of the dragged paths still exist".to_string());
        }
        PENDING_DRAG.with(|cell| *cell.borrow_mut() = Some(path_bufs));
        Ok(())
    }

    fn ensure_hooked() -> Result<(), String> {
        if HOOK_INSTALLED.with(|installed| *installed.borrow()) {
            return Ok(());
        }

        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let app = NSApplication::sharedApplication(mtm);
            let Some(window) = app.keyWindow() else {
                return Err("Cannot install drag hook without a key window".to_string());
            };
            let Some(view) = window.contentView() else {
                return Err("Cannot install drag hook without a content view".to_string());
            };

            // Inject into the superclass of the view (AccessKitSubclassOfWinitView's parent).
            let view_class = (*view).class() as *const _ as *const c_void;
            let super_class = class_getSuperclass(view_class);
            if super_class.is_null() {
                return Err("Could not find the native view superclass".to_string());
            }

            let method = class_getInstanceMethod(super_class, sel!(mouseDragged:));
            if method.is_null() {
                return Err("Could not find the original mouseDragged: method".to_string());
            }
            let orig_imp = method_getImplementation(method);
            if orig_imp.is_null() {
                return Err("Could not read the original mouseDragged: implementation".to_string());
            }
            let orig_fn: OrigFn = std::mem::transmute(orig_imp);

            let mask_added = add_or_verify_method(
                view_class as *mut c_void,
                sel!(draggingSession:sourceOperationMaskForDraggingContext:),
                our_drag_source_mask as *const c_void,
                b"L@:@l\0".as_ptr() as *const c_char,
            );
            if !mask_added {
                return Err("Could not install the native drag operation mask".to_string());
            }

            let ended_added = add_or_verify_method(
                view_class as *mut c_void,
                sel!(draggingSession:endedAtPoint:operation:),
                our_drag_session_ended as *const c_void,
                b"v@:@{CGPoint=dd}L\0".as_ptr() as *const c_char,
            );
            if !ended_added {
                return Err("Could not install the native drag-end callback".to_string());
            }

            let proto = objc_getProtocol(b"NSDraggingSource\0".as_ptr() as *const c_char);
            if proto.is_null() {
                return Err("Could not find the NSDraggingSource protocol".to_string());
            }
            if !class_conformsToProtocol(view_class, proto)
                && !class_addProtocol(view_class as *mut c_void, proto)
            {
                return Err("Could not add NSDraggingSource protocol conformance".to_string());
            }
            if !class_conformsToProtocol(view_class, proto) {
                return Err("Native view does not conform to NSDraggingSource".to_string());
            }

            // Install mouseDragged: last. Once this method is visible, every required
            // callback and protocol conformance is already in place.
            let mouse_added = add_or_verify_method(
                view_class as *mut c_void,
                sel!(mouseDragged:),
                our_mouse_dragged as *const c_void,
                b"v@:@\0".as_ptr() as *const c_char,
            );
            if !mouse_added {
                return Err("Could not install the native mouse drag hook".to_string());
            }
            ORIG_MOUSE_DRAGGED.with(|c| *c.borrow_mut() = Some(orig_fn));
            HOOK_INSTALLED.with(|installed| *installed.borrow_mut() = true);
            eprintln!("[drag] native drag hook installed");
        }
        Ok(())
    }

    unsafe extern "C" fn our_mouse_dragged(this: *mut AnyObject, cmd: Sel, event: *mut AnyObject) {
        // Check PENDING_DRAG FIRST — before forwarding to winit.
        // If we forward first, winit triggers an egui update which can set PENDING_DRAG again
        // before DRAG_ACTIVE is set, causing a second session to start.
        let pending = PENDING_DRAG.with(|c| c.borrow_mut().take());
        if let Some(paths) = pending {
            if !this.is_null() && !event.is_null() {
                let view = &*(this as *const NSView);
                let ns_event = &*(event as *const NSEvent);
                if start_drag(view, this as *const c_void, ns_event, &paths) {
                    // Do NOT forward to winit — the OS drag loop owns it from here.
                    return;
                }
            }
            finish_drag(NSDragOperation::None.bits(), paths);
        }

        ORIG_MOUSE_DRAGGED.with(|c| {
            if let Some(orig) = *c.borrow() {
                orig(this, cmd, event);
            }
        });
    }

    unsafe fn start_drag(
        view: &NSView,
        source: *const c_void,
        event: &NSEvent,
        paths: &[PathBuf],
    ) -> bool {
        if paths.is_empty() {
            return false;
        }
        let win_pt = event.locationInWindow();
        let view_pt = view.convertPoint_fromView(win_pt, None);
        let frame = CGRect::new(
            CGPoint::new(view_pt.x - 16.0, view_pt.y - 16.0),
            CGSize::new(32.0, 32.0),
        );

        let workspace = NSWorkspace::sharedWorkspace();
        let mut drag_items: Vec<Retained<NSDraggingItem>> = Vec::new();

        for path in paths {
            let path_str = path.to_string_lossy().to_string();
            let ns_path = NSString::from_str(&path_str);
            let url = NSURL::fileURLWithPath(&ns_path);
            let url_pw = ProtocolObject::<dyn NSPasteboardWriting>::from_ref(&*url);
            let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), url_pw);
            let icon = workspace.iconForFile(&ns_path);
            let icon_ns: Retained<NSObject> = Retained::into_super(icon);
            let icon_any: Retained<AnyObject> = Retained::into_super(icon_ns);
            item.setDraggingFrame_contents(frame, Some(&*icon_any));
            drag_items.push(item);
        }

        let item_refs: Vec<&NSDraggingItem> = drag_items.iter().map(|i| i.as_ref()).collect();
        let items_array = NSArray::from_slice(&item_refs);

        // Use raw objc_msgSend — the objc2-app-kit binding panics on nil return.
        DRAG_ACTIVE.with(|c| *c.borrow_mut() = true);
        ACTIVE_DRAG_PATHS.with(|c| *c.borrow_mut() = paths.to_vec());
        let session = begin_drag_session_raw(
            view as *const NSView as *const c_void,
            sel!(beginDraggingSessionWithItems:event:source:),
            &*items_array as *const NSArray<NSDraggingItem> as *const c_void,
            event as *const NSEvent as *const c_void,
            source,
        );

        if session.is_null() {
            eprintln!("[drag] ERROR: beginDraggingSession returned nil");
            DRAG_ACTIVE.with(|c| *c.borrow_mut() = false);
            ACTIVE_DRAG_PATHS.with(|c| c.borrow_mut().clear());
            false
        } else {
            eprintln!("[drag] OK: session={session:?}");
            true
        }
    }
}
