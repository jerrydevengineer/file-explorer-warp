use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeginExternalDragOutcome {
    Armed,
    AlreadyArmed,
    AlreadyActive,
    RecoveredStalePending,
    RecoveredStaleActive,
}

#[derive(Debug, PartialEq, Eq)]
pub enum BeginExternalDragError {
    #[cfg(not(target_os = "macos"))]
    UnsupportedPlatform,
    EmptyRequest,
    MissingPaths,
    HookInstall(String),
}

impl std::fmt::Display for BeginExternalDragError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(not(target_os = "macos"))]
            Self::UnsupportedPlatform => {
                write!(formatter, "native external drag is only available on macOS")
            }
            Self::EmptyRequest => write!(formatter, "cannot start an empty drag"),
            Self::MissingPaths => write!(formatter, "none of the dragged paths still exist"),
            Self::HookInstall(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for BeginExternalDragError {}

#[cfg(not(target_os = "macos"))]
pub fn begin_external_drag(
    _paths: &[&Path],
    _gesture_id: u64,
) -> Result<BeginExternalDragOutcome, BeginExternalDragError> {
    Err(BeginExternalDragError::UnsupportedPlatform)
}

#[cfg(not(target_os = "macos"))]
pub fn take_drag_ended_op() -> Option<(usize, Vec<std::path::PathBuf>)> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn cancel_pending_external_drag() {}

#[cfg(target_os = "macos")]
pub fn begin_external_drag(
    paths: &[&Path],
    gesture_id: u64,
) -> Result<BeginExternalDragOutcome, BeginExternalDragError> {
    macos_drag::begin(paths, gesture_id)
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
    use std::collections::VecDeque;
    use std::ffi::{c_char, c_void};
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    use super::{BeginExternalDragError, BeginExternalDragOutcome};
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

    #[derive(Clone, Debug)]
    struct DragRequest {
        gesture_id: u64,
        paths: Vec<PathBuf>,
        entered_at: Instant,
    }

    #[derive(Debug)]
    enum NativeDragState {
        Idle,
        Pending(DragRequest),
        Starting(DragRequest),
        Active {
            request: DragRequest,
            session_id: usize,
        },
    }

    #[derive(Debug)]
    struct EndedDrag {
        operation: usize,
        paths: Vec<PathBuf>,
    }

    #[derive(Debug)]
    struct NativeDragCoordinator {
        state: NativeDragState,
        ended: VecDeque<EndedDrag>,
    }

    impl Default for NativeDragCoordinator {
        fn default() -> Self {
            Self {
                state: NativeDragState::Idle,
                ended: VecDeque::new(),
            }
        }
    }

    impl NativeDragCoordinator {
        fn arm(
            &mut self,
            gesture_id: u64,
            paths: Vec<PathBuf>,
            now: Instant,
        ) -> BeginExternalDragOutcome {
            let request = DragRequest {
                gesture_id,
                paths,
                entered_at: now,
            };
            let previous = std::mem::replace(&mut self.state, NativeDragState::Idle);
            match previous {
                NativeDragState::Idle => {
                    self.state = NativeDragState::Pending(request);
                    BeginExternalDragOutcome::Armed
                }
                NativeDragState::Pending(old) if old.gesture_id == gesture_id => {
                    self.state = NativeDragState::Pending(old);
                    BeginExternalDragOutcome::AlreadyArmed
                }
                NativeDragState::Pending(old) => {
                    log_recovery("pending", &old, now);
                    self.state = NativeDragState::Pending(request);
                    BeginExternalDragOutcome::RecoveredStalePending
                }
                NativeDragState::Starting(old) if old.gesture_id == gesture_id => {
                    self.state = NativeDragState::Starting(old);
                    BeginExternalDragOutcome::AlreadyActive
                }
                NativeDragState::Starting(old) => {
                    // Native start is synchronous and may be re-entrant. Do not replace a
                    // Starting request because AppKit may already be creating its session.
                    self.state = NativeDragState::Starting(old);
                    BeginExternalDragOutcome::AlreadyActive
                }
                NativeDragState::Active {
                    request: old,
                    session_id,
                } if old.gesture_id == gesture_id => {
                    self.state = NativeDragState::Active {
                        request: old,
                        session_id,
                    };
                    BeginExternalDragOutcome::AlreadyActive
                }
                NativeDragState::Active { request: old, .. } => {
                    log_recovery("active", &old, now);
                    self.ended.push_back(EndedDrag {
                        operation: NSDragOperation::None.bits(),
                        paths: old.paths,
                    });
                    self.state = NativeDragState::Pending(request);
                    BeginExternalDragOutcome::RecoveredStaleActive
                }
            }
        }

        fn take_pending_for_start(&mut self) -> Option<DragRequest> {
            let previous = std::mem::replace(&mut self.state, NativeDragState::Idle);
            match previous {
                NativeDragState::Pending(request) => {
                    self.state = NativeDragState::Starting(request.clone());
                    Some(request)
                }
                state => {
                    self.state = state;
                    None
                }
            }
        }

        fn mark_active(&mut self, gesture_id: u64, session_id: usize, now: Instant) -> bool {
            let previous = std::mem::replace(&mut self.state, NativeDragState::Idle);
            match previous {
                NativeDragState::Starting(mut request) if request.gesture_id == gesture_id => {
                    request.entered_at = now;
                    self.state = NativeDragState::Active {
                        request,
                        session_id,
                    };
                    true
                }
                state => {
                    self.state = state;
                    false
                }
            }
        }

        fn fail_start(&mut self, gesture_id: u64) {
            let previous = std::mem::replace(&mut self.state, NativeDragState::Idle);
            match previous {
                NativeDragState::Starting(request) if request.gesture_id == gesture_id => {
                    self.ended.push_back(EndedDrag {
                        operation: NSDragOperation::None.bits(),
                        paths: request.paths,
                    });
                }
                state => self.state = state,
            }
        }

        fn finish_session(&mut self, session_id: usize, operation: usize) -> bool {
            let previous = std::mem::replace(&mut self.state, NativeDragState::Idle);
            match previous {
                NativeDragState::Active {
                    request,
                    session_id: active_session,
                } if active_session == session_id => {
                    self.ended.push_back(EndedDrag {
                        operation,
                        paths: request.paths,
                    });
                    true
                }
                state => {
                    self.state = state;
                    false
                }
            }
        }

        fn cancel_pending(&mut self) -> bool {
            let previous = std::mem::replace(&mut self.state, NativeDragState::Idle);
            match previous {
                NativeDragState::Pending(_) => true,
                state => {
                    self.state = state;
                    false
                }
            }
        }

        fn is_active(&self) -> bool {
            matches!(
                self.state,
                NativeDragState::Starting(_) | NativeDragState::Active { .. }
            )
        }

        fn take_ended(&mut self) -> Option<EndedDrag> {
            self.ended.pop_front()
        }
    }

    fn log_recovery(state: &str, request: &DragRequest, now: Instant) {
        let age = now.saturating_duration_since(request.entered_at);
        eprintln!(
            "[drag] gesture={} recovered-stale-{state} age_ms={} items={}",
            request.gesture_id,
            age.as_millis(),
            request.paths.len(),
        );
    }

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
        static DRAG_COORDINATOR: RefCell<NativeDragCoordinator> = RefCell::new(NativeDragCoordinator::default());
        static HOOK_INSTALLED: RefCell<bool> = RefCell::new(false);
        static ORIG_MOUSE_DRAGGED: RefCell<Option<OrigFn>> = RefCell::new(None);
    }

    pub fn take_ended_op() -> Option<(usize, Vec<PathBuf>)> {
        DRAG_COORDINATOR.with(|coordinator| {
            coordinator
                .borrow_mut()
                .take_ended()
                .map(|ended| (ended.operation, ended.paths))
        })
    }

    pub fn is_active() -> bool {
        DRAG_COORDINATOR.with(|coordinator| coordinator.borrow().is_active())
    }

    pub fn cancel_pending() {
        if DRAG_COORDINATOR.with(|coordinator| coordinator.borrow_mut().cancel_pending()) {
            eprintln!("[drag] cancelled pending request on pointer release");
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
        session: *mut AnyObject,
        _point_x: f64,
        _point_y: f64,
        operation: usize,
    ) {
        let session_id = session as usize;
        let finished = DRAG_COORDINATOR.with(|coordinator| {
            coordinator
                .borrow_mut()
                .finish_session(session_id, operation)
        });
        if finished {
            eprintln!("[drag] native-end session={session_id:#x} operation={operation}");
        } else {
            eprintln!(
                "[drag] ignored delayed native-end session={session_id:#x} operation={operation}"
            );
        }
    }

    pub fn begin(
        paths: &[&Path],
        gesture_id: u64,
    ) -> Result<BeginExternalDragOutcome, BeginExternalDragError> {
        if paths.is_empty() {
            return Err(BeginExternalDragError::EmptyRequest);
        }
        let path_bufs: Vec<PathBuf> = paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| path.to_path_buf())
            .collect();
        if path_bufs.is_empty() {
            return Err(BeginExternalDragError::MissingPaths);
        }
        ensure_hooked().map_err(BeginExternalDragError::HookInstall)?;
        let item_count = path_bufs.len();
        let outcome = DRAG_COORDINATOR.with(|coordinator| {
            coordinator
                .borrow_mut()
                .arm(gesture_id, path_bufs, Instant::now())
        });
        eprintln!("[drag] gesture={gesture_id} begin outcome={outcome:?} items={item_count}");
        Ok(outcome)
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
        // Move Pending to Starting before forwarding to winit. A new request armed by the
        // forwarded event remains Pending for the next native mouseDragged callback.
        let request =
            DRAG_COORDINATOR.with(|coordinator| coordinator.borrow_mut().take_pending_for_start());
        if let Some(request) = request {
            if !this.is_null() && !event.is_null() {
                let view = &*(this as *const NSView);
                let ns_event = &*(event as *const NSEvent);
                if start_drag(view, this as *const c_void, ns_event, &request) {
                    // Do NOT forward to winit — the OS drag loop owns it from here.
                    return;
                }
            }
            DRAG_COORDINATOR
                .with(|coordinator| coordinator.borrow_mut().fail_start(request.gesture_id));
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
        request: &DragRequest,
    ) -> bool {
        if request.paths.is_empty() {
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

        for path in &request.paths {
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
            false
        } else {
            let session_id = session as usize;
            let active = DRAG_COORDINATOR.with(|coordinator| {
                coordinator
                    .borrow_mut()
                    .mark_active(request.gesture_id, session_id, Instant::now())
            });
            if active {
                eprintln!(
                    "[drag] gesture={} native-start session={session_id:#x} items={}",
                    request.gesture_id,
                    request.paths.len(),
                );
                true
            } else {
                eprintln!(
                    "[drag] gesture={} native-start lost coordinator ownership",
                    request.gesture_id
                );
                // AppKit returned a real session. Do not forward the initiating event or
                // attempt another native start even if coordinator ownership changed.
                true
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{NativeDragCoordinator, NativeDragState};
        use crate::platform::drag::BeginExternalDragOutcome;
        use objc2_app_kit::NSDragOperation;
        use std::path::PathBuf;
        use std::time::{Duration, Instant};

        fn paths(name: &str) -> Vec<PathBuf> {
            vec![PathBuf::from(format!("/tmp/{name}"))]
        }

        #[test]
        fn idle_arms_once_and_same_gesture_is_idempotent() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();

            assert_eq!(
                coordinator.arm(1, paths("one"), now),
                BeginExternalDragOutcome::Armed,
            );
            assert_eq!(
                coordinator.arm(1, paths("one"), now + Duration::from_millis(1)),
                BeginExternalDragOutcome::AlreadyArmed,
            );
            assert!(matches!(coordinator.state, NativeDragState::Pending(_)));
        }

        #[test]
        fn pending_transitions_to_active_and_ends_once() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();
            coordinator.arm(7, paths("seven"), now);
            let request = coordinator.take_pending_for_start().unwrap();

            assert_eq!(request.gesture_id, 7);
            assert!(matches!(coordinator.state, NativeDragState::Starting(_)));
            assert!(coordinator.mark_active(7, 0x700, now));
            assert!(coordinator.is_active());
            assert!(coordinator.finish_session(0x700, NSDragOperation::Copy.bits()));
            assert!(!coordinator.is_active());
            assert!(!coordinator.finish_session(0x700, NSDragOperation::Copy.bits()));

            let ended = coordinator.take_ended().unwrap();
            assert_eq!(ended.operation, NSDragOperation::Copy.bits());
            assert_eq!(ended.paths, paths("seven"));
            assert!(coordinator.take_ended().is_none());
        }

        #[test]
        fn pointer_release_cancels_only_pending_state() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();
            coordinator.arm(2, paths("pending"), now);

            assert!(coordinator.cancel_pending());
            assert!(matches!(coordinator.state, NativeDragState::Idle));
            assert!(!coordinator.cancel_pending());

            coordinator.arm(3, paths("active"), now);
            coordinator.take_pending_for_start().unwrap();
            coordinator.mark_active(3, 0x300, now);
            assert!(!coordinator.cancel_pending());
            assert!(coordinator.is_active());
        }

        #[test]
        fn new_gesture_replaces_stale_pending() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();
            coordinator.arm(10, paths("old"), now);

            assert_eq!(
                coordinator.arm(11, paths("new"), now + Duration::from_secs(1)),
                BeginExternalDragOutcome::RecoveredStalePending,
            );
            let request = coordinator.take_pending_for_start().unwrap();
            assert_eq!(request.gesture_id, 11);
            assert_eq!(request.paths, paths("new"));
        }

        #[test]
        fn new_gesture_recovers_stale_active_and_ignores_its_delayed_callback() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();
            coordinator.arm(20, paths("old-active"), now);
            coordinator.take_pending_for_start().unwrap();
            coordinator.mark_active(20, 0x200, now);

            assert_eq!(
                coordinator.arm(21, paths("new"), now + Duration::from_secs(1)),
                BeginExternalDragOutcome::RecoveredStaleActive,
            );
            let request = coordinator.take_pending_for_start().unwrap();
            assert_eq!(request.gesture_id, 21);
            assert_eq!(request.paths, paths("new"));

            // A delayed callback from the recovered session must not end the new session,
            // including in the small window before AppKit returns the new session pointer.
            assert!(!coordinator.finish_session(0x200, NSDragOperation::Copy.bits()));
            assert!(matches!(coordinator.state, NativeDragState::Starting(_)));
            assert!(coordinator.mark_active(21, 0x210, now));

            let recovered = coordinator.take_ended().unwrap();
            assert_eq!(recovered.operation, NSDragOperation::None.bits());
            assert_eq!(recovered.paths, paths("old-active"));
        }

        #[test]
        fn different_gesture_does_not_replace_a_synchronous_native_start() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();
            coordinator.arm(30, paths("starting"), now);
            coordinator.take_pending_for_start().unwrap();

            assert_eq!(
                coordinator.arm(31, paths("new"), now),
                BeginExternalDragOutcome::AlreadyActive,
            );
            assert!(matches!(coordinator.state, NativeDragState::Starting(_)));
        }

        #[test]
        fn failed_native_start_returns_to_idle_with_cancelled_result() {
            let mut coordinator = NativeDragCoordinator::default();
            let now = Instant::now();
            coordinator.arm(40, paths("failed"), now);
            coordinator.take_pending_for_start().unwrap();

            coordinator.fail_start(40);

            assert!(matches!(coordinator.state, NativeDragState::Idle));
            let ended = coordinator.take_ended().unwrap();
            assert_eq!(ended.operation, NSDragOperation::None.bits());
            assert_eq!(ended.paths, paths("failed"));
        }
    }
}
