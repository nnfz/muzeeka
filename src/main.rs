// Muzeeka — native GPUI shell.

use std::borrow::Cow;

use gpui::App;

mod ui;

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let use_ui_font = load_ui_font(cx);
        ui::bind_keys(cx);
        ui::MainWindow::open(use_ui_font, cx);
    });
}

fn load_ui_font(cx: &App) -> bool {
    let font = Cow::Borrowed(include_bytes!("ui/src/fonts/GoogleSansFlex.ttf").as_slice());
    match cx.text_system().add_fonts(vec![font]) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("Google Sans Flex was not loaded, using the system font: {error}");
            false
        }
    }
}
