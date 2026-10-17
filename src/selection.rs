//! Owned Wayland clipboard selection with KDE's password hint.
//!
//! Why this exists: Klipper's D-Bus setClipboardContents (and wl-copy /
//! xclip) can only hand over a plain string. The clipboard offer is then
//! created by someone else, with no extra formats, so a copied password
//! lands in Klipper's plaintext history. KDE's convention for password
//! managers is to offer an extra MIME type, `x-kde-passwordManagerHint`
//! (data: "secret"), alongside the text; clipboard managers that see it
//! keep the content out of history. KeePassXC and Firefox do exactly
//! this. Attaching that format requires owning the selection ourselves.
//!
//! Ownership is via the data-control protocols, the only Wayland
//! mechanism a background process can use to set the clipboard
//! (wl_data_device.set_selection needs an input serial we never have):
//! ext-data-control-v1 (KWin Plasma 6.3+, wlroots 0.18+) is tried first,
//! zwlr-data-control-unstable-v1 (older KWin, wlroots) is the fallback.
//!
//! A single thread owns the Wayland connection for the life of the
//! process; the runner daemon never exits, so the "clipboard owner must
//! stay alive" requirement is satisfied naturally. Callers send commands
//! over a channel and get an ack; a self-pipe wakes the poll loop.
//!
//! Everything here is best-effort: any failure (no Wayland session, no
//! data-control global, no seat) marks the module unavailable and the
//! caller falls back to the Klipper/tool chain.

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::ExtDataControlOfferV1,
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};
use wayland_protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};

pub const HINT_MIME: &str = "x-kde-passwordManagerHint";
const TEXT_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
];

enum Cmd {
    SetSecret {
        text: String,
        clear_after: Option<Duration>,
        ack: Sender<bool>,
    },
}

pub struct SelectionHandle {
    tx: Sender<Cmd>,
    wake_write: Mutex<UnixStream>,
    dead: AtomicBool,
}

static HANDLE: OnceLock<SelectionHandle> = OnceLock::new();
static SPAWN_FAILED: AtomicBool = AtomicBool::new(false);

/// Set the clipboard to `text`, marked as a secret via
/// x-kde-passwordManagerHint. Returns false when Wayland/data-control is
/// unavailable, in which case the caller should use the fallback chain.
/// When clear_after is set, the selection thread clears the clipboard at
/// that time if (and only if) it still owns the selection.
pub fn set_secret(text: &str, clear_after: Option<Duration>) -> bool {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() || SPAWN_FAILED.load(Ordering::Relaxed) {
        return false;
    }
    let handle = HANDLE.get_or_init(|| {
        let (tx, rx) = channel::<Cmd>();
        let (wake_read, wake_write) = UnixStream::pair().expect("socketpair");
        let _ = wake_read.set_nonblocking(true);
        let _ = wake_write.set_nonblocking(true);
        std::thread::spawn(move || owner_thread(rx, wake_read));
        SelectionHandle {
            tx,
            wake_write: Mutex::new(wake_write),
            dead: AtomicBool::new(false),
        }
    });
    if handle.dead.load(Ordering::Relaxed) {
        return false;
    }
    let (ack_tx, ack_rx) = channel::<bool>();
    let cmd = Cmd::SetSecret {
        text: text.to_string(),
        clear_after,
        ack: ack_tx,
    };
    if handle.tx.send(cmd).is_err() {
        handle.dead.store(true, Ordering::Relaxed);
        return false;
    }
    if let Ok(mut w) = handle.wake_write.lock() {
        let _ = w.write_all(&[1]);
    }
    match ack_rx.recv_timeout(Duration::from_millis(750)) {
        Ok(ok) => ok,
        Err(_) => {
            handle.dead.store(true, Ordering::Relaxed);
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Owner thread
// ---------------------------------------------------------------------------

enum Manager {
    Ext(ExtDataControlManagerV1),
    Wlr(ZwlrDataControlManagerV1),
}

enum Device {
    Ext(ExtDataControlDeviceV1),
    Wlr(ZwlrDataControlDeviceV1),
}

enum Source {
    Ext(ExtDataControlSourceV1),
    Wlr(ZwlrDataControlSourceV1),
}

impl Source {
    fn destroy(&self) {
        match self {
            Source::Ext(s) => s.destroy(),
            Source::Wlr(s) => s.destroy(),
        }
    }
}

struct State {
    device: Device,
    source: Option<Source>,
    text: String,
    clear_deadline: Option<Instant>,
    /// False once the compositor tells us the source was cancelled
    /// (someone else took the selection, or we cleared it).
    owned: bool,
}

impl State {
    fn set_selection_none(&self) {
        match &self.device {
            Device::Ext(d) => d.set_selection(None),
            Device::Wlr(d) => d.set_selection(None),
        }
    }
    fn set_selection_source(&self, source: &Source) {
        match (&self.device, source) {
            (Device::Ext(d), Source::Ext(s)) => d.set_selection(Some(s)),
            (Device::Wlr(d), Source::Wlr(s)) => d.set_selection(Some(s)),
            _ => unreachable!("manager/device/source variants are created consistently"),
        }
    }
}

fn owner_thread(rx: Receiver<Cmd>, wake_read: UnixStream) {
    if let Err(e) = run_owner(&rx, wake_read) {
        eprintln!("[bitwarden] Wayland selection unavailable: {e}");
    }
    SPAWN_FAILED.store(true, Ordering::Relaxed);
    if let Some(h) = HANDLE.get() {
        h.dead.store(true, Ordering::Relaxed);
    }
    // Drain pending commands with a failure ack so callers do not hang.
    while let Ok(cmd) = rx.try_recv() {
        match cmd {
            Cmd::SetSecret { ack, .. } => {
                let _ = ack.send(false);
            }
        }
    }
}

fn run_owner(rx: &Receiver<Cmd>, wake_read: UnixStream) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<State>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();

    let seat = bind_seat(&globals, &qh)?;
    let (manager, device) = bind_manager_and_device(&globals, &qh, &seat)?;

    let mut state = State {
        device,
        source: None,
        text: String::new(),
        clear_deadline: None,
        owned: false,
    };
    run_loop(&conn, &mut queue, &qh, &mut state, &manager, rx, &wake_read)
}

fn bind_seat(globals: &GlobalList, qh: &QueueHandle<State>) -> Result<WlSeat, String> {
    globals
        .bind::<WlSeat, _, _>(qh, 1..=7, ())
        .map_err(|e| format!("no wl_seat: {e}"))
}

fn bind_manager_and_device(
    globals: &GlobalList,
    qh: &QueueHandle<State>,
    seat: &WlSeat,
) -> Result<(Manager, Device), String> {
    if let Ok(m) = globals.bind::<ExtDataControlManagerV1, _, _>(qh, 1..=1, ()) {
        let device = m.get_data_device(seat, qh, ());
        return Ok((Manager::Ext(m), Device::Ext(device)));
    }
    if let Ok(m) = globals.bind::<ZwlrDataControlManagerV1, _, _>(qh, 1..=2, ()) {
        let device = m.get_data_device(seat, qh, ());
        return Ok((Manager::Wlr(m), Device::Wlr(device)));
    }
    Err("compositor offers neither ext-data-control-v1 nor zwlr-data-control".into())
}

fn run_loop(
    conn: &Connection,
    queue: &mut EventQueue<State>,
    qh: &QueueHandle<State>,
    state: &mut State,
    manager: &Manager,
    rx: &Receiver<Cmd>,
    wake_read: &UnixStream,
) -> Result<(), String> {
    let wl_fd = conn.as_fd().as_raw_fd();
    let wake_fd = wake_read.as_fd().as_raw_fd();
    loop {
        queue
            .dispatch_pending(state)
            .map_err(|e| format!("dispatch: {e}"))?;
        let _ = conn.flush();

        // Apply any queued commands before sleeping.
        let mut had_cmd = false;
        while let Ok(cmd) = rx.try_recv() {
            had_cmd = true;
            apply_cmd(cmd, state, manager, qh, conn);
        }
        if had_cmd {
            continue;
        }

        // Auto-clear deadline.
        if let Some(deadline) = state.clear_deadline {
            if Instant::now() >= deadline {
                state.clear_deadline = None;
                if state.owned {
                    state.set_selection_none();
                    if let Some(src) = state.source.take() {
                        src.destroy();
                    }
                    state.owned = false;
                    let _ = conn.flush();
                }
                continue;
            }
        }

        let timeout_ms: i32 = match state.clear_deadline {
            Some(d) => d
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32,
            None => -1,
        };

        let Some(guard) = queue.prepare_read() else {
            // Events are already queued; loop back to dispatch them.
            continue;
        };
        let mut fds = [
            libc::pollfd {
                fd: wl_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if n > 0 && (fds[0].revents & libc::POLLIN) != 0 {
            guard.read().map_err(|e| format!("read: {e}"))?;
        } else {
            drop(guard);
        }
        if n > 0 && (fds[1].revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 64];
            use std::io::Read;
            let mut wr: &UnixStream = wake_read;
            let _ = wr.read(&mut buf);
        }
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("poll: {err}"));
            }
        }
    }
}

fn apply_cmd(
    cmd: Cmd,
    state: &mut State,
    manager: &Manager,
    qh: &QueueHandle<State>,
    conn: &Connection,
) {
    match cmd {
        Cmd::SetSecret {
            text,
            clear_after,
            ack,
        } => {
            // Replace any previous source.
            if let Some(old) = state.source.take() {
                old.destroy();
            }
            let source = match manager {
                Manager::Ext(m) => {
                    let s = m.create_data_source(qh, ());
                    for mime in TEXT_MIMES {
                        s.offer((*mime).to_string());
                    }
                    s.offer(HINT_MIME.to_string());
                    Source::Ext(s)
                }
                Manager::Wlr(m) => {
                    let s = m.create_data_source(qh, ());
                    for mime in TEXT_MIMES {
                        s.offer((*mime).to_string());
                    }
                    s.offer(HINT_MIME.to_string());
                    Source::Wlr(s)
                }
            };
            state.set_selection_source(&source);
            state.source = Some(source);
            state.text = text;
            state.owned = true;
            state.clear_deadline = clear_after.map(|d| Instant::now() + d);
            let _ = conn.flush();
            let _ = ack.send(true);
        }
    }
}

fn serve_send(state: &State, mime_type: &str, fd: OwnedFd) {
    let data: &[u8] = if mime_type == HINT_MIME {
        b"secret"
    } else {
        state.text.as_bytes()
    };
    let mut file = std::fs::File::from(fd);
    let _ = file.write_all(data);
    // fd closes on drop, which is the protocol's EOF signal.
}

// ---------------------------------------------------------------------------
// Dispatch implementations
// ---------------------------------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wayland_client::protocol::wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ignore ExtDataControlManagerV1);
delegate_noop!(State: ignore ExtDataControlOfferV1);
delegate_noop!(State: ignore ZwlrDataControlManagerV1);
delegate_noop!(State: ignore ZwlrDataControlOfferV1);

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => {
                serve_send(state, &mime_type, fd)
            }
            ext_data_control_source_v1::Event::Cancelled => {
                // Only the current source's cancellation ends ownership.
                // A replaced source is cancelled too, but by then a newer
                // source is already installed.
                use wayland_client::Proxy;
                let is_current = matches!(
                    &state.source,
                    Some(Source::Ext(cur)) if cur.id() == proxy.id()
                );
                if is_current {
                    state.owned = false;
                    state.source = None;
                    state.clear_deadline = None;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &ZwlrDataControlSourceV1,
        event: zwlr_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                serve_send(state, &mime_type, fd)
            }
            zwlr_data_control_source_v1::Event::Cancelled => {
                use wayland_client::Proxy;
                let is_current = matches!(
                    &state.source,
                    Some(Source::Wlr(cur)) if cur.id() == proxy.id()
                );
                if is_current {
                    state.owned = false;
                    state.source = None;
                    state.clear_deadline = None;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // We do not read other clients' offers. Destroy offer objects as
        // they arrive so they do not pile up.
        if let ext_data_control_device_v1::Event::DataOffer { id } = event {
            id.destroy();
        }
    }

    fn event_created_child(
        opcode: u16,
        qhandle: &QueueHandle<Self>,
    ) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        // opcode 0 is data_offer, the only object-creating event here.
        assert_eq!(opcode, 0);
        qhandle.make_data::<ExtDataControlOfferV1, _>(())
    }

}

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwlrDataControlDeviceV1,
        event: zwlr_data_control_device_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_device_v1::Event::DataOffer { id } = event {
            id.destroy();
        }
    }

    fn event_created_child(
        opcode: u16,
        qhandle: &QueueHandle<Self>,
    ) -> std::sync::Arc<dyn wayland_client::backend::ObjectData> {
        assert_eq!(opcode, 0);
        qhandle.make_data::<ZwlrDataControlOfferV1, _>(())
    }
}
