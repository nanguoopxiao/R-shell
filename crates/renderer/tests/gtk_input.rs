#![cfg(feature = "gtk")]

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use shell_core::TerminalSize;
use shell_renderer::gtk::GtkTerminalView;
use shell_terminal::TerminalBuffer;

#[test]
#[ignore = "requires a GTK display; run explicitly with --ignored --test-threads=1"]
fn ime_commits_terminal_replies_and_widget_cleanup() {
    gtk4::init().expect("a GTK display is required for this integration test");
    let view = GtkTerminalView::new(TerminalBuffer::new(TerminalSize::new(10, 4), 20));
    let area = view.widget();
    let weak_area = area.downgrade();
    let window = gtk4::Window::builder().child(&area).build();
    gtk4::prelude::WidgetExt::realize(&window);
    area.realize();
    let received = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    let input = Rc::clone(&received);
    view.set_input_handler(move |bytes| input.borrow_mut().push(bytes));

    let controllers = area.observe_controllers();
    let keyboard = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i))
        .find_map(|item| item.downcast::<gtk4::EventControllerKey>().ok())
        .expect("terminal key controller");
    let context = keyboard.im_context().expect("terminal IME context");
    context.emit_by_name::<()>("commit", &[&"中文🙂"]);
    assert_eq!(&received.borrow()[0], "中文🙂".as_bytes());
    view.feed(b"\x1b[2;3H\x1b[6n");
    assert_eq!(
        received.borrow().as_slice(),
        ["中文🙂".as_bytes(), b"\x1b[2;3R"]
    );

    // A reconnect handler can replace itself without a RefCell borrow panic.
    let reconnect_view = view.clone();
    view.set_input_handler(move |_| reconnect_view.set_input_handler(|_| {}));
    view.feed(b"\x1b[6n");
    view.clear_io_handlers();
    context.emit_by_name::<()>("commit", &[&"closed"]);
    assert_eq!(received.borrow().len(), 2);

    drop(context);
    drop(keyboard);
    drop(controllers);
    window.set_child(None::<&gtk4::Widget>);
    window.destroy();
    drop(window);
    drop(area);
    drop(view);
    assert!(
        weak_area.upgrade().is_none(),
        "terminal callbacks must not retain their widget"
    );
}
