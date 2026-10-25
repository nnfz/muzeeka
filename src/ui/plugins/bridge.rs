//! Plugin threads talk to [`Session`](crate::ui::session::Session) through this queue.
//!
//! The library connection lives on the GPUI thread. A call from a plugin worker
//! waits until that thread runs it. A call that is already on the UI thread,
//! inside [`enter`], runs immediately. [`drain`] also runs queued calls, so a
//! plugin `stop` that joins its own workers cannot deadlock the window.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use gpui::{App, Entity};
use parking_lot::Mutex;

use crate::ui::session::Session;

struct Job {
    run: Box<dyn FnOnce(&mut Session) + Send>,
}

static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
static SESSION: Mutex<Option<Entity<Session>>> = Mutex::new(None);
static UI_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

thread_local! {
    static APP: Cell<*mut App> = const { Cell::new(std::ptr::null_mut()) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
}

pub fn set_session(session: Entity<Session>) {
    *UI_THREAD.lock() = Some(std::thread::current().id());
    *SESSION.lock() = Some(session);
}

pub fn session() -> Option<Entity<Session>> {
    SESSION.lock().clone()
}

/// Run `body` while plugin calls on this thread can touch `cx`.
///
/// The pointer is only read on this thread, and only until `body` returns.
/// `cx` itself is not used again until then.
pub fn enter<T>(cx: &mut App, body: impl FnOnce() -> T) -> T {
    let ptr = cx as *mut App;
    let previous = APP.with(|slot| slot.replace(ptr));
    let result = body();
    APP.with(|slot| slot.set(previous));
    result
}

pub fn with_session<T: Send + 'static>(
    f: impl FnOnce(&mut Session) -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    if on_ui_thread() {
        return call_inline(f);
    }
    let (tx, rx) = mpsc::channel();
    QUEUE.lock().push_back(Job {
        run: Box::new(move |session| {
            let _ = tx.send(f(session));
        }),
    });
    rx.recv_timeout(Duration::from_secs(120))
        .map_err(|_| "The player did not answer the plugin".to_string())?
}

pub fn drain(cx: &mut App, session: &Entity<Session>) {
    loop {
        let job = {
            let mut queue = QUEUE.lock();
            queue.pop_front()
        };
        let Some(job) = job else {
            break;
        };
        session.update(cx, |session, cx| {
            BUSY.set(true);
            (job.run)(session);
            BUSY.set(false);
            cx.notify();
        });
    }
}

/// Block until `rx` answers, running queued plugin calls in between.
/// Used while a plugin starts or stops on the UI thread.
pub fn wait_on_ui<T>(rx: &mpsc::Receiver<T>, timeout: Duration) -> Result<T, String> {
    if !on_ui_thread() {
        return rx
            .recv_timeout(timeout)
            .map_err(|_| "Plugin did not finish".to_string());
    }
    let started = Instant::now();
    loop {
        drain_from_tls();
        match rx.recv_timeout(Duration::from_millis(15)) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if started.elapsed() > timeout {
                    return Err("Plugin did not finish".into());
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Plugin runtime is gone".into());
            }
        }
    }
}

fn drain_from_tls() {
    let ptr = APP.with(|slot| slot.get());
    let Some(session) = session() else {
        return;
    };
    if ptr.is_null() {
        return;
    }
    let cx = unsafe { &mut *ptr };
    drain(cx, &session);
}

fn call_inline<T>(
    f: impl FnOnce(&mut Session) -> Result<T, String>,
) -> Result<T, String> {
    if BUSY.get() {
        return Err("Plugin host was called again while the library was busy".into());
    }
    let ptr = APP.with(|slot| slot.get());
    if ptr.is_null() {
        return Err("Plugin host was called on the UI thread outside the player".into());
    }
    let Some(session) = session() else {
        return Err("Plugins are not attached".into());
    };
    let cx = unsafe { &mut *ptr };
    session.update(cx, |session, cx| {
        BUSY.set(true);
        let result = f(session);
        BUSY.set(false);
        cx.notify();
        result
    })
}

fn on_ui_thread() -> bool {
    UI_THREAD
        .lock()
        .is_some_and(|id| id == std::thread::current().id())
}
