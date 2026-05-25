use std::path::Path;

/// Show the macOS Share sheet for `path`.
/// Uses NSSharingServicePicker anchored to the app's key window.
#[cfg(target_os = "macos")]
pub fn show_share_sheet(path: &Path) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::ClassType;
    use objc2_app_kit::{NSApplication, NSSharingServicePicker, NSView};
    use objc2_foundation::{MainThreadMarker, NSArray, NSRectEdge, NSString, NSURL};

    let path_str = path.to_string_lossy().to_string();

    unsafe {
        // Build NSURL for the file
        let ns_path = NSString::from_str(&path_str);
        let url: Retained<NSURL> = NSURL::fileURLWithPath(&ns_path);

        // Upcast NSURL → NSObject → AnyObject for NSArray<AnyObject>
        let url_any: Retained<AnyObject> = {
            let nsobj: Retained<objc2_foundation::NSObject> = Retained::into_super(url);
            Retained::into_super(nsobj)
        };

        let items: Retained<NSArray> = NSArray::from_id_slice(&[url_any]);

        let picker = NSSharingServicePicker::initWithItems(
            NSSharingServicePicker::alloc(),
            &items,
        );

        // eframe's update() runs on the main thread
        let mtm = MainThreadMarker::new_unchecked();
        let app = NSApplication::sharedApplication(mtm);

        if let Some(window) = app.keyWindow() {
            if let Some(view) = window.contentView() {
                let bounds = NSView::bounds(&view);
                picker.showRelativeToRect_ofView_preferredEdge(
                    bounds,
                    &view,
                    NSRectEdge::MinY,
                );
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn show_share_sheet(_path: &Path) {}
