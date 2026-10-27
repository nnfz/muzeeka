#[path = "../../sdk/muzeeka_plugin.rs"]
mod muzeeka_plugin;

mod files;
mod realm;
mod songs;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, UNIX_EPOCH};

use files::{
    choose_extension, choose_image_extension, find_data_dir, hash_path, plugin_data_dir, read_head,
};
use muzeeka_plugin::{MuzeekaHost, MUZEEKA_PLUGIN_ABI};
use songs::{survey, Song};

const PLAYLIST: &str = "osu!lazer";
/// Playlist name from before the plugin was renamed. Adopted once.
const LEGACY_PLAYLIST: &str = "osu!lazer";
const BATCH: usize = 8;

static STOP: AtomicBool = AtomicBool::new(false);
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

#[no_mangle]
pub extern "C" fn muzeeka_plugin_abi() -> u32 {
    MUZEEKA_PLUGIN_ABI
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_start(host: *const MuzeekaHost) -> c_int {
    if host.is_null() {
        return 1;
    }
    let host = unsafe { *host };
    STOP.store(false, Ordering::SeqCst);
    let data_dir = read_data_dir(&host);
    match thread::Builder::new()
        .name("osu-lazer".into())
        .spawn(move || run(host, data_dir))
    {
        Ok(handle) => {
            *WORKER.lock().unwrap_or_else(|err| err.into_inner()) = Some(handle);
            0
        }
        Err(err) => {
            log(&host, true, &format!("не удалось запустить поток: {err}"));
            1
        }
    }
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    STOP.store(true, Ordering::SeqCst);
    let handle = WORKER.lock().unwrap_or_else(|err| err.into_inner()).take();
    if let Some(handle) = handle {
        let _ = handle.join();
    }
}

fn run(host: MuzeekaHost, data_dir_setting: String) {
    let mut last_stamp: Option<(u128, u64)> = None;
    let mut last_error = String::new();
    let mut announced_dir = false;
    // One try per process. A score rewriting client.realm must not import again.
    let mut tried_covers = HashSet::new();
    // Avoid rereading the realm every few seconds when osu! is gone and the files are too.
    let mut attempted_while_empty = false;
    while !STOP.load(Ordering::SeqCst) {
        match sync_once(
            &host,
            &data_dir_setting,
            &mut last_stamp,
            &mut announced_dir,
            &mut tried_covers,
            &mut attempted_while_empty,
        ) {
            Ok(()) => last_error.clear(),
            Err(err) => {
                if err != last_error {
                    log(&host, true, &err);
                    last_error = err;
                }
            }
        }
        for _ in 0..40 {
            if STOP.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
}

fn sync_once(
    host: &MuzeekaHost,
    data_dir_setting: &str,
    last_stamp: &mut Option<(u128, u64)>,
    announced_dir: &mut bool,
    tried_covers: &mut HashSet<String>,
    attempted_while_empty: &mut bool,
) -> Result<(), String> {
    let data_dir = find_data_dir(data_dir_setting)?;
    if !*announced_dir {
        log(
            host,
            false,
            &format!("папка osu!lazer: {}", data_dir.display()),
        );
        *announced_dir = true;
    }
    let realm_path = data_dir.join("client.realm");
    let stamp = file_stamp(&realm_path)
        .ok_or_else(|| format!("не удалось прочитать {}", realm_path.display()))?;
    let plugin_dir = plugin_data_dir()?;
    let mut saved = load_state(&plugin_dir);
    adopt_playlist_name(host, &mut saved, &plugin_dir)?;
    let empty = playlist_is_empty(host)?;
    if empty {
        if !saved.imported.is_empty() && !*attempted_while_empty {
            saved.refill = true;
            *attempted_while_empty = true;
        }
    } else {
        *attempted_while_empty = false;
    }
    if *last_stamp == Some(stamp) && !saved.refill {
        return Ok(());
    }
    let bytes = read_realm(&realm_path)?;
    let found = survey(&bytes)?;
    if found.songs.is_empty() {
        *last_stamp = Some(stamp);
        log(
            host,
            false,
            &format!(
                "в картах нет музыки (карты {}, сеты {})",
                found.beatmaps, found.sets
            ),
        );
        return Ok(());
    }
    let refill = saved.refill;
    let (prepared, noted_empty) = prepare_audio(
        &data_dir,
        &plugin_dir,
        &found.songs,
        &saved.imported,
        &mut saved.covers,
        tried_covers,
        refill,
    )?;
    if STOP.load(Ordering::SeqCst) {
        return Ok(());
    }
    if prepared.is_empty() {
        saved.refill = false;
        if noted_empty || refill {
            save_state(&plugin_dir, &saved)?;
        }
        *last_stamp = Some(stamp);
        return Ok(());
    }
    let fresh_total = prepared.iter().filter(|song| song.fresh).count();
    let cover_total = prepared.iter().filter(|song| song.cover.is_some()).count();
    if refill && fresh_total > 0 {
        log(
            host,
            false,
            &format!("плейлист {PLAYLIST} удалён, собираю его снова ({fresh_total})"),
        );
    } else if fresh_total > 0 {
        log(
            host,
            false,
            &format!(
                "в картах {} песен, добавляю {fresh_total} новых в плейлист {PLAYLIST}",
                found.songs.len()
            ),
        );
    }
    let mut added = 0usize;
    let mut covers_set = 0usize;
    for batch in prepared.chunks(BATCH) {
        if STOP.load(Ordering::SeqCst) {
            save_state(&plugin_dir, &saved)?;
            return Ok(());
        }
        let covered = import_paths(host, batch)?;
        for song in batch {
            if song.fresh {
                if let Some(legacy) = &song.legacy {
                    delete_tracks(host, &[legacy.display().to_string()])?;
                    let _ = fs::remove_file(legacy);
                }
                saved
                    .imported
                    .insert(song.hash.clone(), song.path.display().to_string());
                added += 1;
            }
            if song.cover.is_some() {
                if covered.contains(&path_key(&song.path.display().to_string())) {
                    saved
                        .covers
                        .insert(song.hash.clone(), song.background_hash.clone());
                    covers_set += 1;
                } else {
                    tried_covers.insert(song.hash.clone());
                }
            }
        }
        save_state(&plugin_dir, &saved)?;
        if fresh_total > BATCH && batch.iter().any(|song| song.fresh) {
            log(
                host,
                false,
                &format!("в плейлист {PLAYLIST} добавлено {added} из {fresh_total}"),
            );
        }
    }
    saved.refill = false;
    save_state(&plugin_dir, &saved)?;
    *last_stamp = Some(stamp);
    if added > 0 {
        log(
            host,
            false,
            &format!("плейлист {PLAYLIST}: добавлено {added}"),
        );
    }
    if covers_set > 0 {
        log(host, false, &format!("фон карты у {covers_set} песен"));
    } else if cover_total > 0 {
        log(
            host,
            false,
            "фон карты не записался. Закройте Muzeeka и откройте снова.",
        );
    }
    Ok(())
}

struct Prepared {
    hash: String,
    path: PathBuf,
    artist: String,
    title: String,
    /// A hardlink or copy made by an older build of this plugin.
    legacy: Option<PathBuf>,
    /// Beatmap background. Sent only when this song still needs a cover.
    cover: Option<PathBuf>,
    background_hash: String,
    /// False when the original file is already in the playlist and only the cover is new.
    fresh: bool,
}

fn playlist_is_empty(host: &MuzeekaHost) -> Result<bool, String> {
    if STOP.load(Ordering::SeqCst) {
        return Ok(false);
    }
    let value = ensure_playlist(host, PLAYLIST)?;
    let count = value
        .get("trackCount")
        .and_then(|item| item.as_u64())
        .unwrap_or(0);
    Ok(count == 0)
}

/// The first builds called the playlist "osu!". Rename that row once so a new
/// empty "osu!lazer" is not created beside it.
fn adopt_playlist_name(
    host: &MuzeekaHost,
    saved: &mut Saved,
    plugin_dir: &Path,
) -> Result<(), String> {
    if saved.renamed || STOP.load(Ordering::SeqCst) {
        return Ok(());
    }
    let legacy = ensure_playlist(host, LEGACY_PLAYLIST)?;
    let id = legacy
        .get("id")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim();
    if id.is_empty() {
        return Err("плейлист osu!lazer не создался".into());
    }
    let name = legacy
        .get("name")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    if !name.eq_ignore_ascii_case(PLAYLIST) {
        let payload = serde_json::json!({ "id": id, "name": PLAYLIST }).to_string();
        host.call("library.renamePlaylist", &payload)?;
    }
    saved.renamed = true;
    save_state(plugin_dir, saved)
}

fn ensure_playlist(host: &MuzeekaHost, name: &str) -> Result<serde_json::Value, String> {
    let payload = serde_json::json!({ "name": name }).to_string();
    host.call("library.ensurePlaylist", &payload)
}

fn prepare_audio(
    osu_dir: &Path,
    plugin_dir: &Path,
    songs: &[Song],
    imported: &HashMap<String, String>,
    covers: &mut HashMap<String, String>,
    tried_covers: &HashSet<String>,
    refill: bool,
) -> Result<(Vec<Prepared>, bool), String> {
    let audio_dir = plugin_dir.join("audio");
    let mut prepared = Vec::new();
    let mut missing = 0usize;
    let mut noted_empty = false;
    let files_dir = osu_dir.join("files");
    for song in songs {
        if STOP.load(Ordering::SeqCst) {
            break;
        }
        let src = hash_path(&files_dir, &song.hash);
        if !src.is_file() {
            missing += 1;
            continue;
        }
        if choose_extension(&song.audio_filename, &read_head(&src)).is_none() {
            missing += 1;
            continue;
        }
        if song.background_hash.is_empty() && !covers.contains_key(&song.hash) {
            covers.insert(song.hash.clone(), String::new());
            noted_empty = true;
        }
        let saved = imported.get(&song.hash).map(String::as_str);
        let legacy = saved.and_then(|saved| legacy_copy(&audio_dir, saved, &src));
        let already =
            !refill && saved.is_some_and(|saved| same_path(saved, &src)) && legacy.is_none();
        let background = background_path(&files_dir, song);
        let cover_known = covers
            .get(&song.hash)
            .is_some_and(|stored| stored == &song.background_hash);
        let need_cover = background.is_some() && !cover_known && !tried_covers.contains(&song.hash);
        if already && !need_cover {
            continue;
        }
        prepared.push(Prepared {
            hash: song.hash.clone(),
            path: src,
            artist: song.artist.clone(),
            title: song.title.clone(),
            legacy,
            cover: if need_cover { background } else { None },
            background_hash: song.background_hash.clone(),
            fresh: !already,
        });
    }
    if missing > 0 && prepared.is_empty() && imported.is_empty() {
        return Err(format!(
            "нашёл {missing} песен в базе, но файлов в files/ нет"
        ));
    }
    Ok((prepared, noted_empty))
}

fn background_path(files_dir: &Path, song: &Song) -> Option<PathBuf> {
    if song.background_hash.len() != 64 {
        return None;
    }
    let path = hash_path(files_dir, &song.background_hash);
    if !path.is_file() {
        return None;
    }
    choose_image_extension(&song.background_filename, &read_head(&path))?;
    Some(path)
}

/// Older builds stored a file name under plugin-data/audio. Those links are removed
/// after the original osu file is in the playlist.
fn legacy_copy(audio_dir: &Path, saved: &str, src: &Path) -> Option<PathBuf> {
    if same_path(saved, src) {
        return None;
    }
    let path = if Path::new(saved).is_absolute() {
        PathBuf::from(saved)
    } else {
        audio_dir.join(saved)
    };
    if path.is_file() && is_inside(audio_dir, &path) {
        Some(path)
    } else {
        None
    }
}

fn same_path(saved: &str, src: &Path) -> bool {
    path_key(saved) == path_key(&src.display().to_string())
}

fn is_inside(dir: &Path, file: &Path) -> bool {
    let dir = path_key(&dir.display().to_string());
    let file = path_key(&file.display().to_string());
    file.starts_with(&dir)
        && file
            .get(dir.len()..)
            .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with('/'))
}

fn path_key(text: &str) -> String {
    let text = text.trim().trim_start_matches(r"\\?\");
    text.replace('/', "\\").to_ascii_lowercase()
}

/// Path keys whose returned track already has a cover.
fn import_paths(host: &MuzeekaHost, songs: &[Prepared]) -> Result<HashSet<String>, String> {
    if songs.is_empty() || STOP.load(Ordering::SeqCst) {
        return Ok(HashSet::new());
    }
    let paths: Vec<String> = songs
        .iter()
        .map(|song| song.path.display().to_string())
        .collect();
    let labels: Vec<serde_json::Value> = songs
        .iter()
        .map(|song| {
            serde_json::json!({
                "path": song.path.display().to_string(),
                "title": song.title,
                "artist": song.artist,
                "cover": song.cover.as_ref().map(|path| path.display().to_string()).unwrap_or_default(),
            })
        })
        .collect();
    let payload = serde_json::json!({
        "playlist": PLAYLIST,
        "paths": paths,
        "labels": labels,
        "clear": false,
        "open": false,
    });
    let value = host.call("library.import", &payload.to_string())?;
    let message = value
        .get("message")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    if message == "No supported audio files found" {
        return Err(
            "плеер пока не берёт файлы osu без расширения. Закройте Muzeeka и откройте снова."
                .into(),
        );
    }
    Ok(covered_keys(&value))
}

fn covered_keys(value: &serde_json::Value) -> HashSet<String> {
    let Some(tracks) = value
        .get("playlist")
        .and_then(|item| item.get("tracks"))
        .and_then(|item| item.as_array())
    else {
        return HashSet::new();
    };
    tracks
        .iter()
        .filter_map(|track| {
            let cover = track
                .get("coverUrl")
                .and_then(|item| item.as_str())
                .unwrap_or("");
            if cover.trim().is_empty() {
                return None;
            }
            let path = track.get("path").and_then(|item| item.as_str())?;
            Some(path_key(path))
        })
        .collect()
}

fn delete_tracks(host: &MuzeekaHost, paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let payload = serde_json::json!({ "paths": paths });
    host.call("library.deleteTracks", &payload.to_string())?;
    Ok(())
}

fn read_data_dir(host: &MuzeekaHost) -> String {
    let Ok(value) = host.call("settings.get", "{}") else {
        return String::new();
    };
    value
        .get("data_dir")
        .and_then(|item| item.as_str())
        .unwrap_or("")
        .trim()
        .trim_matches('"')
        .trim()
        .to_string()
}

fn read_realm(path: &Path) -> Result<Vec<u8>, String> {
    let mut file =
        fs::File::open(path).map_err(|err| format!("не открыть {}: {err}", path.display()))?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if len < 24 || len > 512 * 1024 * 1024 {
        return Err(format!("странный размер client.realm ({len} байт)"));
    }
    let mut bytes = vec![0u8; len as usize];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("не прочитать {}: {err}", path.display()))?;
    let mut check =
        fs::File::open(path).map_err(|err| format!("не открыть {}: {err}", path.display()))?;
    let mut header = [0u8; 16];
    check
        .read_exact(&mut header)
        .map_err(|err| format!("не прочитать заголовок realm: {err}"))?;
    if header != bytes[..16] {
        return Err("client.realm изменился во время чтения".into());
    }
    Ok(bytes)
}

fn file_stamp(path: &Path) -> Option<(u128, u64)> {
    let meta = fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some((modified, meta.len()))
}

struct Saved {
    imported: HashMap<String, String>,
    /// Audio hash → background hash. Empty string means the map has no background.
    covers: HashMap<String, String>,
    /// The osu!lazer playlist was deleted. Put every known song back.
    refill: bool,
    /// The old playlist name "osu!" was already renamed.
    renamed: bool,
}

fn load_state(dir: &Path) -> Saved {
    let Ok(raw) = fs::read_to_string(dir.join("imported.json")) else {
        return Saved {
            imported: HashMap::new(),
            covers: HashMap::new(),
            refill: false,
            renamed: false,
        };
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Saved {
            imported: HashMap::new(),
            covers: HashMap::new(),
            refill: false,
            renamed: false,
        };
    };
    Saved {
        imported: string_map(value.get("imported")),
        covers: string_map(value.get("covers")),
        refill: value
            .get("refill")
            .and_then(|item| item.as_bool())
            .unwrap_or(false),
        renamed: value
            .get("renamed")
            .and_then(|item| item.as_bool())
            .unwrap_or(false),
    }
}

fn string_map(value: Option<&serde_json::Value>) -> HashMap<String, String> {
    value
        .and_then(|item| item.as_object())
        .map(|map| {
            map.iter()
                .filter_map(|(hash, name)| Some((hash.clone(), name.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn save_state(dir: &Path, saved: &Saved) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&serde_json::json!({
        "imported": map_value(&saved.imported),
        "covers": map_value(&saved.covers),
        "refill": saved.refill,
        "renamed": saved.renamed,
    }))
    .map_err(|err| err.to_string())?;
    fs::write(dir.join("imported.json"), text)
        .map_err(|err| format!("не записать список песен: {err}"))
}

fn map_value(map: &HashMap<String, String>) -> serde_json::Map<String, serde_json::Value> {
    let mut entries = serde_json::Map::new();
    for (hash, name) in map {
        entries.insert(hash.clone(), serde_json::Value::String(name.clone()));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_copy_is_only_a_file_inside_plugin_audio() {
        let root = std::env::temp_dir().join(format!("osu-legacy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let audio = root.join("audio");
        fs::create_dir_all(&audio).unwrap();
        let copy = audio.join("Song.mp3");
        fs::write(&copy, b"x").unwrap();
        let src = root.join("ab").join("hash");
        fs::create_dir_all(src.parent().unwrap()).unwrap();
        fs::write(&src, b"x").unwrap();
        assert_eq!(
            legacy_copy(&audio, "Song.mp3", &src).as_deref(),
            Some(copy.as_path())
        );
        assert!(legacy_copy(&audio, &src.display().to_string(), &src).is_none());
        let outside = root.join("Song.mp3");
        fs::write(&outside, b"x").unwrap();
        assert!(legacy_copy(&audio, &outside.display().to_string(), &src).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    fn song(hash: &str, background: &str) -> Song {
        Song {
            hash: hash.to_string(),
            audio_filename: "audio.mp3".into(),
            artist: "cYsmix".into(),
            title: "triangles".into(),
            background_hash: background.to_string(),
            background_filename: "bg.jpg".into(),
        }
    }

    #[test]
    fn missing_cover_is_queued_until_the_background_hash_is_stored() {
        let root = std::env::temp_dir().join(format!("osu-cover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let audio_hash = "ab".repeat(32);
        let bg_hash = "cd".repeat(32);
        let files = root.join("files");
        let audio = hash_path(&files, &audio_hash);
        let background = hash_path(&files, &bg_hash);
        fs::create_dir_all(audio.parent().unwrap()).unwrap();
        fs::create_dir_all(background.parent().unwrap()).unwrap();
        fs::write(&audio, b"ID3xxxx").unwrap();
        fs::write(&background, &[0xFF, 0xD8, 0xFF]).unwrap();
        let plugin = root.join("plugin");
        fs::create_dir_all(&plugin).unwrap();
        let track = song(&audio_hash, &bg_hash);
        let imported = HashMap::from([(audio_hash.clone(), audio.display().to_string())]);
        let mut covers = HashMap::new();
        let (prepared, noted) = prepare_audio(
            &root,
            &plugin,
            &[track.clone()],
            &imported,
            &mut covers,
            &HashSet::new(),
            false,
        )
        .unwrap();
        assert!(!noted);
        assert_eq!(prepared.len(), 1);
        assert!(!prepared[0].fresh);
        assert_eq!(prepared[0].cover.as_deref(), Some(background.as_path()));

        covers.insert(audio_hash.clone(), bg_hash.clone());
        let (prepared, _) = prepare_audio(
            &root,
            &plugin,
            &[track.clone()],
            &imported,
            &mut covers,
            &HashSet::new(),
            false,
        )
        .unwrap();
        assert!(prepared.is_empty());

        covers.clear();
        let tried = HashSet::from([audio_hash.clone()]);
        let (prepared, _) = prepare_audio(
            &root,
            &plugin,
            &[track.clone()],
            &imported,
            &mut covers,
            &tried,
            false,
        )
        .unwrap();
        assert!(
            prepared.is_empty(),
            "a failed cover is not sent again this process"
        );

        let bare = song(&audio_hash, "");
        let mut covers = HashMap::new();
        let (prepared, noted) = prepare_audio(
            &root,
            &plugin,
            &[bare],
            &imported,
            &mut covers,
            &HashSet::new(),
            false,
        )
        .unwrap();
        assert!(prepared.is_empty());
        assert!(noted);
        let (prepared, _) = prepare_audio(
            &root,
            &plugin,
            &[track],
            &imported,
            &mut covers,
            &HashSet::new(),
            true,
        )
        .unwrap();
        assert_eq!(prepared.len(), 1);
        assert!(prepared[0].fresh, "a deleted playlist is filled again");
        assert_eq!(covers.get(&audio_hash).map(String::as_str), Some(""));
        let _ = fs::remove_dir_all(&root);
    }
}

fn log(host: &MuzeekaHost, error: bool, message: &str) {
    if STOP.load(Ordering::SeqCst) {
        return;
    }
    let method = if error { "log.error" } else { "log.info" };
    let payload = serde_json::json!({ "message": message }).to_string();
    let _ = host.call(method, &payload);
}
