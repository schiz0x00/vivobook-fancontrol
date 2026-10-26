//! Talks to vfcd on a worker thread so the UI never blocks on EC reads.

use eframe::egui;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use vfc::proto::{Client, Request, Response, Status};

const POLL: Duration = Duration::from_millis(500);

pub struct Notice {
    pub at: Instant,
    pub ok: bool,
    pub text: String,
}

#[derive(Default)]
pub struct State {
    pub status: Option<Status>,
    pub connected: bool,
    pub error: Option<String>,
    pub notices: Vec<Notice>,
    /// Last explorer read: (start address, bytes).
    pub ec_read: Option<(u16, Vec<u8>)>,
    /// Bumped on every fresh table read.
    pub tables_gen: u64,
    pub busy: usize,
}

pub struct Backend {
    state: Arc<Mutex<State>>,
    tx: Sender<(Request, String)>,
}

impl Backend {
    pub fn start(ctx: egui::Context) -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let (tx, rx) = mpsc::channel::<(Request, String)>();
        let st = state.clone();
        thread::spawn(move || {
            let mut client: Option<Client> = None;
            let mut last_poll = Instant::now() - POLL;
            loop {
                let wait = POLL.saturating_sub(last_poll.elapsed());
                let cmd = match rx.recv_timeout(wait) {
                    Ok(c) => Some(c),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                };
                if let Some((req, what)) = cmd {
                    let res = call(&mut client, &req);
                    let mut s = lock(&st);
                    s.busy = s.busy.saturating_sub(1);
                    match res {
                        Ok(Response::Bytes(b)) => match req {
                            Request::ReadEc { addr, .. } => s.ec_read = Some((addr, b)),
                            _ => {
                                if let Some(status) = &mut s.status {
                                    status.tables = b;
                                }
                                s.tables_gen += 1;
                            }
                        },
                        Ok(Response::Message(m)) => notice(&mut s, true, format!("{what}: {m}")),
                        Ok(_) => notice(&mut s, true, format!("{what}: done")),
                        Err(e) => notice(&mut s, false, format!("{what}: {e:#}")),
                    }
                    // Show the effect right away.
                    last_poll = Instant::now() - POLL;
                }
                if last_poll.elapsed() >= POLL {
                    last_poll = Instant::now();
                    let res = call(&mut client, &Request::Status);
                    let mut s = lock(&st);
                    match res {
                        Ok(Response::Status(status)) => {
                            let tables_changed = s.status.as_ref().is_none_or(|o| o.tables != status.tables);
                            s.status = Some(*status);
                            s.connected = true;
                            s.error = None;
                            if tables_changed {
                                s.tables_gen += 1;
                            }
                        }
                        Ok(other) => s.error = Some(format!("unexpected reply {other:?}")),
                        Err(e) => {
                            s.connected = false;
                            s.error = Some(format!("{e:#}"));
                        }
                    }
                }
                ctx.request_repaint();
            }
        });
        Self { state, tx }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Queue a request; `what` labels the result notice.
    pub fn send(&self, req: Request, what: impl Into<String>) {
        lock(&self.state).busy += 1;
        let _ = self.tx.send((req, what.into()));
    }

    /// Queue a request whose success needs no notice (reads).
    pub fn read(&self, req: Request) {
        self.send(req, "read");
    }
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn notice(s: &mut State, ok: bool, text: String) {
    if ok && text.starts_with("read:") {
        return;
    }
    s.notices.push(Notice { at: Instant::now(), ok, text });
    if s.notices.len() > 20 {
        s.notices.remove(0);
    }
}

fn call(client: &mut Option<Client>, req: &Request) -> anyhow::Result<Response> {
    if client.is_none() {
        *client = Some(Client::connect()?);
    }
    let res = client.as_mut().unwrap().request(req);
    if let Err(e) = &res {
        // Daemon-side errors keep the connection; transport errors drop it.
        if e.downcast_ref::<std::io::Error>().is_some() || e.to_string().contains("closed") {
            *client = None;
        }
    }
    res
}
