use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

use boa_engine::{
    js_string, Context, JsArgs, JsResult, JsString, JsValue, NativeFunction, Source,
};

use super::host::{PluginCall, PluginHost};

const PRELUDE: &str = r#"
function __call(method, args) {
  var raw = __host(method, JSON.stringify(args === undefined ? {} : args));
  if (typeof raw !== "string") return raw;
  var parsed;
  try { parsed = JSON.parse(raw); } catch (e) { return raw; }
  if (parsed && typeof parsed === "object" && parsed.__error) {
    throw new Error(parsed.__error);
  }
  return parsed;
}
var muzeeka = {
  player: {
    state: function() { return __call("player.state"); },
    play: function(path, playlistId) { return __call("player.play", { path: path, playlistId: playlistId }); },
    pause: function() { return __call("player.pause"); },
    resume: function() { return __call("player.resume"); },
    toggle: function() { return __call("player.toggle"); },
    next: function() { return __call("player.next"); },
    prev: function() { return __call("player.prev"); },
    seek: function(position) { return __call("player.seek", { position: position }); },
    volume: function(volume) { return __call("player.volume", { volume: volume }); }
  },
  library: {
    playlists: function() { return __call("library.playlists"); },
    playlist: function(id) { return __call("library.playlist", { id: id }); }
  },
  audio: {
    devices: function() { return __call("audio.devices"); },
    addOutput: function(deviceId) { return __call("audio.addOutput", { deviceId: deviceId }); },
    removeOutput: function(id) { return __call("audio.removeOutput", { id: id }); },
    outputs: function() { return __call("audio.outputs"); }
  },
  http: {
    serve: function(opts) { return __call("http.serve", opts || {}); },
    stop: function() { return __call("http.stop"); },
    status: function() { return __call("http.status"); }
  },
  settings: {
    get: function(key) {
      var all = __call("settings.get");
      if (key === undefined || key === null) return all;
      return all[key];
    },
    set: function(key, value) {
      if (typeof key === "object") return __call("settings.set", { values: key });
      var patch = {};
      patch[key] = value;
      return __call("settings.set", { values: patch });
    }
  },
  log: {
    info: function(msg) { return __call("log.info", { message: String(msg) }); },
    error: function(msg) { return __call("log.error", { message: String(msg) }); }
  }
};
"#;

struct StartJob {
    plugin_id: String,
    source: String,
    permissions: Vec<String>,
    dir: std::path::PathBuf,
    host: PluginHost,
    reply: mpsc::Sender<Result<(), String>>,
}

struct StopJob {
    plugin_id: String,
    reply: mpsc::Sender<Result<(), String>>,
}

enum JsMsg {
    Start(StartJob),
    Stop(StopJob),
}

#[derive(Clone)]
pub struct JsEngine {
    tx: mpsc::Sender<JsMsg>,
}

impl JsEngine {
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::channel::<JsMsg>();
        thread::Builder::new()
            .name("muzeeka-plugins".into())
            .spawn(move || worker(rx))
            .expect("failed to start plugin JS thread");
        Self { tx }
    }

    pub fn start(
        &self,
        plugin_id: &str,
        source: String,
        permissions: Vec<String>,
        dir: std::path::PathBuf,
        host: PluginHost,
    ) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(JsMsg::Start(StartJob {
                plugin_id: plugin_id.to_string(),
                source,
                permissions,
                dir,
                host,
                reply,
            }))
            .map_err(|_| "Plugin JS runtime is gone".to_string())?;
        rx.recv()
            .map_err(|_| "Plugin JS runtime did not answer".to_string())?
    }

    pub fn stop(&self, plugin_id: &str) -> Result<(), String> {
        let (reply, rx) = mpsc::channel();
        self.tx
            .send(JsMsg::Stop(StopJob {
                plugin_id: plugin_id.to_string(),
                reply,
            }))
            .map_err(|_| "Plugin JS runtime is gone".to_string())?;
        rx.recv()
            .map_err(|_| "Plugin JS runtime did not answer".to_string())?
    }
}

struct LivePlugin {
    context: Context,
    plugin_id: String,
    permissions: Vec<String>,
    dir: std::path::PathBuf,
    host: PluginHost,
}

thread_local! {
    static CURRENT: std::cell::RefCell<Option<CurrentCall>> = const { std::cell::RefCell::new(None) };
}

struct CurrentCall {
    plugin_id: String,
    permissions: Vec<String>,
    dir: std::path::PathBuf,
    host: PluginHost,
}

fn worker(rx: mpsc::Receiver<JsMsg>) {
    let mut live: HashMap<String, LivePlugin> = HashMap::new();
    while let Ok(msg) = rx.recv() {
        match msg {
            JsMsg::Start(job) => start_plugin(&mut live, job),
            JsMsg::Stop(job) => {
                let result = stop_plugin(&mut live, &job.plugin_id);
                let _ = job.reply.send(result);
            }
        }
    }
}

fn start_plugin(live: &mut HashMap<String, LivePlugin>, job: StartJob) {
    if live.contains_key(&job.plugin_id) {
        let _ = stop_plugin(live, &job.plugin_id);
    }

    let result = (|| {
        let mut context = Context::default();
        context
            .register_global_callable(js_string!("__host"), 2, NativeFunction::from_fn_ptr(js_host))
            .map_err(|e| format!("JS init: {e}"))?;
        context
            .eval(Source::from_bytes(PRELUDE))
            .map_err(|e| format!("JS prelude: {e}"))?;
        context
            .eval(Source::from_bytes(job.source.as_bytes()))
            .map_err(|e| format!("Plugin script: {e}"))?;

        CURRENT.with(|slot| {
            *slot.borrow_mut() = Some(CurrentCall {
                plugin_id: job.plugin_id.clone(),
                permissions: job.permissions.clone(),
                dir: job.dir.clone(),
                host: job.host.clone(),
            });
        });
        let start_result = context.eval(Source::from_bytes(
            b"if (typeof start === 'function') { start(muzeeka); }",
        ));
        CURRENT.with(|slot| slot.borrow_mut().take());
        start_result.map_err(|e| format!("start(): {e}"))?;

        live.insert(
            job.plugin_id.clone(),
            LivePlugin {
                context,
                plugin_id: job.plugin_id.clone(),
                permissions: job.permissions,
                dir: job.dir,
                host: job.host,
            },
        );
        Ok(())
    })();

    let _ = job.reply.send(result);
}

fn stop_plugin(live: &mut HashMap<String, LivePlugin>, plugin_id: &str) -> Result<(), String> {
    let Some(mut plugin) = live.remove(plugin_id) else {
        return Ok(());
    };
    CURRENT.with(|slot| {
        *slot.borrow_mut() = Some(CurrentCall {
            plugin_id: plugin.plugin_id.clone(),
            permissions: plugin.permissions.clone(),
            dir: plugin.dir.clone(),
            host: plugin.host.clone(),
        });
    });
    let result = plugin
        .context
        .eval(Source::from_bytes(
            b"if (typeof stop === 'function') { stop(muzeeka); }",
        ))
        .map(|_| ())
        .map_err(|e| format!("stop(): {e}"));
    CURRENT.with(|slot| slot.borrow_mut().take());
    plugin.host.http.stop(plugin_id);
    result
}

fn js_host(_this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let method = args
        .get_or_undefined(0)
        .to_string(ctx)?
        .to_std_string_escaped();
    let payload = args
        .get_or_undefined(1)
        .to_string(ctx)?
        .to_std_string_escaped();

    let result = CURRENT.with(|slot| {
        let slot = slot.borrow();
        let Some(current) = slot.as_ref() else {
            return Err("plugin host called outside start/stop".to_string());
        };
        current.host.dispatch(
            &PluginCall {
                plugin_id: &current.plugin_id,
                permissions: &current.permissions,
                dir: &current.dir,
            },
            &method,
            &payload,
        )
    });

    match result {
        Ok(value) => {
            let raw = serde_json::to_string(&value).unwrap_or_else(|_| "null".into());
            Ok(JsValue::from(JsString::from(raw.as_str())))
        }
        Err(err) => {
            let raw = serde_json::json!({ "__error": err }).to_string();
            Ok(JsValue::from(JsString::from(raw.as_str())))
        }
    }
}

