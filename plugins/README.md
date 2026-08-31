# Плагины Muzeeka

Muzeeka поддерживает плагины двух типов: **JS** (скрипт, исполняется встроенным движком
Boa) и **нативные** (Windows-DLL с C-ABI). Плагины получают доступ к плееру, библиотеке,
аудиоустройствам, настройкам и могут поднимать собственный HTTP-сервер (со статикой и
REST/SSE API плеера).

Папка плагинов:

- **dev-сборка** — `plugins/` в корне репозитория (рядом с `src-tauri`);
- **packaged-сборка** — `plugins/` рядом с exe (иначе — в ресурсах приложения).

Плагин — это папка с файлом `plugin.json` внутри. Положили папку → перезапустили/обновили
список в *Настройки → Плагины*. Папка `sdk/` сканером игнорируется.

Данные плагина (настройки и прочее) лежат отдельно, в `<app data>/plugin-data/<id>/`.

---

## plugin.json

```json
{
  "id": "user.mixpamp",
  "name": "MixPamp",
  "version": "0.1.0",
  "author": "you",
  "description": "Что делает плагин",
  "main": "index.js",
  "runtime": "js",
  "enabled_by_default": false,
  "permissions": ["player:read", "player:control"],
  "settings": [
    {
      "key": "gain",
      "type": "number",
      "label": "Усиление",
      "description": "0.0–2.0",
      "min": 0,
      "max": 2,
      "default": 1
    }
  ]
}
```

| Поле | Описание |
|---|---|
| `id` | Обязателен. Формат: минимум две секции через точку (`vendor.name`), только `a-z 0-9 . _ -`, без точек по краям и `..`. Пример: `muzeeka.remote`, `user.mixpamp`. |
| `name` | Обязателен, не пустой. Отображается в настройках. |
| `version`, `author`, `description` | Необязательные, показываются в UI. |
| `main` | Точка входа, по умолчанию `index.js`. Для нативных — путь к `.dll` (относительно папки плагина). |
| `runtime` | `"js"` или `"native"` (`"dll"` тоже допустимо). Если не указан — определяется по расширению `main`. |
| `enabled_by_default` | Включать плагин при первом обнаружении (пользователь всё равно может выключить). |
| `permissions` | Список прав, см. ниже. Неизвестное право → манифест не принимается. |
| `settings` | Декларативные настройки; форма для них рисует сама Muzeeka, без кода UI. |

### settings[]

Каждый элемент: `key`, `type` (`number` \| `boolean` \| `string`), `label`, `description`,
для чисел — `min`/`max`, и необязательный `default`. Хост приводит сохранённые значения к
объявленному типу, clamp-ит числа в min/max и подставляет default, поэтому плагин всегда
получает валидное значение. Изменение настроек в UI **перезапускает** плагин.

---

## Права (permissions)

| Право | Открывает |
|---|---|
| `player:read` | `player.state` |
| `player:control` | `player.play / pause / resume / toggle / next / prev / seek / volume` |
| `library:read` | `library.playlists`, `library.playlist` |
| `audio:devices` | `audio.devices` |
| `audio:output` | `audio.addOutput`, `audio.removeOutput`, `audio.outputs` |
| `http:listen` | `http.serve`, `http.stop`, `http.status` |
| `fs:plugin-dir` | Зарезервировано, пока ничего не даёт. |

Вызов без нужного права возвращает ошибку `Plugin is missing permission '<x>'`.

---

## API плагина

JS-плагины получают глобальный объект `muzeeka`, нативные — те же методы через
`host.call("<method>", "<json>")`. Имена и payload идентичны.

```js
muzeeka.player
  .state()                  // снапшот: isPlaying, isPaused, position, duration, volume,
                            // shuffleEnabled, repeatMode, track, activePlaylistId/Name
  .play(path, playlistId?)  // сыграть файл (playlistId — контекст воспроизведения)
  .pause() .resume() .toggle() .next() .prev()
  .seek(position)           // секунды
  .volume(v)                // 0.0–1.0

muzeeka.library
  .playlists()              // [{ id, name, trackCount }]
  .playlist(id)             // { id, name, tracks: [{ path, title, artist, album, durationSecs, coverUrl }] }

muzeeka.audio
  .devices()                // устройства вывода
  .addOutput(deviceId)      // добавить параллельный вывод
  .removeOutput(id)
  .outputs()                // активные доп. выводы

muzeeka.http
  .serve({ port, staticDir, mount: ["player-api"] })
  .stop() .status()

muzeeka.settings
  .get(key?)                // без аргумента — весь объект настроек
  .set(key, value)          // или .set({ ...patch })

muzeeka.log
  .info(msg) .error(msg)    // попадает в dev-лог и виден в настройках
```

Ошибки (нет права, неверные аргументы, неизвестный метод) бросаются как `Error` в JS и
возвращаются как `{"__error":"..."}` в нативных вызовах.

`track` в состоянии плеера: `{ path, title, artist, album, durationSecs, coverUrl }` или
`null`.

---

## JS-плагины

Файл `main` (обычно `index.js`) исполняется при запуске плагина, затем вызываются хуки:

```js
function start(muzeeka) {
  const port = muzeeka.settings.get("port") || 8765;
  muzeeka.http.serve({ port, staticDir: "ui", mount: ["player-api"] });
}

function stop(muzeeka) {
  muzeeka.http.stop();
}
```

Важные особенности окружения:

- Каждый плагин — отдельный JS-контекст на общем потоке движка. `start()` должен быстро
  завершаться: долгий цикл внутри заблокирует старт/остановку остальных JS-плагинов.
- **Хост-вызовы доступны только внутри `start()` и `stop()`** — после возврата из `start()`
  вызовы `muzeeka.*` бросают ошибку. Таймеров, событий и фоновых колбэков в JS-рантайме
  нет: JS-плагин — это «сконфигурировал сервер при старте и завершился».
- `console` нет — логируйте через `muzeeka.log`.
- Ошибка в скрипте или в `start()` = плагин не запущен; текст ошибки виден в настройках.

Рабочий сценарий JS-плагина — веб-интерфейс: `http.serve` раздаёт статику из папки
плагина (`staticDir` — относительный путь) и, при `mount: ["player-api"]`, полный REST/SSE
API плеера (см. ниже). Живая логика на стороне клиента в браузере.

## Нативные плагины (DLL)

`main` указывает на DLL с ABI 1. DLL должна лежать **внутри папки плагина**.

Три обязательных экспорта:

```c
#include "muzeeka_plugin.h"

uint32_t muzeeka_plugin_abi(void);                       // вернуть MUZEEKA_PLUGIN_ABI
int      muzeeka_plugin_start(const MuzeekaHost *host);  // 0 = ок
void     muzeeka_plugin_stop(void);
```

`MuzeekaHost` — три поля: `data`, `call(data, method, payload_json) -> char*`,
`free_str(ptr)`. Payload и результат — UTF-8 JSON; ошибка — `{"__error":"..."}`. Строку из
`call` нужно освободить через `free_str`. Указатель `host` валиден до возврата из `stop`;
после этого звать хост нельзя.

Правила:

- Долгую работу делайте в собственных потоках и **join-ьте их в `stop`** (см.
  `plugins/native-probe/src/lib.rs`).
- Паника/крэш в DLL убивает весь Muzeeka — изоляции нет.
- DLL грузится с `LOAD_WITH_ALTERED_SEARCH_PATH`, так что свои зависимые DLL кладите рядом
  в папку плагина.
- Несовпадение ABI → плагин не стартует, нужно пересобрать против актуального
  `plugins/sdk/muzeeka_plugin.h`.

### Rust

Возьмите хелпер `plugins/sdk/muzeeka_plugin.rs` (структура `MuzeekaHost` с методом
`call(method, payload) -> Result<serde_json::Value, String>`), минимальный пример —
`plugins/sdk/example.rs`, живой — `plugins/native-probe`. Крейт собирается как `cdylib`.

```toml
[lib]
crate-type = ["cdylib"]
```

### C/C++

Подключите `plugins/sdk/muzeeka_plugin.h` и реализуйте три экспорта.

---

## HTTP-сервер плагина

Каждому плагину — свой слушатель (повторный `serve` с теми же опциями — no-op, с новыми —
перезапуск). Опции:

- `port` — порты < 1024 заменяются на 8765;
- `staticDir` — папка статики относительно папки плагина; несуществующие пути отдаются
  `index.html` (SPA-режим);
- `mount: ["player-api"]` — смонтировать REST/SSE API плеера.

Сервер слушает `0.0.0.0`, CORS открыт полностью. `http.status()` и карточка плагина в
настройках показывают `localhost`-URL и лучший LAN-адрес (эвристики отбрасывают
VPN/virtual-адаптеры и fake-IP диапазоны).

### Player API (`mount: ["player-api"]`)

```
GET  /api, /api/info      описание API (self-documenting)
GET  /api/state           снапшот состояния плеера
GET  /api/stream          живой поток состояния: SSE (событие "state") по умолчанию,
                          ?format=ndjson — построчный JSON, ?interval=мс (50–2000, по умолч. 250)
GET  /api/events          алиас /api/stream
GET  /api/playlists       список плейлистов
GET  /api/playlist?id=…   треки плейлиста
GET  /api/cover?path=…    байты обложки трека
POST /api/play            { "path": "...", "playlistId"?: "..." }
POST /api/toggle | /api/pause | /api/resume | /api/next | /api/prev
POST /api/seek            { "position": 12.5 }
POST /api/volume          { "volume": 0.5 }
POST /api/playlist/select { "id": "..." }
POST /api/shuffle/toggle  → { "shuffle_enabled": bool }
POST /api/repeat/toggle   → { "repeat_mode": "off|one|all" }
```

---

## Плагины в комплекте

### muzeeka.remote — JS

Пульт управления с телефона: на старте поднимает HTTP-сервер (настройка `port`, по
умолчанию 8765), раздаёт `ui/index.html` (тёмный мобильный интерфейс) и player-api.
Права: `player:read`, `player:control`, `library:read`, `http:listen`. Включён по
умолчанию; при первом запуске настройки порта/вкл-выкл переносятся со старого встроенного
remote-модуля.

### muzeeka.native-probe — нативный

Тестовый DLL-плагин: раз в `interval_ms` (500–60000, по умолчанию 3000) пишет в лог
«что играет». Демонстрирует фоновый поток, чтение настроек и `player.state` из нативного
кода. Выключен по умолчанию. Пересборка: `cargo build --release` в `plugins/native-probe`.

### plugins/sdk

`muzeeka_plugin.h` (C/C++), `muzeeka_plugin.rs` (Rust-хелпер), `example.rs` (минимальный
пример). Сканером плагинов не сканируется.

---

## Чек-лист: что умеет плагин

- Читать состояние плеера и библиотеку (плейлисты, треки).
- Управлять воспроизведением: play/pause/next/prev/seek/volume.
- Добавлять и убирать параллельные аудиовыходы (например, вторая колонка).
- Поднимать HTTP-сервер со своим веб-UI и REST/SSE API плеера — удалённый пульт,
  интеграции, OBS-оверлеи и т.п.
- Хранить настройки: декларативная форма в UI Muzeeka + чтение/запись из плагина.
- Писать в dev-лог (`muzeeka.log`).
- Нативные плагины: любая фоновая логика в своих потоках (поллинг, hotkeys, сетевые
  клиенты) — в рамках того же набора хост-методов.

Чего пока нет: подписки на события плеера из JS (только поллинг в нативных потоках или
SSE через собственный HTTP), произвольный доступ к файловой системе, свои страницы UI
внутри окна Muzeeka.
