//! A `Player` backed by a subprocess speaking the JSON-lines bot protocol.
//!
//! Design notes:
//! * A dedicated writer task owns stdin; `notify` and `request_action` push lines onto a bounded
//!   queue so a stalled bot can never block the table. If the queue is full, events are dropped
//!   (counted) and requests fail fast (the engine substitutes check/fold).
//! * A dedicated reader task owns stdout and parses every line; action responses are matched to
//!   requests by `request_id`, so a late answer to an earlier request is discarded rather than
//!   desynchronising the stream.
//! * The last few KB of stderr (and `log` messages) are retained for diagnostics.
//! * When the process exits, `is_alive()` flips to false and further requests fail immediately —
//!   a crashed bot costs no timeouts.

use crate::player::{Player, PlayerError};
use crate::protocol::{BotMessage, EngineMessage, PROTOCOL_VERSION};
use async_trait::async_trait;
use poker_utils::{Action, DecisionContext, LegalActions, PlayerId, PublicEvent};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};

pub const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB

#[derive(Clone, Debug)]
pub struct SpawnOptions {
    pub player_id: PlayerId,
    pub display_name: String,
    /// Free-form session identifier passed in the `hello` message.
    pub session: String,
    /// Bytes of stderr/log output retained for diagnostics.
    pub stderr_capture_bytes: usize,
    /// Optional file to which stderr is appended.
    pub stderr_log: Option<PathBuf>,
    /// Capacity of the outgoing message queue.
    pub outgoing_queue: usize,
    /// How long `shutdown` waits for a graceful exit after `goodbye` before killing.
    pub shutdown_grace: Duration,
    /// On Unix, also SIGKILL the child's process group when killing (use with `setsid` in
    /// `pre_exec` so helper processes started by the bot die with it).
    pub kill_process_group: bool,
}

impl SpawnOptions {
    pub fn new(player_id: PlayerId, display_name: impl Into<String>) -> Self {
        SpawnOptions {
            player_id,
            display_name: display_name.into(),
            session: "local".into(),
            stderr_capture_bytes: 8 * 1024,
            stderr_log: None,
            outgoing_queue: 4096,
            shutdown_grace: Duration::from_millis(300),
            kill_process_group: false,
        }
    }

    pub fn session(mut self, session: impl Into<String>) -> Self {
        self.session = session.into();
        self
    }

    pub fn stderr_log(mut self, path: PathBuf) -> Self {
        self.stderr_log = Some(path);
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("failed to spawn bot process: {0}")]
    Io(#[from] std::io::Error),
    #[error("bot process has no {0} pipe")]
    MissingPipe(&'static str),
}

#[derive(Default, Debug)]
struct Diagnostics {
    stderr_tail: VecDeque<u8>,
    invalid_lines: u64,
    last_invalid_line: Option<String>,
    dropped_events: u64,
}

pub struct ProcessBot {
    player_id: PlayerId,
    display_name: String,
    outgoing: mpsc::Sender<String>,
    responses: Mutex<mpsc::Receiver<BotMessage>>,
    alive: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    diagnostics: Arc<StdMutex<Diagnostics>>,
    stderr_capacity: usize,
    next_request: AtomicU64,
    shutdown_grace: Duration,
    kill_process_group: bool,
    pid: Option<u32>,
}

impl ProcessBot {
    /// Spawn `program args...` in the current directory.
    pub async fn spawn(
        program: &str,
        args: &[String],
        opts: SpawnOptions,
    ) -> Result<Self, SpawnError> {
        let mut cmd = Command::new(program);
        cmd.args(args);
        Self::spawn_command(cmd, opts).await
    }

    /// Spawn from a fully configured `Command` (cwd, env, rlimits via `pre_exec`, docker...).
    pub async fn spawn_command(mut cmd: Command, opts: SpawnOptions) -> Result<Self, SpawnError> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdin = child.stdin.take().ok_or(SpawnError::MissingPipe("stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or(SpawnError::MissingPipe("stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or(SpawnError::MissingPipe("stderr"))?;

        let alive = Arc::new(AtomicBool::new(true));
        let diagnostics = Arc::new(StdMutex::new(Diagnostics::default()));
        let (out_tx, mut out_rx) = mpsc::channel::<String>(opts.outgoing_queue.max(16));
        let (resp_tx, resp_rx) = mpsc::channel::<BotMessage>(256);

        // Writer task.
        {
            let alive = Arc::clone(&alive);
            let mut stdin = stdin;
            tokio::spawn(async move {
                let mut buf: Vec<u8> = Vec::with_capacity(8192);
                while let Some(line) = out_rx.recv().await {
                    buf.clear();
                    buf.extend_from_slice(line.as_bytes());
                    buf.push(b'\n');
                    // Coalesce whatever else is already queued into the same write.
                    while buf.len() < 64 * 1024 {
                        match out_rx.try_recv() {
                            Ok(more) => {
                                buf.extend_from_slice(more.as_bytes());
                                buf.push(b'\n');
                            }
                            Err(_) => break,
                        }
                    }
                    if stdin.write_all(&buf).await.is_err() || stdin.flush().await.is_err() {
                        alive.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            });
        }

        // Reader task (stdout).
        {
            let alive = Arc::clone(&alive);
            let diagnostics = Arc::clone(&diagnostics);
            let cap = opts.stderr_capture_bytes;
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut buf: Vec<u8> = Vec::with_capacity(4096);
                loop {
                    buf.clear();
                    match read_line_limited(&mut reader, &mut buf, MAX_LINE_BYTES).await {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(_) => break,
                    }
                    let text = String::from_utf8_lossy(&buf);
                    let text = text.trim();
                    if text.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<BotMessage>(text) {
                        Ok(BotMessage::Log { message }) => {
                            let mut d = diagnostics.lock().unwrap();
                            append_tail(
                                &mut d.stderr_tail,
                                format!("[log] {}\n", message).as_bytes(),
                                cap,
                            );
                        }
                        Ok(msg) => {
                            // Drop if the engine is not consuming (unsolicited spam).
                            let _ = resp_tx.try_send(msg);
                        }
                        Err(_) => {
                            let mut d = diagnostics.lock().unwrap();
                            d.invalid_lines += 1;
                            let mut s = text.to_string();
                            s.truncate(512);
                            d.last_invalid_line = Some(s);
                        }
                    }
                }
                alive.store(false, Ordering::SeqCst);
            });
        }

        // Stderr task.
        {
            let diagnostics = Arc::clone(&diagnostics);
            let cap = opts.stderr_capture_bytes;
            let log_path = opts.stderr_log.clone();
            tokio::spawn(async move {
                let mut file = match log_path {
                    Some(p) => tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(p)
                        .await
                        .ok(),
                    None => None,
                };
                let mut stderr = stderr;
                let mut chunk = [0u8; 4096];
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            {
                                let mut d = diagnostics.lock().unwrap();
                                append_tail(&mut d.stderr_tail, &chunk[..n], cap);
                            }
                            if let Some(f) = file.as_mut() {
                                let _ = f.write_all(&chunk[..n]).await;
                            }
                        }
                    }
                }
            });
        }

        let bot = ProcessBot {
            player_id: opts.player_id,
            display_name: opts.display_name,
            outgoing: out_tx,
            responses: Mutex::new(resp_rx),
            alive,
            child: Arc::new(Mutex::new(Some(child))),
            diagnostics,
            stderr_capacity: opts.stderr_capture_bytes,
            next_request: AtomicU64::new(1),
            shutdown_grace: opts.shutdown_grace,
            kill_process_group: opts.kill_process_group,
            pid,
        };
        bot.send(&EngineMessage::Hello {
            protocol_version: PROTOCOL_VERSION.to_string(),
            player_id: opts.player_id,
            session: opts.session,
        });
        Ok(bot)
    }

    fn send(&self, msg: &EngineMessage) -> bool {
        let line = match serde_json::to_string(msg) {
            Ok(l) => l,
            Err(_) => return false,
        };
        self.send_line(line)
    }

    fn send_line(&self, line: String) -> bool {
        match self.outgoing.try_send(line) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                let mut d = self.diagnostics.lock().unwrap();
                d.dropped_events += 1;
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.alive.store(false, Ordering::SeqCst);
                false
            }
        }
    }

    /// Last captured stderr / log output.
    pub fn stderr_tail(&self) -> String {
        let d = self.diagnostics.lock().unwrap();
        let (a, b) = d.stderr_tail.as_slices();
        let mut v = Vec::with_capacity(a.len() + b.len());
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        String::from_utf8_lossy(&v).into_owned()
    }

    pub fn invalid_line_count(&self) -> u64 {
        self.diagnostics.lock().unwrap().invalid_lines
    }

    pub fn last_invalid_line(&self) -> Option<String> {
        self.diagnostics.lock().unwrap().last_invalid_line.clone()
    }

    pub fn dropped_event_count(&self) -> u64 {
        self.diagnostics.lock().unwrap().dropped_events
    }

    /// Kill the process immediately and reap it.
    pub async fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        let mut guard = self.child.lock().await;
        if let Some(child) = guard.as_mut() {
            #[cfg(unix)]
            if self.kill_process_group {
                if let Some(pid) = self.pid {
                    // SAFETY: plain syscall; a negative pid targets the process group.
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        *guard = None;
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Wait up to `timeout` for the process to exit on its own.
    pub async fn wait_exit(&self, timeout: Duration) -> Option<std::process::ExitStatus> {
        let mut guard = self.child.lock().await;
        let child = guard.as_mut()?;
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => {
                self.alive.store(false, Ordering::SeqCst);
                Some(status)
            }
            _ => None,
        }
    }

    pub fn stderr_capacity(&self) -> usize {
        self.stderr_capacity
    }
}

#[async_trait]
impl Player for ProcessBot {
    fn player_id(&self) -> PlayerId {
        self.player_id
    }

    fn display_name(&self) -> String {
        self.display_name.clone()
    }

    async fn notify(&self, event: &PublicEvent) {
        if !self.alive.load(Ordering::SeqCst) {
            return;
        }
        self.send(&EngineMessage::NotifyEvent {
            event: event.clone(),
        });
    }

    async fn notify_batch(&self, events: &[PublicEvent]) {
        if events.is_empty() || !self.alive.load(Ordering::SeqCst) {
            return;
        }
        // One queue message (and typically one write syscall) for the whole burst.
        let mut buf = String::with_capacity(events.len() * 128);
        for (i, e) in events.iter().enumerate() {
            if i > 0 {
                buf.push('\n');
            }
            buf.push_str(r#"{"type":"notify_event","event":"#);
            match serde_json::to_string(e) {
                Ok(j) => buf.push_str(&j),
                Err(_) => return,
            }
            buf.push('}');
        }
        self.send_line(buf);
    }

    async fn request_action(
        &self,
        ctx: &DecisionContext,
        legal: &LegalActions,
        timeout_ms: u64,
    ) -> Result<Action, PlayerError> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(PlayerError::Dead);
        }
        let n = self.next_request.fetch_add(1, Ordering::SeqCst);
        let request_id = format!("req_{}_{}", self.player_id, n);
        let msg = EngineMessage::RequestAction {
            request_id: request_id.clone(),
            deadline_ms: timeout_ms,
            context: ctx.clone(),
            legal: legal.clone(),
        };
        if !self.send(&msg) {
            return Err(if self.alive.load(Ordering::SeqCst) {
                PlayerError::CommunicationFailed("outgoing queue full".into())
            } else {
                PlayerError::Dead
            });
        }
        let mut rx = self.responses.lock().await;
        let wait = async {
            loop {
                match rx.recv().await {
                    Some(BotMessage::Action {
                        request_id: rid,
                        action,
                    }) if rid == request_id => {
                        return Ok(action);
                    }
                    Some(_) => continue, // stale or irrelevant
                    None => return Err(PlayerError::Dead),
                }
            }
        };
        match tokio::time::timeout(Duration::from_millis(timeout_ms), wait).await {
            Ok(r) => r,
            Err(_) => Err(PlayerError::Timeout),
        }
    }

    async fn shutdown(&self) {
        if self.alive.load(Ordering::SeqCst) {
            self.send(&EngineMessage::Goodbye);
            if self.wait_exit(self.shutdown_grace).await.is_some() {
                return;
            }
        }
        self.kill().await;
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

fn append_tail(tail: &mut VecDeque<u8>, bytes: &[u8], cap: usize) {
    tail.extend(bytes.iter().copied());
    while tail.len() > cap {
        tail.pop_front();
    }
}

/// Read one line (up to and including `\n`) into `buf`, discarding overlong lines. Returns the
/// number of bytes read (0 at EOF).
async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<usize> {
    let mut total = 0usize;
    let mut discarding = false;
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(total);
        }
        let (consumed, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if !discarding {
            if buf.len() + consumed > limit {
                buf.clear();
                discarding = true;
            } else {
                buf.extend_from_slice(&available[..consumed]);
            }
        }
        total += consumed;
        reader.consume(consumed);
        if done {
            if discarding {
                buf.clear();
                buf.extend_from_slice(b"{\"type\":\"__overlong__\"}\n");
            }
            return Ok(total);
        }
    }
}
