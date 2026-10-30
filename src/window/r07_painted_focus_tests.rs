// SPDX-FileCopyrightText: 2026 Broadcaster contributors
// SPDX-License-Identifier: GPL-3.0-or-later

// Actual X11 framebuffer pixels, not GTK's logical :focus state. The only
// conditional styling is a scoped TEST-ONLY suppression of the selected
// widget's outline. Compare focused, suppressed, and restored images without
// changing GtkWindow::focus or selecting a different widget.
const R07_CAPTURE_X11: &str = r#"
import ctypes as c,sys
x=c.CDLL('libX11.so.6')
x.XOpenDisplay.argtypes=[c.c_char_p];x.XOpenDisplay.restype=c.c_void_p
d=x.XOpenDisplay(None);assert d, 'R07_PIXEL_DISPLAY_UNAVAILABLE'
x.XDefaultRootWindow.argtypes=[c.c_void_p];x.XDefaultRootWindow.restype=c.c_ulong
x.XQueryTree.argtypes=[c.c_void_p,c.c_ulong,c.POINTER(c.c_ulong),c.POINTER(c.c_ulong),c.POINTER(c.POINTER(c.c_ulong)),c.POINTER(c.c_uint)]
x.XQueryTree.restype=c.c_int
root=x.XDefaultRootWindow(d)
a,b,n=c.c_ulong(),c.c_ulong(),c.c_uint();ch=c.POINTER(c.c_ulong)()
assert x.XQueryTree(d,root,c.byref(a),c.byref(b),c.byref(ch),c.byref(n)) and n.value, 'R07_PIXEL_NO_WINDOW'
window=ch[n.value-1]
x.XTranslateCoordinates.argtypes=[c.c_void_p,c.c_ulong,c.c_ulong,c.c_int,c.c_int,c.POINTER(c.c_int),c.POINTER(c.c_int),c.POINTER(c.c_ulong)]
x.XTranslateCoordinates.restype=c.c_int
wx,wy,child=c.c_int(),c.c_int(),c.c_ulong()
assert x.XTranslateCoordinates(d,window,root,0,0,c.byref(wx),c.byref(wy),c.byref(child))
x.XFree.argtypes=[c.c_void_p];x.XFree(c.cast(ch,c.c_void_p))
rx,ry,w,h=map(int,sys.argv[1:]);rx+=wx.value;ry+=wy.value
assert rx>=0 and ry>=0 and w>0 and h>0, (rx,ry,w,h)
x.XSync.argtypes=[c.c_void_p,c.c_int];x.XSync(d,0)
x.XGetImage.argtypes=[c.c_void_p,c.c_ulong,c.c_int,c.c_int,c.c_uint,c.c_uint,c.c_ulong,c.c_int]
x.XGetImage.restype=c.c_void_p
# A GTK-mapped scrolled action may extend beyond the Xvfb root. Capture
# only the visible intersection; absent offscreen pixels remain zero in all
# three frames and cannot manufacture positive focus contrast.
x.XDefaultScreen.argtypes=[c.c_void_p];x.XDefaultScreen.restype=c.c_int
x.XDisplayWidth.argtypes=[c.c_void_p,c.c_int];x.XDisplayWidth.restype=c.c_int
x.XDisplayHeight.argtypes=[c.c_void_p,c.c_int];x.XDisplayHeight.restype=c.c_int
screen=x.XDefaultScreen(d)
sw,sh=x.XDisplayWidth(d,screen),x.XDisplayHeight(d,screen)
sx,sy=max(rx,0),max(ry,0)
ex,ey=min(rx+w,sw),min(ry+h,sh)
pixels=bytearray(w*h*3)
if ex>sx and ey>sy:
 im=x.XGetImage(d,root,sx,sy,ex-sx,ey-sy,0xffffffffffffffff,2)
 assert im, 'R07_PIXEL_VISIBLE_XGETIMAGE_FAILED'
 x.XGetPixel.argtypes=[c.c_void_p,c.c_int,c.c_int];x.XGetPixel.restype=c.c_ulong
 for yy in range(ey-sy):
  for xx in range(ex-sx):
   v=x.XGetPixel(im,xx,yy)
   i=(((yy+sy-ry)*w)+(xx+sx-rx))*3
   pixels[i:i+3]=bytes(((v>>16)&255,(v>>8)&255,v&255))
 x.XDestroyImage.argtypes=[c.c_void_p];x.XDestroyImage(im)
x.XCloseDisplay.argtypes=[c.c_void_p];x.XCloseDisplay(d)
sys.stdout.buffer.write(pixels)
"#;

#[derive(Clone, Copy)]
struct R07PaintBox {
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    // The selected real control's bounds in pixels relative to this crop.
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl R07PaintBox {
    fn new(win: &TvWindow, widget: &gtk::Widget) -> Self {
        let rect = widget
            .compute_bounds(win.upcast_ref::<gtk::Widget>())
            .expect("R07 painted control is inside window");
        let (left, top, right, bottom) = (
            rect.x().floor() as i32,
            rect.y().floor() as i32,
            (rect.x() + rect.width()).ceil() as i32,
            (rect.y() + rect.height()).ceil() as i32,
        );
        assert!(right > left + 6 && bottom > top + 6, "real painted geometry");
        let margin = 8;
        // Keep the capture in the X11 framebuffer even at compact edges.
        let x = (left - margin).max(0);
        let y = (top - margin).max(0);
        Self {
            x,
            y,
            w: (right + margin - x) as usize,
            h: (bottom + margin - y) as usize,
            left: left - x,
            top: top - y,
            right: right - x,
            bottom: bottom - y,
        }
    }

    fn capture(&self) -> Vec<u8> {
        let output = std::process::Command::new("python3")
            .args([
                "-c",
                R07_CAPTURE_X11,
                &self.x.to_string(),
                &self.y.to_string(),
                &self.w.to_string(),
                &self.h.to_string(),
            ])
            .output()
            .expect("R07 real X11 screenshot process");
        assert!(
            output.status.success(),
            "R07 screenshot failed (not a focus oracle): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout.len(), self.w * self.h * 3, "X11 RGB screenshot size");
        output.stdout
    }

    fn outline_differences(&self, a: &[u8], b: &[u8]) -> usize {
        let mut count = 0;
        for y in 0..self.h {
            for x in 0..self.w {
                let x = x as i32;
                let y = y as i32;
                // Outline is three pixels at the widget edge (+1 offset),
                // including antialiasing. Exclude interior labels/carets.
                if x < self.left - 6
                    || x > self.right + 6
                    || y < self.top - 6
                    || y > self.bottom + 6
                {
                    continue;
                }
                if x > self.left + 5
                    && x < self.right - 5
                    && y > self.top + 5
                    && y < self.bottom - 5
                {
                    continue;
                }
                let i = (y as usize * self.w + x as usize) * 3;
                let delta: u32 = (0..3)
                    .map(|c| a[i + c].abs_diff(b[i + c]) as u32)
                    .sum();
                if delta >= 75 {
                    count += 1;
                }
            }
        }
        count
    }
}

fn r07_painted_probe(win: &TvWindow, widget: &gtk::Widget, label: &str) -> usize {
    assert!(widget.is_mapped() && widget.is_visible() && widget.is_sensitive());
    let focused = win.focused_widget().expect("R07 painted focus owner");
    assert!(TvWindow::is_within(&focused, widget), "R07 painted {label} logical owner");
    let box_ = R07PaintBox::new(win, widget);
    settle();
    let positive = box_.capture();
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        ".r07-suppress-paint, .r07-suppress-paint:focus, .r07-suppress-paint:focus-within, .r07-suppress-paint:focus-visible, .r07-suppress-paint * { outline: none; outline-width: 0; box-shadow: none; }",
    );
    let display = gtk::prelude::WidgetExt::display(widget);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_USER + 100,
    );
    widget.add_css_class("r07-suppress-paint");
    settle();
    assert!(TvWindow::is_within(
        &win.focused_widget().expect("negative paint retained focus"),
        widget
    ));
    let suppressed = box_.capture();
    widget.remove_css_class("r07-suppress-paint");
    gtk::style_context_remove_provider_for_display(&display, &provider);
    settle();
    let restored = box_.capture();
    let changed = box_.outline_differences(&positive, &suppressed);
    let reversible = box_.outline_differences(&positive, &restored);
    eprintln!("R07_PIXEL {label}: suppressed_changed={changed} restored_changed={reversible} crop={}x{}", box_.w, box_.h);
    // Collect all measured controls before reporting missing painted outlines.
    // No logical focus assertion alone can turn a blank ring into PASS.
    assert!(
        reversible <= (changed / 3).max(12),
        "R07 {label}: outline did not return after removing test-only suppression"
    );
    changed
}

fn r07_native_painted_focus(width: i32, height: i32, negative: bool) {
    adw::init().expect("mapped GTK display");
    let resource =
        gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES)).unwrap();
    gio::resources_register(&resource);
    // Native test fixtures register() but do not run main.rs startup: install
    // Broadcaster's application stylesheet before comparing painted pixels.
    let css = gtk::CssProvider::new();
    css.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("R07 painted GTK display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>)
        .expect("registered real application");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    let mut missing_paint = Vec::new();
    for page in ["mediathek", "live", "streams"] {
        win.imp().stack.set_visible_child_name(page);
        if page == "live" {
            win.imp().live_view.show_toolbar_for_test();
        }
        settle();
        let targets = match page {
            "mediathek" => win.imp().mediathek_view.toolbar_focus_targets(),
            "live" => win.imp().live_view.toolbar_focus_targets(),
            "streams" => win.imp().streams_view.toolbar_focus_targets(),
            _ => unreachable!(),
        };
        assert!(targets.len() >= 2, "R07 {page} Search and button targets");
        let search = &targets[0];
        assert!(search.grab_focus(), "R07 {page} Search entry");
        settle();
        inject("Right", true);
        inject("Left", false);
        let search_delta = r07_painted_probe(&win, search, &format!("{page}:Search"));
        if search_delta < 15 {
            missing_paint.push(format!("{page}:Search"));
        }
        if negative {
            assert_eq!(
                search_delta, 0,
                "R07_G01_EXPECTED_NEGATIVE_ORACLE: suppressed real Search pixels must differ"
            );
            unreachable!("R07 negative painted-focus oracle unexpectedly passed");
        }
        inject("Right", false);
        let button = targets
            .iter()
            .skip(1)
            .find(|candidate| {
                win.focused_widget()
                    .is_some_and(|focused| TvWindow::is_within(&focused, candidate))
            })
            .expect("real next toolbar control after native Right");
        let button_delta = r07_painted_probe(&win, button, &format!("{page}:button"));
        if button_delta < 15 {
            missing_paint.push(format!("{page}:button"));
        }
    }
    assert!(
        missing_paint.is_empty(),
        "R07_PAINTED_FOCUS_MISSING: {missing_paint:?}"
    );
    win.destroy();
}

#[test]
#[ignore = "R07 real X11 painted search/button outline, 640x480"]
fn r07_native_painted_focus_640_480() {
    r07_native_painted_focus(640, 480, false);
}

#[test]
#[ignore = "R07 real X11 painted search/button outline, 1280x800"]
fn r07_native_painted_focus_1280_800() {
    r07_native_painted_focus(1280, 800, false);
}

#[test]
#[ignore = "R07 deliberately wrong X11 pixel screenshot oracle"]
fn r07_native_painted_focus_negative_pixels() {
    r07_native_painted_focus(640, 480, true);
}
// R07 G03: compare real framebuffer focus paint on the actual source-card
// actions, not merely their GTK focus classes. Reuse G01's reversible
// test-only CSS suppression and bounded screenshot oracle.
fn r07_native_painted_cards(width: i32, height: i32) {
    adw::init().expect("native display");
    let resource = gio::Resource::from_data(&glib::Bytes::from_static(crate::GRESOURCE_BYTES))
        .expect("real Broadcaster resources");
    gio::resources_register(&resource);
    let css = gtk::CssProvider::new();
    css.load_from_resource("/io/github/Certain1989/Broadcaster/broadcaster.css");
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().expect("R07 painted display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let app = TvApplication::new();
    app.register(None::<&gio::Cancellable>).expect("real app register");
    let win = TvWindow::new(&app);
    win.set_default_size(width, height);
    win.present();
    settle();
    let mut deltas = Vec::new();

    win.imp().stack.set_visible_child_name("live");
    let live = &win.imp().live_view;
    live.show_toolbar_for_test();
    live.install_r07_native_channel_rows();
    settle();
    assert!(live.toolbar_focus_targets()[0].grab_focus());
    settle();
    inject("Down", true);
    let live_list = TvWindow::find_descendant(live.upcast_ref(), &|w| w.is::<gtk::ListBox>())
        .and_then(|w| w.downcast::<gtk::ListBox>().ok())
        .expect("real Live card ListBox");
    assert!(live_list.row_at_index(0).unwrap().has_focus());
    inject("Return", false);
    let play = win.focused_widget().expect("actual Live Play");
    assert_eq!(play.downcast_ref::<gtk::Button>().and_then(|b| b.action_name()).as_deref(),
        Some("card.play"));
    deltas.push(("Live Play", r07_painted_probe(&win, &play, "live:card.Play")));
    inject("Left", false);

    win.imp().stack.set_visible_child_name("mediathek");
    let med = &win.imp().mediathek_view;
    med.r07_install_offline_result_cards();
    settle();
    assert!(med.toolbar_focus_targets()[0].grab_focus());
    settle();
    for _ in 0..10 {
        if med.r07_result_row(0).has_focus() { break; }
        inject("Down", false);
    }
    assert!(med.r07_result_row(0).has_focus());
    inject("Return", false);
    let row = med.r07_result_row(0);
    let targets = [
        ("Mediathek Play", TvWindow::find_descendant(row.upcast_ref(), &|w| w.is::<adw::SplitButton>()).unwrap()),
        ("Mediathek Copy", TvWindow::find_descendant(row.upcast_ref(), &|w| w.is::<gtk::MenuButton>()).unwrap()),
        ("Mediathek Download", TvWindow::find_descendant(row.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| b.action_name().as_deref() == Some("card.download"))
        }).unwrap()),
        ("Mediathek Website", TvWindow::find_descendant(row.upcast_ref(), &|w| {
            w.downcast_ref::<gtk::Button>().is_some_and(|b| b.action_name().as_deref() == Some("card.open-website"))
        }).unwrap()),
    ];
    for (label, target) in targets {
        for _ in 0..7 {
            if win.focused_widget().is_some_and(|w| TvWindow::is_within(&w, &target)) { break; }
            inject("Right", false);
        }
        let focused = win.focused_widget().expect("Mediathek action owner");
        assert!(TvWindow::is_within(&focused, &target), "G03 physically reach {label}");
        let child_pixels = r07_painted_probe(&win, &focused, label);
        // SplitButton is composite: the drawn outline may be on its
        // parent rather than on the internally focused GTK child.
        let pixels = if label == "Mediathek Play" && child_pixels < 15 {
            child_pixels.max(r07_painted_probe(&win, &target, "Mediathek Play parent"))
        } else {
            child_pixels
        };
        deltas.push((label, pixels));
    }
    inject("XF86Back", false);

    win.imp().stack.set_visible_child_name("streams");
    let streams = &win.imp().streams_view;
    let ids = streams.install_remote_action_test_rows();
    settle();
    assert!(streams.toolbar_focus_targets()[0].grab_focus());
    settle();
    for _ in 0..16 {
        if win.focused_widget().is_some_and(|w| streams.contains_results_focus(&w)) { break; }
        inject("Down", false);
    }
    assert_eq!(streams.remote_action_test_selected_id().as_deref(), Some(ids[0].as_str()));
    inject("Left", false);
    let star = streams.remote_action_test_row_star(&ids[0]).expect("mapped real source-card Star");
    assert!(star.has_focus());
    deltas.push(("My Channels Star", r07_painted_probe(&win, star.upcast_ref(), "streams:star")));
    inject("Right", false);
    inject("Return", false);
    for label in ["Play", "Edit Metadata", "Programme mapping"] {
        let focused = win.focused_widget().expect("My Channels focused native card action");
        assert_eq!(focused.downcast_ref::<gtk::Button>().and_then(|b| b.label()).as_deref(), Some(label));
        streams.assert_remote_action_focus_owner(&focused, &ids[0]);
        deltas.push((label, r07_painted_probe(&win, &focused, &format!("streams:{label}"))));
        if label != "Programme mapping" { inject("Down", false); }
    }
    assert!(deltas.iter().all(|(_, pixels)| *pixels >= 15),
        "R07_G03_MISSING_PAINTED_CARD: {deltas:?}");
    win.destroy();
}

#[test]
#[ignore = "R07 G03 physical card actions and painted pixel contrast 640x480"]
fn r07_native_painted_cards_640_480() { r07_native_painted_cards(640, 480); }

#[test]
#[ignore = "R07 G03 physical card actions and painted pixel contrast 1280x800"]
fn r07_native_painted_cards_1280_800() { r07_native_painted_cards(1280, 800); }
