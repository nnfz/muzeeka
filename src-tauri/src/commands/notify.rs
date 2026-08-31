// Coalesced playback notifications for Discord RPC and plugin hosts.
//
// A single long-lived worker processes the latest snapshot instead of spawning
// a new OS thread on every play/pause/seek IPC call.

use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::discord_rpc::DiscordPresence;
use crate::player::Player;
use crate::session::PlaybackSession;

struct Pending {
    player: Player,
    discord: DiscordPresence,
    controller: Arc<PlaybackSession>,
    /// Seek-style updates wait for a quiet period so drag doesn't flood Discord IPC.
    debounce: bool,
}

struct NotifyHub {
    pending: Mutex<Option<Pending>>,
}

static HUB: OnceLock<(Arc<NotifyHub>, mpsc::SyncSender<()>)> = OnceLock::new();

fn hub() -> &'static (Arc<NotifyHub>, mpsc::SyncSender<()>) {
    HUB.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        let hub = Arc::new(NotifyHub {
            pending: Mutex::new(None),
        });
        let worker_hub = Arc::clone(&hub);
        thread::Builder::new()
            .name("playback-notify".into())
            .spawn(move || notify_worker(worker_hub, rx))
            .expect("failed to start playback-notify thread");
        (hub, tx)
    })
}

fn notify_worker(hub: Arc<NotifyHub>, rx: mpsc::Receiver<()>) {
    const DEBOUNCE: Duration = Duration::from_millis(400);

    while rx.recv().is_ok() {
        loop {
            while rx.try_recv().is_ok() {}

            let needs_debounce = {
                let slot = hub.pending.lock().unwrap_or_else(|e| e.into_inner());
                match slot.as_ref() {
                    Some(p) => p.debounce,
                    None => break,
                }
            };

            if needs_debounce {
                thread::sleep(DEBOUNCE);
                while rx.try_recv().is_ok() {}
            }

            let job = {
                let mut slot = hub.pending.lock().unwrap_or_else(|e| e.into_inner());
                // If a non-debounced update arrived during sleep, prefer processing it now
                // without another wait; otherwise take whatever is latest.
                slot.take()
            };

            let Some(job) = job else {
                break;
            };

            job.discord.update_from_player(&job.player.get_state());
            job.controller.notify_playback();
        }
    }
}

fn enqueue(
    player: &Player,
    discord: &DiscordPresence,
    controller: &Arc<PlaybackSession>,
    debounce: bool,
) {
    let (hub, wake) = hub();
    {
        let mut slot = hub.pending.lock().unwrap_or_else(|e| e.into_inner());
        // Immediate work must not be delayed by a prior seek debounce flag.
        let debounce = match slot.as_ref() {
            Some(existing) if !existing.debounce => false,
            _ => debounce,
        };
        *slot = Some(Pending {
            player: player.clone(),
            discord: discord.clone(),
            controller: Arc::clone(controller),
            debounce,
        });
    }
    let _ = wake.try_send(());
}

/// Notify Discord and plugin hosts after play / pause / resume / stop.
pub fn notify_playback_change(
    player: &Player,
    discord: &DiscordPresence,
    controller: &Arc<PlaybackSession>,
) {
    enqueue(player, discord, controller, false);
}

/// Same as [`notify_playback_change`], but coalesces high-frequency seek updates.
pub fn notify_playback_seek(
    player: &Player,
    discord: &DiscordPresence,
    controller: &Arc<PlaybackSession>,
) {
    enqueue(player, discord, controller, true);
}
