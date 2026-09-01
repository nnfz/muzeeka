//! Micspam — дублирует выход плеера в виртуальный аудиокабель.
//!
//! Что делает: находит среди выходов устройство по имени (`device_match`), цепляет к нему
//! параллельный вывод плеера и следит, чтобы вывод не отвалился. В Discord/игре микрофоном
//! ставится парный вход кабеля (`CABLE Output`).
//!
//! Драйвер кабеля — kernel-mode, из плагина его не создать: VB-CABLE ставится один раз
//! вручную. Если устройства нет, плагин просто пишет об этом в лог и ждёт.

#[path = "../../sdk/muzeeka_plugin.rs"]
mod muzeeka_plugin;

use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use muzeeka_plugin::{MuzeekaHost, MUZEEKA_PLUGIN_ABI};

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

    let handle = match thread::Builder::new()
        .name("micspam".into())
        .spawn(move || worker(host))
    {
        Ok(h) => h,
        Err(err) => {
            log(&host, "error", &format!("не удалось создать поток: {err}"));
            return 1;
        }
    };
    *WORKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    0
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    STOP.store(true, Ordering::SeqCst);
    // Воркер сам снимает вывод перед выходом, поэтому join обязателен до возврата:
    // после stop() указатель host больше не валиден.
    if let Some(handle) = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = handle.join();
    }
}

/// Состояние воркера между итерациями опроса.
struct State {
    /// id активного дополнительного вывода (`out-N`), пока он подключён.
    output_id: Option<String>,
    /// Последнее сообщение об ошибке — чтобы не засорять лог каждые две секунды.
    last_error: Option<String>,
    /// Громкость, которую хост уже принял. Нужна, чтобы не дёргать setOutputVolume
    /// на каждой итерации опроса.
    applied_volume: Option<f32>,
}

fn worker(host: MuzeekaHost) {
    let mut state = State {
        output_id: None,
        last_error: None,
        applied_volume: None,
    };

    log(&host, "info", "micspam запущен");

    while !STOP.load(Ordering::SeqCst) {
        let cfg = settings(&host);
        tick(&host, &mut state, &cfg);
        sleep_interruptible(Duration::from_millis(cfg.poll_ms));
    }

    detach(&host, &mut state);
    log(&host, "info", "micspam остановлен");
}

struct Config {
    device_match: String,
    poll_ms: u64,
    /// Громкость только для кабеля, 0.0–1.0. В ушах ничего не меняется.
    volume: f32,
}

fn settings(host: &MuzeekaHost) -> Config {
    let v = host.call("settings.get", "{}").unwrap_or(serde_json::Value::Null);
    let s = |key: &str, fallback: &str| -> String {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .unwrap_or(fallback)
            .to_string()
    };
    Config {
        device_match: s("device_match", "CABLE Input"),
        poll_ms: v
            .get("poll_ms")
            .and_then(|x| x.as_u64())
            .unwrap_or(2000)
            .clamp(500, 10_000),
        // Настройка в процентах — так ползунок в UI понятнее, чем 0.0–1.0.
        volume: (v
            .get("volume_percent")
            .and_then(|x| x.as_f64())
            .unwrap_or(70.0)
            .clamp(0.0, 100.0)
            / 100.0) as f32,
    }
}

/// Одна итерация: убедиться, что кабель на месте и вывод к нему подключён.
fn tick(host: &MuzeekaHost, state: &mut State, cfg: &Config) {
    // Вывод уже подключён — проверяем, что хост о нём всё ещё знает. Перезапуск BASS
    // (смена устройства, Ctrl+R) может его снять; тогда цепляем заново.
    if let Some(id) = state.output_id.clone() {
        match host.call("audio.outputs", "{}") {
            Ok(list) => {
                let alive = list
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .any(|o| o.get("id").and_then(|x| x.as_str()) == Some(id.as_str()))
                    })
                    .unwrap_or(false);
                if alive {
                    apply_volume(host, state, &id, cfg.volume);
                    return;
                }
                state.output_id = None;
                state.applied_volume = None;
                log(host, "info", "вывод отвалился, подключаю заново");
            }
            Err(err) => {
                report(host, state, &format!("audio.outputs: {err}"));
                return;
            }
        }
    }

    let devices = match host.call("audio.devices", "{}") {
        Ok(d) => d,
        Err(err) => {
            report(host, state, &format!("audio.devices: {err}"));
            return;
        }
    };

    let Some(device) = find_device(&devices, &cfg.device_match) else {
        report(
            host,
            state,
            &format!(
                "устройство с именем «{}» не найдено. Установи VB-CABLE и выбери его вход в настройках плагина.",
                cfg.device_match
            ),
        );
        return;
    };

    // Громкость передаём сразу при подключении: хост выставит её до channel_play,
    // чтобы кабель не выстрелил на полной громкости в первые миллисекунды.
    let payload =
        serde_json::json!({ "deviceId": device.0, "volume": cfg.volume }).to_string();
    match host.call("audio.addOutput", &payload) {
        Ok(info) => {
            let id = info
                .get("id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            if id.is_empty() {
                report(host, state, "addOutput вернул пустой id");
                return;
            }
            state.output_id = Some(id);
            state.applied_volume = Some(cfg.volume);
            state.last_error = None;
            log(
                host,
                "info",
                &format!(
                    "музыка идёт в «{}» на {}% — ставь парный вход микрофоном",
                    device.1,
                    (cfg.volume * 100.0).round()
                ),
            );
        }
        Err(err) => report(host, state, &format!("addOutput «{}»: {err}", device.1)),
    }
}

/// Двигает громкость кабеля, если ползунок изменился. Основной выход не трогается —
/// у него своя громкость на мижере.
fn apply_volume(host: &MuzeekaHost, state: &mut State, output_id: &str, volume: f32) {
    // Настройка приходит из UI в целых процентах, так что сравнение на равенство
    // здесь устойчиво: дробных дребезжаний быть не может.
    if state.applied_volume == Some(volume) {
        return;
    }
    let payload = serde_json::json!({ "id": output_id, "volume": volume }).to_string();
    match host.call("audio.setOutputVolume", &payload) {
        Ok(_) => {
            state.applied_volume = Some(volume);
            state.last_error = None;
            log(
                host,
                "info",
                &format!("громкость микспама: {}%", (volume * 100.0).round()),
            );
        }
        Err(err) => report(host, state, &format!("setOutputVolume: {err}")),
    }
}

fn detach(host: &MuzeekaHost, state: &mut State) {
    state.applied_volume = None;
    if let Some(id) = state.output_id.take() {
        let payload = serde_json::json!({ "id": id }).to_string();
        if let Err(err) = host.call("audio.removeOutput", &payload) {
            log(host, "error", &format!("removeOutput: {err}"));
        }
    }
}

/// Ищет включённое устройство, чьё имя содержит `needle` (без учёта регистра).
/// Возвращает `(deviceId, name)`.
fn find_device(devices: &serde_json::Value, needle: &str) -> Option<(i64, String)> {
    let needle = needle.to_lowercase();
    devices.as_array()?.iter().find_map(|d| {
        let name = d.get("name").and_then(|x| x.as_str())?;
        if !name.to_lowercase().contains(&needle) {
            return None;
        }
        if !d.get("enabled").and_then(|x| x.as_bool()).unwrap_or(false) {
            return None;
        }
        let id = d.get("id").and_then(|x| x.as_i64())?;
        if id <= 0 {
            return None;
        }
        Some((id, name.to_string()))
    })
}

// ── Логирование ─────────────────────────────────────────────────────────────

/// Пишет ошибку один раз, пока текст не изменится: опрос идёт каждые пару секунд.
fn report(host: &MuzeekaHost, state: &mut State, msg: &str) {
    if state.last_error.as_deref() == Some(msg) {
        return;
    }
    state.last_error = Some(msg.to_string());
    log(host, "error", msg);
}

fn log(host: &MuzeekaHost, level: &str, msg: &str) {
    let payload = serde_json::json!({ "message": msg }).to_string();
    let _ = host.call(&format!("log.{level}"), &payload);
}

fn sleep_interruptible(total: Duration) {
    let start = Instant::now();
    while start.elapsed() < total {
        if STOP.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}
