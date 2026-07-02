use std::path::Path;

#[cfg(not(target_os = "macos"))]
pub fn begin_external_drag(_paths: &[&Path]) {}

#[cfg(not(target_os = "macos"))]
pub fn take_drag_ended_op() -> Option<(usize, Vec<std::path::PathBuf>)> { None }

#[cfg(target_os = "macos")]
pub fn begin_external_drag(paths: &[&Path]) {
    macos_drag::begin(paths);
}

#[cfg(target_os = "macos")]
pub fn take_drag_ended_op() -> Option<(usize, Vec<std::path::PathBuf>)> {
    macos_drag::take_ended_op()
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
        NSApplication, NSDraggingItem, NSEvent, NSPasteboardWriting, NSView, NSWorkspace,
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

    thread_local! {
        static PENDING_DRAG: RefCell<Option<Vec<PathBuf>>> = RefCell::new(None);
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

    unsafe extern "C" fn our_drag_source_mask(
        _this: *mut AnyObject,
        _cmd: Sel,
        _session: *mut AnyObject,
        _context: isize,
    ) -> usize {
        // Return 16 (NSDragOperationMove): Finder checks for conflicts BEFORE renaming.
        // Stop → op=0 → skip reload → file stays. Copy (1) fires before user sees dialog.
        16
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
        DRAG_ACTIVE.with(|c| *c.borrow_mut() = false);
        let paths = ACTIVE_DRAG_PATHS.with(|c| c.borrow().clone());
        ACTIVE_DRAG_PATHS.with(|c| c.borrow_mut().clear());
        DRAG_ENDED_PATHS.with(|c| *c.borrow_mut() = Some(paths));
        DRAG_ENDED_OP.with(|c| *c.borrow_mut() = Some(_op));
        eprintln!("[drag] session ended, op={_op}");
    }

    pub fn begin(paths: &[&Path]) {
        if DRAG_ACTIVE.with(|c| *c.borrow()) {
            return;
        }
        let path_bufs: Vec<PathBuf> = paths.iter().map(|p| p.to_path_buf()).collect();
        PENDING_DRAG.with(|cell| *cell.borrow_mut() = Some(path_bufs));
        ensure_hooked();
    }

    fn ensure_hooked() {
        let already = ORIG_MOUSE_DRAGGED.with(|c| c.borrow().is_some());
        if already {
            return;
        }

        unsafe {
            let mtm = MainThreadMarker::new_unchecked();
            let app = NSApplication::sharedApplication(mtm);
            let Some(window) = app.keyWindow() else { return };
            let Some(view) = window.contentView() else { return };

            // Inject into the superclass of the view (AccessKitSubclassOfWinitView's parent).
            let view_class = (*view).class() as *const _ as *const c_void;
            let super_class = class_getSuperclass(view_class);
            if super_class.is_null() { return; }

            let method = class_getInstanceMethod(super_class, sel!(mouseDragged:));
            if method.is_null() { return; }
            let orig_imp = method_getImplementation(method);
            if orig_imp.is_null() { return; }
            let orig_fn: OrigFn = std::mem::transmute(orig_imp);
            ORIG_MOUSE_DRAGGED.with(|c| *c.borrow_mut() = Some(orig_fn));

            // 1. mouseDragged: — must call beginDraggingSession from within this handler
            class_addMethod(
                view_class as *mut c_void,
                sel!(mouseDragged:),
                our_mouse_dragged as *const c_void,
                b"v@:@\0".as_ptr() as *const c_char,
            );

            // 2. Source operation mask — return 16 (Move)
            class_addMethod(
                view_class as *mut c_void,
                sel!(draggingSession:sourceOperationMaskForDraggingContext:),
                our_drag_source_mask as *const c_void,
                b"L@:@l\0".as_ptr() as *const c_char,
            );

            // 3. Session ended callback — arm64 NSPoint in float regs → {CGPoint=dd}
            class_addMethod(
                view_class as *mut c_void,
                sel!(draggingSession:endedAtPoint:operation:),
                our_drag_session_ended as *const c_void,
                b"v@:@{CGPoint=dd}L\0".as_ptr() as *const c_char,
            );

            // 4. NSDraggingSource protocol conformance
            let proto = objc_getProtocol(b"NSDraggingSource\0".as_ptr() as *const c_char);
            if !proto.is_null() {
                class_addProtocol(view_class as *mut c_void, proto);
                class_conformsToProtocol(view_class, proto);
            }
        }
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
                start_drag(view, this as *const c_void, ns_event, &paths);
            }
            // Do NOT forward to winit — the OS drag loop owns it from here.
            return;
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
    ) {
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
        let session = begin_drag_session_raw(
            view as *const NSView as *const c_void,
            sel!(beginDraggingSessionWithItems:event:source:),
            &*items_array as *const NSArray<NSDraggingItem> as *const c_void,
            event as *const NSEvent as *const c_void,
            source,
        );

        if session.is_null() {
            eprintln!("[drag] ERROR: beginDraggingSession returned nil");
        } else {
            eprintln!("[drag] OK: session={session:?}");
            DRAG_ACTIVE.with(|c| *c.borrow_mut() = true);
            ACTIVE_DRAG_PATHS.with(|c| *c.borrow_mut() = paths.to_vec());
        }
    }
}
