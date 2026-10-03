//! Floating menus.
//!
//! A dropdown hangs under a trigger. A context menu opens at a point.
//! Both paint in a deferred layer so a scrolling parent does not clip them.
//! Items are ordinary rows: the caller arms on mouse down and runs the action
//! on mouse up, the same as every other control.

use std::path::PathBuf;
use std::process::Command;

use gpui::{
    Bounds, Deferred, ElementId, Pixels, Point, Stateful, anchored, deferred, div, point,
    prelude::*, px, rgba,
};

use crate::ui::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Accent,
    Active,
    Danger,
    Disabled,
}

/// Glass panel. Scrolls once the list is taller than 240px.
pub fn panel(id: impl Into<ElementId>) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .flex()
        .flex_col()
        .min_w(px(168.))
        .max_w(px(320.))
        .max_h(px(240.))
        .overflow_y_scroll()
        .p(px(4.))
        .rounded(px(6.))
        .bg(theme::bg_glass())
        .border_1()
        .border_color(theme::border())
        .shadow_md()
}

/// Full-window layer behind an open menu. The caller closes on mouse down.
pub fn backdrop(id: impl Into<ElementId>) -> Stateful<gpui::Div> {
    div().id(id).absolute().size_full().occlude()
}

/// Item chrome. The caller adds the label and the mouse-up action.
pub fn row(button: Stateful<gpui::Div>, tone: Tone) -> Stateful<gpui::Div> {
    let disabled = tone == Tone::Disabled;
    let danger = tone == Tone::Danger;
    let accent = matches!(tone, Tone::Accent | Tone::Active);
    button
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .px(px(10.))
        .py(px(7.))
        .rounded(px(4.))
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(disabled, |row| row.opacity(0.4).cursor_default())
        .when(!disabled, |row| row.cursor_pointer())
        .text_color(match tone {
            Tone::Danger => danger_text(),
            Tone::Accent | Tone::Active => theme::accent(),
            Tone::Disabled => theme::text_muted(),
            Tone::Normal => theme::text(),
        })
        .when(tone == Tone::Active, |row| row.bg(theme::accent_soft()))
        .hover(move |style| {
            if disabled {
                style
            } else if danger {
                style.bg(danger_bg()).text_color(danger_hot())
            } else if accent {
                style.bg(theme::accent_soft())
            } else {
                style.bg(rgba(0xffffff12))
            }
        })
}

/// Opens `child` with its top-left at `origin`, then keeps it inside the window.
pub fn at(origin: Point<Pixels>, child: impl IntoElement) -> Deferred {
    place(origin, gpui::Anchor::TopLeft, child)
}

/// Opens `child` just under `anchor`. `align_end` pins the menu's right edge
/// to the trigger's right edge.
pub fn under(anchor: Bounds<Pixels>, align_end: bool, child: impl IntoElement) -> Deferred {
    let origin = point(
        if align_end {
            anchor.right()
        } else {
            anchor.left()
        },
        anchor.bottom() + px(6.),
    );
    let corner = if align_end {
        gpui::Anchor::TopRight
    } else {
        gpui::Anchor::TopLeft
    };
    place(origin, corner, child)
}

fn place(origin: Point<Pixels>, corner: gpui::Anchor, child: impl IntoElement) -> Deferred {
    deferred(
        anchored()
            .snap_to_window_with_margin(px(8.))
            .anchor(corner)
            .position(origin)
            .child(child),
    )
    .with_priority(2)
}

/// Drop a `#cue:N` suffix so Explorer selects the audio file.
pub fn file_on_disk(path: &str) -> &str {
    path.split_once("#cue:")
        .map(|(file, _)| file)
        .unwrap_or(path)
}

pub fn reveal_on_disk(path: &str) -> Result<(), String> {
    let file = file_on_disk(path);
    if file.is_empty() {
        return Err("This track has no file path".into());
    }
    Command::new("explorer")
        .arg(format!("/select,{file}"))
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not show the file: {error}"))
}

/// `Ok(None)` is a cancelled dialog.
pub fn pick_image() -> Result<Option<PathBuf>, String> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.OpenFileDialog
$dialog.Filter = 'Images|*.png;*.jpg;*.jpeg;*.webp;*.gif;*.bmp'
$dialog.Multiselect = $false
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
  Write-Output $dialog.FileName
}
"#;
    let output = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-WindowStyle", "Hidden", "-Command", script])
        .output()
        .map_err(|error| format!("Could not open the file dialog: {error}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            return Err("Could not open the file dialog".into());
        }
        return Err(format!("Could not open the file dialog: {detail}"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let path = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .map(PathBuf::from);
    Ok(path)
}

fn danger_text() -> gpui::Rgba {
    gpui::rgb(0xfca5a5)
}

fn danger_hot() -> gpui::Rgba {
    gpui::rgb(0xf87171)
}

fn danger_bg() -> gpui::Rgba {
    rgba(0xf8717124)
}

#[cfg(test)]
mod tests {
    use super::file_on_disk;

    #[test]
    fn file_on_disk_strips_cue_suffix() {
        assert_eq!(
            file_on_disk(r"D:\music\album.flac#cue:3"),
            r"D:\music\album.flac"
        );
        assert_eq!(file_on_disk(r"D:\music\song.mp3"), r"D:\music\song.mp3");
    }
}
