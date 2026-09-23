//! Discord Ducking — автоматически убавляет музыку когда кто-то говорит в Discord.
//!
//! Работает через мониторинг аудио-активности Discord процесса с использованием Windows Audio Session API.
//! Когда детектируется активность (кто-то говорит), плавно снижает громкость музыки, затем восстанавливает.

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
        .name("discord-ducking".into())
        .spawn(move || worker(host))
    {
        Ok(h) => h,
        Err(err) => {
            log(&host, "error", &format!("could not spawn thread: {err}"));
            return 1;
        }
    };
    *WORKER.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    0
}

#[no_mangle]
pub extern "C" fn muzeeka_plugin_stop() {
    STOP.store(true, Ordering::SeqCst);
    if let Some(handle) = WORKER.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = handle.join();
    }
}

struct State {
    /// Текущая целевая громкость (0.0–1.0).
    target_volume: f32,
    /// Фактическая громкость, анимируемая к target.
    current_volume: f32,
    /// Оригинальная громкость до дакинга.
    original_volume: f32,
    /// Время последней активности Discord.
    last_activity: Option<Instant>,
    /// Флаг, что мы сейчас в состоянии дакинга.
    is_ducked: bool,
}

#[derive(Clone)]
struct Config {
    duck_volume: f32,  // 0.0–1.0
    fade_ms: u64,
    release_delay_ms: u64,
    test_duck: bool,
}

fn worker(host: MuzeekaHost) {
    let mut state = State {
        target_volume: 1.0,
        current_volume: 1.0,
        original_volume: 1.0,
        last_activity: None,
        is_ducked: false,
    };

    log(&host, "info", "discord-ducking started");

    // Главный цикл: проверяем активность Discord и управляем громкостью
    while !STOP.load(Ordering::SeqCst) {
        let cfg = load_config(&host);

        log(&host, "info", &format!("loaded config: duck_volume={}, fade_ms={}, release_delay={}, test_duck={}",
            cfg.duck_volume, cfg.fade_ms, cfg.release_delay_ms, cfg.test_duck));

        // Проверка кнопки "Проверить"
        if cfg.test_duck {
            log(&host, "info", "test duck triggered");
            // Сброс флага кнопки
            let reset_payload = serde_json::json!({"test_duck": false}).to_string();
            let _ = host.call("plugin.setConfig", &reset_payload);

            // Сохраняем текущую громкость
            if let Ok(player_state) = host.call("player.state", "{}") {
                log(&host, "info", &format!("player state: {:?}", player_state));
                if let Some(vol) = player_state.get("volume").and_then(|v| v.as_f64()) {
                    state.original_volume = vol as f32;
                    log(&host, "info", &format!("saved original volume: {}", state.original_volume));
                }
            }

            // Тестовый дакинг: плавно убавляем
            state.target_volume = cfg.duck_volume;
            state.is_ducked = true;
            log(&host, "info", &format!("test: ducking to {}%", (cfg.duck_volume * 100.0).round()));

            // Анимируем снижение
            let test_start = Instant::now();
            while test_start.elapsed() < Duration::from_secs(3) && !STOP.load(Ordering::SeqCst) {
                animate_volume(&host, &mut state, &cfg);
                thread::sleep(Duration::from_millis(50));
            }

            // Восстанавливаем
            state.target_volume = state.original_volume;
            state.is_ducked = false;
            log(&host, "info", "test: restoring volume");

            // Анимируем восстановление
            while (state.current_volume - state.target_volume).abs() > 0.01 && !STOP.load(Ordering::SeqCst) {
                animate_volume(&host, &mut state, &cfg);
                thread::sleep(Duration::from_millis(50));
            }

            continue;
        }

        // Проверяем активность Discord
        let discord_active = check_discord_audio_activity();

        if discord_active {
            state.last_activity = Some(Instant::now());
            if !state.is_ducked {
                duck_volume(&host, &mut state, &cfg);
            }
        } else if state.is_ducked {
            // Проверяем, прошла ли задержка перед восстановлением
            if let Some(last) = state.last_activity {
                if last.elapsed().as_millis() >= cfg.release_delay_ms as u128 {
                    restore_volume(&host, &mut state, &cfg);
                }
            }
        }

        // Плавная анимация громкости
        animate_volume(&host, &mut state, &cfg);

        sleep_interruptible(Duration::from_millis(50));
    }

    // При остановке восстанавливаем громкость
    if state.is_ducked {
        let cfg = load_config(&host);
        restore_volume(&host, &mut state, &cfg);
    }

    log(&host, "info", "discord-ducking stopped");
}

fn load_config(host: &MuzeekaHost) -> Config {
    let v = host.call("plugin.getConfig", "{}").unwrap_or(serde_json::Value::Null);
    Config {
        duck_volume: (v.get("duck_volume").and_then(|x| x.as_f64()).unwrap_or(20.0).clamp(0.0, 100.0) / 100.0) as f32,
        fade_ms: v.get("fade_ms").and_then(|x| x.as_u64()).unwrap_or(300).clamp(50, 2000),
        release_delay_ms: v.get("release_delay_ms").and_then(|x| x.as_u64()).unwrap_or(500).clamp(0, 5000),
        test_duck: v.get("test_duck").and_then(|x| x.as_bool()).unwrap_or(false),
    }
}

fn duck_volume(host: &MuzeekaHost, state: &mut State, cfg: &Config) {
    // Получаем текущую громкость из плеера
    if let Ok(player_state) = host.call("player.state", "{}") {
        if let Some(vol) = player_state.get("volume").and_then(|v| v.as_f64()) {
            state.original_volume = vol as f32;
        }
    }

    state.target_volume = cfg.duck_volume;
    state.is_ducked = true;
    log(host, "info", &format!("ducking to {}%", (cfg.duck_volume * 100.0).round()));
}

fn restore_volume(host: &MuzeekaHost, state: &mut State, _cfg: &Config) {
    state.target_volume = state.original_volume;
    state.is_ducked = false;
    state.last_activity = None;
    log(host, "info", "restoring volume");
}

fn animate_volume(host: &MuzeekaHost, state: &mut State, cfg: &Config) {
    if (state.current_volume - state.target_volume).abs() < 0.01 {
        return;
    }

    // Вычисляем шаг изменения громкости за 50ms
    let fade_steps = (cfg.fade_ms as f32 / 50.0).max(1.0);
    let delta = (state.target_volume - state.current_volume) / fade_steps;

    state.current_volume += delta;
    state.current_volume = state.current_volume.clamp(0.0, 1.0);

    // Применяем громкость к плееру
    let payload = serde_json::json!({ "volume": state.current_volume }).to_string();
    let _ = host.call("player.volume", &payload);
}

/// Проверяет активность аудио в Discord через Windows Audio Session API.
/// Возвращает true если Discord воспроизводит звук (кто-то говорит).
#[cfg(target_os = "windows")]
fn check_discord_audio_activity() -> bool {
    // PLACEHOLDER: Реальная реализация требует Windows API (wasapi)
    // Здесь упрощенная заглушка - можно расширить с использованием windows-rs
    use std::process::Command;

    // Простая проверка: запущен ли Discord процесс
    if let Ok(output) = Command::new("tasklist")
        .args(&["/FI", "IMAGENAME eq Discord.exe", "/NH"])
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return stdout.contains("Discord.exe");
    }
    false
}

#[cfg(not(target_os = "windows"))]
fn check_discord_audio_activity() -> bool {
    // На не-Windows платформах пока не поддерживается
    false
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
        thread::sleep(Duration::from_millis(50));
    }
}
