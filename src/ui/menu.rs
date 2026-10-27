//! Floating menus.
//!
//! A dropdown hangs under a trigger. A context menu opens at a point.
//! Both paint in a deferred layer so a scrolling parent does not clip them.
//! Items are ordinary rows: the caller arms on mouse down and runs the action
//! on mouse up, the same as every other control.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gpui::{
    anchored, deferred, div, point, prelude::*, px, rgba, Bounds, Deferred, ElementId, Pixels,
    Point, Stateful,
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

/// Rows kept on screen in a context menu. The rest scroll.
pub const VISIBLE_ROWS: usize = 12;
pub const ROW_HEIGHT: f32 = 32.;
const PAD_Y: f32 = 4.;
/// `border_1` on each edge. The panel's height is a border box, and GPUI's
/// scroll extent ignores that border, so it has to be inside the cap. Twelve
/// rows then fit exactly and the thirteenth is what starts the scroll.
const BORDER: f32 = 1.;

pub fn max_height() -> f32 {
    BORDER * 2. + PAD_Y * 2. + ROW_HEIGHT * VISIBLE_ROWS as f32
}

/// Child stack plus padding, minus the panel border box. This is the extent
/// GPUI will scroll. Zero means the rows fit.
#[cfg(test)]
fn scroll_extent(rows: usize) -> f32 {
    let padded = rows as f32 * ROW_HEIGHT + PAD_Y * 2.;
    (padded - max_height()).max(0.)
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
        .bg(theme::bg_surface())
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
        .h(px(ROW_HEIGHT))
        .flex_shrink_0()
        .px(px(10.))
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
    let path = path
        .split_once("#cue:")
        .map(|(file, _)| file)
        .unwrap_or(path);
    path.strip_prefix(r"\\?\").unwrap_or(path)
}

/// `explorer` argument that selects `path`.
///
/// Quotes wrap the path only. Quoting the whole `/select,...` switch makes
/// Explorer treat it as a folder name and open Documents.
pub fn explorer_select_arg(path: &str) -> String {
    let file = file_on_disk(path).replace('/', "\\");
    format!("/select,\"{file}\"")
}

pub fn reveal_on_disk(path: &str) -> Result<(), String> {
    let file = file_on_disk(path);
    if file.is_empty() {
        return Err("This track has no file path".into());
    }
    if !Path::new(file).is_file() {
        return Err("File was not found on disk".into());
    }
    let mut command = Command::new("explorer");
    command.raw_arg(explorer_select_arg(path));
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not show the file: {error}"))
}

/// `Ok(None)` is a cancelled dialog.
pub fn pick_folder() -> Result<Option<PathBuf>, String> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = 'Download folder'
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
  Write-Output $dialog.SelectedPath
}
"#;
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .output()
        .map_err(|error| format!("Could not open the folder dialog: {error}"))?;
    if !output.status.success() && output.stdout.is_empty() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            return Err("Could not open the folder dialog".into());
        }
        return Err(format!("Could not open the folder dialog: {detail}"));
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

fn dialog_path(script: &str) -> Result<Option<PathBuf>, String> {
    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
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

/// `Ok(None)` is a cancelled dialog.
pub fn pick_file(title: &str, filter: &str) -> Result<Option<PathBuf>, String> {
    let script = format!(
        r#"
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.OpenFileDialog
$dialog.Title = '{title}'
$dialog.Filter = '{filter}'
$dialog.Multiselect = $false
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{
  Write-Output $dialog.FileName
}}
"#
    );
    dialog_path(&script)
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
        .args([
            "-NoProfile",
            "-STA",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
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
    use super::{explorer_select_arg, file_on_disk, scroll_extent};

    #[test]
    fn twelve_rows_fit_and_the_next_one_scrolls() {
        assert_eq!(scroll_extent(12), 0.);
        assert!(scroll_extent(13) >= super::ROW_HEIGHT - super::BORDER * 2.);
        assert_eq!(scroll_extent(0), 0.);
    }

    #[test]
    fn file_on_disk_strips_cue_suffix() {
        assert_eq!(
            file_on_disk(r"D:\music\album.flac#cue:3"),
            r"D:\music\album.flac"
        );
        assert_eq!(file_on_disk(r"D:\music\song.mp3"), r"D:\music\song.mp3");
        assert_eq!(
            file_on_disk(r"\\?\Z:\Music\song.mp3#cue:1"),
            r"Z:\Music\song.mp3"
        );
    }

    #[test]
    fn explorer_select_quotes_only_the_path() {
        assert_eq!(
            explorer_select_arg(r"Z:\Music\My Song.mp3"),
            "/select,\"Z:\\Music\\My Song.mp3\""
        );
        assert_eq!(
            explorer_select_arg(r"D:/music/album.flac#cue:3"),
            "/select,\"D:\\music\\album.flac\""
        );
    }
}
