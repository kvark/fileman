//! SSH transport built on sunset.
//!
//! The rest of the app is thread-per-operation and blocking, while sunset is
//! async and wants a single task driving one connection. This module owns that
//! mismatch: each host gets a thread running a current-thread tokio runtime,
//! and [`Conn`] is a blocking handle that ships [`Job`]s to it and waits for
//! the reply. Nothing above this module sees a future.
//!
//! Two rules of sunset shape the design:
//!
//! - A channel is only usable once its `SessionOpened` event has been answered
//!   with the subsystem or command it should run, so channels are parked in
//!   `pending` until the progress loop hands them over.
//! - Every channel half, stderr included, has to be drained or the whole
//!   session stalls once the peer fills that pipe.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use embassy_futures::select::{Either, select};
use embassy_sync::channel::Channel as EmbassyChannel;
use sunset::{CliEvent, SignKey};
use sunset_async::{ChanIn, ChanInOut, ProgressHolder, SSHClient, SunsetRawMutex};
use sunset_sftp::client::{RemoteHandle, SftpClient};
use sunset_sftp::embedded_io_async::Read as _;
use sunset_sftp::protocol::Attrs;
use tokio::sync::mpsc as tmpsc;

pub mod knownhosts;

/// Encoding buffer for the SFTP client. Sized for long paths and long name
/// replies rather than the embedded default.
const SFTP_BUF: usize = 8192;
/// Read/write chunk for file transfers. The client splits this into protocol
/// sized requests and pipelines them, so a large chunk costs about one round
/// trip instead of one per request.
pub const CHUNK: usize = 256 * 1024;
/// Bound on a buffered capture, so a runaway command cannot exhaust memory.
const MAX_EXEC_OUTPUT: usize = 64 * 1024 * 1024;

/// A remote file or directory handle, addressed by id so the sync side never
/// holds a borrow into the connection task.
pub type HandleId = u64;

/// Metadata for one directory entry, decoupled from sunset's borrowed types.
#[derive(Debug, Clone)]
pub struct DirItem {
    pub name: String,
    pub attrs: FileAttrs,
}

/// The subset of SFTP attributes the app uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct FileAttrs {
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub mtime: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl FileAttrs {
    fn from(a: &Attrs) -> Self {
        Self {
            size: a.size,
            permissions: a.permissions,
            mtime: a.mtime,
            uid: a.uid,
            gid: a.gid,
        }
    }

    fn file_type(&self) -> u32 {
        self.permissions.unwrap_or(0) & 0o170000
    }

    pub fn is_dir(&self) -> bool {
        self.file_type() == 0o040000
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type() == 0o120000
    }
}

/// How a remote file should be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    Read,
    Write,
}

/// An error from the remote side.
///
/// `fatal` marks the ones that mean the connection itself is gone, which the
/// app uses to decide whether to drop the session rather than just report the
/// operation as failed.
#[derive(Debug, Clone)]
pub struct SshError {
    pub message: String,
    pub fatal: bool,
}

impl SshError {
    pub fn op(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
        }
    }

    pub fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: true,
        }
    }
}

impl std::fmt::Display for SshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SshError {}

pub type SshResult<T> = Result<T, SshError>;

type Reply<T> = mpsc::SyncSender<SshResult<T>>;

/// One unit of work for the connection task.
///
/// Each carries the sender its result goes back on; the caller blocks on the
/// matching receiver.
enum Job {
    Realpath(String, Reply<String>),
    Stat {
        path: String,
        follow: bool,
        reply: Reply<FileAttrs>,
    },
    SetStat {
        path: String,
        attrs: FileAttrs,
        reply: Reply<()>,
    },
    ReadLink(String, Reply<String>),
    Symlink {
        target: String,
        link: String,
        reply: Reply<()>,
    },
    OpenDir(String, Reply<HandleId>),
    /// One server batch of directory entries. `None` means the listing ended.
    ReadDir(HandleId, Reply<Option<Vec<DirItem>>>),
    Open {
        path: String,
        mode: OpenMode,
        reply: Reply<HandleId>,
    },
    Read {
        handle: HandleId,
        offset: u64,
        len: usize,
        reply: Reply<Vec<u8>>,
    },
    Write {
        handle: HandleId,
        offset: u64,
        data: Vec<u8>,
        reply: Reply<()>,
    },
    Close(HandleId, Reply<()>),
    MkDir(String, Reply<()>),
    RmDir(String, Reply<()>),
    Remove(String, Reply<()>),
    Rename {
        from: String,
        to: String,
        reply: Reply<()>,
    },
    /// Run a command and capture its output.
    Exec {
        cmd: String,
        reply: Reply<ExecOutput>,
    },
    /// Run a command, streaming stdin in and stdout out so a large transfer
    /// does not have to be buffered whole. The reply carries its stderr.
    ExecStream {
        cmd: String,
        stdin: Option<tmpsc::Receiver<Vec<u8>>>,
        stdout: tmpsc::Sender<io::Result<Vec<u8>>>,
        reply: Reply<Vec<u8>>,
    },
}

/// Captured result of a command run over exec.
#[derive(Debug, Clone, Default)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// A live connection to one host.
///
/// Dropping the handle shuts the connection thread down.
pub struct Conn {
    tx: tmpsc::UnboundedSender<Job>,
    alive: Arc<AtomicBool>,
    pub host: String,
    pub home_dir: Option<String>,
}

impl Conn {
    /// True while the connection task is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Sends a job and blocks for its reply.
    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Job) -> SshResult<T> {
        // Capacity one, so the connection task never blocks handing a reply
        // back to a caller that has given up.
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if self.tx.send(make(reply_tx)).is_err() {
            self.alive.store(false, Ordering::Relaxed);
            return Err(SshError::fatal("SSH connection closed"));
        }
        match reply_rx.recv() {
            Ok(r) => {
                if let Err(ref e) = r
                    && e.fatal
                {
                    self.alive.store(false, Ordering::Relaxed);
                }
                r
            }
            Err(_) => {
                self.alive.store(false, Ordering::Relaxed);
                Err(SshError::fatal("SSH connection closed"))
            }
        }
    }

    pub fn realpath(&self, path: &str) -> SshResult<String> {
        self.call(|reply| Job::Realpath(path.to_string(), reply))
    }

    pub fn stat(&self, path: &str) -> SshResult<FileAttrs> {
        self.call(|reply| Job::Stat {
            path: path.to_string(),
            follow: true,
            reply,
        })
    }

    pub fn lstat(&self, path: &str) -> SshResult<FileAttrs> {
        self.call(|reply| Job::Stat {
            path: path.to_string(),
            follow: false,
            reply,
        })
    }

    pub fn set_stat(&self, path: &str, attrs: FileAttrs) -> SshResult<()> {
        self.call(|reply| Job::SetStat {
            path: path.to_string(),
            attrs,
            reply,
        })
    }

    pub fn readlink(&self, path: &str) -> SshResult<String> {
        self.call(|reply| Job::ReadLink(path.to_string(), reply))
    }

    pub fn symlink(&self, target: &str, link: &str) -> SshResult<()> {
        self.call(|reply| Job::Symlink {
            target: target.to_string(),
            link: link.to_string(),
            reply,
        })
    }

    pub fn open_dir(&self, path: &str) -> SshResult<HandleId> {
        self.call(|reply| Job::OpenDir(path.to_string(), reply))
    }

    pub fn read_dir(&self, handle: HandleId) -> SshResult<Option<Vec<DirItem>>> {
        self.call(|reply| Job::ReadDir(handle, reply))
    }

    pub fn open(&self, path: &str, mode: OpenMode) -> SshResult<HandleId> {
        self.call(|reply| Job::Open {
            path: path.to_string(),
            mode,
            reply,
        })
    }

    pub fn read_at(&self, handle: HandleId, offset: u64, len: usize) -> SshResult<Vec<u8>> {
        self.call(|reply| Job::Read {
            handle,
            offset,
            len,
            reply,
        })
    }

    pub fn write_at(&self, handle: HandleId, offset: u64, data: Vec<u8>) -> SshResult<()> {
        self.call(|reply| Job::Write {
            handle,
            offset,
            data,
            reply,
        })
    }

    pub fn close(&self, handle: HandleId) -> SshResult<()> {
        self.call(|reply| Job::Close(handle, reply))
    }

    pub fn mkdir(&self, path: &str) -> SshResult<()> {
        self.call(|reply| Job::MkDir(path.to_string(), reply))
    }

    pub fn rmdir(&self, path: &str) -> SshResult<()> {
        self.call(|reply| Job::RmDir(path.to_string(), reply))
    }

    pub fn remove(&self, path: &str) -> SshResult<()> {
        self.call(|reply| Job::Remove(path.to_string(), reply))
    }

    pub fn rename(&self, from: &str, to: &str) -> SshResult<()> {
        self.call(|reply| Job::Rename {
            from: from.to_string(),
            to: to.to_string(),
            reply,
        })
    }

    /// Runs a command, returning its captured output.
    pub fn exec(&self, cmd: &str) -> SshResult<ExecOutput> {
        self.call(|reply| Job::Exec {
            cmd: cmd.to_string(),
            reply,
        })
    }

    /// Starts a command, streaming its stdout through the returned reader.
    ///
    /// With [`Stdin::Piped`] the stream is also an [`io::Write`] feeding the
    /// command's input; [`ExecStream::finish_input`] then signals end of input,
    /// which is what `tar xf -` waits for.
    pub fn exec_stream(&self, cmd: &str, stdin: Stdin) -> SshResult<ExecStream> {
        let (out_tx, out_rx) = tmpsc::channel(4);
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        let (in_tx, in_rx) = match stdin {
            Stdin::Piped => {
                let (t, r) = tmpsc::channel(4);
                (Some(t), Some(r))
            }
            Stdin::Closed => (None, None),
        };
        let job = Job::ExecStream {
            cmd: cmd.to_string(),
            stdin: in_rx,
            stdout: out_tx,
            reply: done_tx,
        };
        if self.tx.send(job).is_err() {
            self.alive.store(false, Ordering::Relaxed);
            return Err(SshError::fatal("SSH connection closed"));
        }
        Ok(ExecStream {
            stdout: out_rx,
            stdin: in_tx,
            done: done_rx,
            pending: Vec::new(),
            pos: 0,
        })
    }
}

/// Whether a streamed command is given an input pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stdin {
    Piped,
    Closed,
}

/// The local end of a streamed command.
pub struct ExecStream {
    stdout: tmpsc::Receiver<io::Result<Vec<u8>>>,
    stdin: Option<tmpsc::Sender<Vec<u8>>>,
    done: mpsc::Receiver<SshResult<Vec<u8>>>,
    pending: Vec<u8>,
    pos: usize,
}

impl ExecStream {
    /// Signals end of input, so a command reading its stdin to EOF can finish.
    pub fn finish_input(&mut self) {
        self.stdin = None;
    }

    /// Waits for the command to finish, returning whatever it wrote to stderr.
    ///
    /// Any stdout still buffered is drained first: the connection task blocks
    /// while handing output over, so waiting without draining would deadlock.
    pub fn wait(mut self) -> SshResult<Vec<u8>> {
        self.stdin = None;
        while let Some(chunk) = self.stdout.blocking_recv() {
            if matches!(chunk, Ok(ref c) if c.is_empty()) {
                break;
            }
        }
        self.done
            .recv()
            .unwrap_or_else(|_| Err(SshError::fatal("SSH connection closed")))
    }
}

impl io::Read for ExecStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos == self.pending.len() {
            match self.stdout.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.pending = chunk;
                    self.pos = 0;
                    if self.pending.is_empty() {
                        return Ok(0);
                    }
                }
                Some(Err(e)) => return Err(e),
                // The task dropped the sender: end of output.
                None => return Ok(0),
            }
        }
        let n = (self.pending.len() - self.pos).min(buf.len());
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl io::Write for ExecStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let tx = self
            .stdin
            .as_ref()
            .ok_or_else(|| io::Error::other("exec stdin is closed"))?;
        tx.blocking_send(buf.to_vec())
            .map_err(|_| io::Error::other("exec stdin closed by remote"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A remote file opened for reading, presented as a seekable byte stream.
///
/// SFTP reads are positional, so seeking is just bookkeeping.
pub struct RemoteFile {
    conn: Arc<Conn>,
    handle: HandleId,
    offset: u64,
    size: Option<u64>,
}

impl RemoteFile {
    pub fn open(conn: Arc<Conn>, path: &str) -> SshResult<Self> {
        let size = conn.stat(path).ok().and_then(|a| a.size);
        let handle = conn.open(path, OpenMode::Read)?;
        Ok(Self {
            conn,
            handle,
            offset: 0,
            size,
        })
    }

    pub fn size(&self) -> Option<u64> {
        self.size
    }
}

impl io::Read for RemoteFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let data = self
            .conn
            .read_at(self.handle, self.offset, buf.len().min(CHUNK))
            .map_err(io::Error::other)?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        self.offset += n as u64;
        Ok(n)
    }
}

impl io::Seek for RemoteFile {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let (base, delta) = match pos {
            io::SeekFrom::Start(n) => {
                self.offset = n;
                return Ok(n);
            }
            io::SeekFrom::Current(d) => (self.offset as i64, d),
            io::SeekFrom::End(d) => {
                let size = self
                    .size
                    .ok_or_else(|| io::Error::other("remote file size is unknown"))?;
                (size as i64, d)
            }
        };
        let target = base.checked_add(delta).unwrap_or(0);
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start of file",
            ));
        }
        self.offset = target as u64;
        Ok(self.offset)
    }
}

impl Drop for RemoteFile {
    fn drop(&mut self) {
        let _ = self.conn.close(self.handle);
    }
}

/// Everything needed to open one connection.
pub struct ConnectParams {
    /// Name the user typed, used for known-hosts and display.
    pub host: String,
    /// Address actually dialled, after ssh_config resolution.
    pub hostname: String,
    pub port: u16,
    pub user: String,
    /// Private key files to try, in order.
    pub identity_files: Vec<String>,
    /// Whether to offer keys held by a running ssh-agent.
    pub use_agent: bool,
}

/// Opens a connection, blocking until authentication completes.
pub fn connect(params: ConnectParams) -> SshResult<Conn> {
    let (job_tx, job_rx) = tmpsc::unbounded_channel::<Job>();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<SshResult<Option<String>>>(1);
    let alive = Arc::new(AtomicBool::new(true));

    let host = params.host.clone();
    let thread_alive = alive.clone();
    std::thread::Builder::new()
        .name(format!("ssh-{host}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(SshError::fatal(format!(
                        "tokio runtime for {}: {e}",
                        params.host
                    ))));
                    return;
                }
            };
            rt.block_on(run_connection(params, job_rx, ready_tx));
            thread_alive.store(false, Ordering::Relaxed);
        })
        .map_err(|e| SshError::fatal(format!("spawning SSH thread: {e}")))?;

    let home_dir = ready_rx
        .recv()
        .map_err(|_| SshError::fatal("SSH connection thread stopped"))??;

    Ok(Conn {
        tx: job_tx,
        alive,
        host,
        home_dir,
    })
}

/// What a freshly opened session channel should be asked to run.
#[derive(Clone, Debug)]
enum ChanKind {
    Sftp,
    Exec(String),
}

/// A session channel with its stderr half, parked until `SessionOpened`.
type Opened<'g> = (ChanKind, ChanInOut<'g>, Option<ChanIn<'g>>);
type Pending<'g> = RefCell<HashMap<u32, Opened<'g>>>;
type Ready = Cell<Option<mpsc::SyncSender<SshResult<Option<String>>>>>;

async fn run_connection(
    params: ConnectParams,
    job_rx: tmpsc::UnboundedReceiver<Job>,
    ready_tx: mpsc::SyncSender<SshResult<Option<String>>>,
) {
    let addr = format!("{}:{}", params.hostname, params.port);
    let stream = match tokio::net::TcpStream::connect(&addr).await {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(SshError::fatal(format!("TCP connect to {addr}: {e}"))));
            return;
        }
    };
    // SFTP alternates a small request with a large reply. Without this, Nagle
    // holds each request back until the peer's delayed ACK.
    let _ = stream.set_nodelay(true);
    let mut stream = stream;
    let (mut rsock, mut wsock) = stream.split();

    let ssh = SSHClient::new_owned();
    let ssh_fut = ssh.run_tokio(&mut rsock, &mut wsock);

    let opened: EmbassyChannel<SunsetRawMutex, Opened, 4> = EmbassyChannel::new();
    let pending: Pending = RefCell::new(HashMap::new());
    let ready: Ready = Cell::new(Some(ready_tx));

    let session = async {
        let driver = drive_events(&ssh, &params, &opened, &pending);
        let worker = serve_jobs(&ssh, &opened, &pending, job_rx, &ready);
        match select(driver, worker).await {
            Either::First(r) | Either::Second(r) => r,
        }
    };

    let outcome = match select(ssh_fut, session).await {
        Either::First(Ok(())) => Err(SshError::fatal("SSH connection closed")),
        Either::First(Err(e)) => Err(SshError::fatal(format!("SSH connection: {e}"))),
        Either::Second(r) => r,
    };

    // If we never got as far as reporting readiness, the caller is still
    // blocked on it.
    if let Some(tx) = ready.take() {
        let _ = tx.send(match outcome {
            Err(e) => Err(e),
            Ok(()) => Err(SshError::fatal("SSH session ended before authentication")),
        });
    }
}

/// Answers authentication and channel-open events for the life of the session.
async fn drive_events<'g>(
    ssh: &'g SSHClient<'static>,
    params: &ConnectParams,
    opened: &EmbassyChannel<SunsetRawMutex, Opened<'g>, 4>,
    pending: &Pending<'g>,
) -> SshResult<()> {
    let mut keys = load_identities(&params.identity_files);
    #[cfg(unix)]
    let mut agent = if params.use_agent {
        agent::load(&mut keys).await
    } else {
        None
    };

    loop {
        let mut ph = ProgressHolder::new();
        let ev = match ssh.progress(&mut ph).await {
            Ok(ev) => ev,
            Err(e) => return Err(SshError::fatal(format!("SSH session: {e}"))),
        };
        match ev {
            CliEvent::Hostkey(h) => {
                let key = h
                    .hostkey()
                    .map_err(|e| SshError::fatal(format!("reading host key: {e}")))?;
                match knownhosts::verify(&params.host, params.port, &key) {
                    Ok(()) => h
                        .accept()
                        .map_err(|e| SshError::fatal(format!("accepting host key: {e}")))?,
                    Err(e) => {
                        let _ = h.reject();
                        return Err(SshError::fatal(e));
                    }
                }
            }
            CliEvent::Username(u) => u
                .username(&params.user)
                .map_err(|e| SshError::fatal(format!("sending username: {e}")))?,
            CliEvent::Password(p) => {
                // There is no terminal to prompt on, so skip rather than hang.
                p.skip()
                    .map_err(|e| SshError::fatal(format!("skipping password auth: {e}")))?
            }
            CliEvent::Pubkey(p) => {
                let r = match keys.pop() {
                    Some(k) => p.pubkey(k),
                    None => p.skip(),
                };
                r.map_err(|e| SshError::fatal(format!("offering public key: {e}")))?
            }
            CliEvent::AgentSign(req) => {
                #[cfg(unix)]
                {
                    let a = agent.as_mut().ok_or_else(|| {
                        SshError::fatal("agent signature requested without an agent")
                    })?;
                    let key = req
                        .key()
                        .map_err(|e| SshError::fatal(format!("agent key: {e}")))?;
                    let msg = req
                        .message()
                        .map_err(|e| SshError::fatal(format!("agent message: {e}")))?;
                    let sig = a
                        .sign_auth(key, &msg)
                        .await
                        .map_err(|e| SshError::fatal(format!("agent signing: {e}")))?;
                    req.signed(&sig)
                        .map_err(|e| SshError::fatal(format!("agent signature: {e}")))?;
                }
                #[cfg(not(unix))]
                {
                    let _ = req;
                    return Err(SshError::fatal(
                        "ssh-agent is not supported on this platform",
                    ));
                }
            }
            CliEvent::Authenticated => {
                // Dropped so the client is usable for opening a channel.
                drop(ph);
                let (io, err) = ssh
                    .open_session_nopty()
                    .await
                    .map_err(|e| SshError::fatal(format!("opening SFTP channel: {e}")))?;
                pending
                    .borrow_mut()
                    .insert(io.num().0, (ChanKind::Sftp, io, Some(err)));
            }
            CliEvent::SessionOpened(mut opener) => {
                let num = opener.channel().0;
                let entry = pending.borrow_mut().remove(&num);
                let Some((kind, io, err)) = entry else {
                    continue;
                };
                let r = match kind {
                    ChanKind::Sftp => opener.subsystem("sftp"),
                    ChanKind::Exec(ref cmd) => opener.exec(cmd),
                };
                r.map_err(|e| SshError::fatal(format!("channel request: {e}")))?;
                // Only usable once the request above has been sent.
                opened.send((kind, io, err)).await;
            }
            CliEvent::SessionExit(_) | CliEvent::Banner(_) | CliEvent::PollAgain => (),
            CliEvent::Defunct => return Err(SshError::fatal("SSH connection closed")),
        }
    }
}

/// Runs the SFTP subsystem and serves jobs until the handle is dropped.
async fn serve_jobs<'g>(
    ssh: &'g SSHClient<'static>,
    opened: &EmbassyChannel<SunsetRawMutex, Opened<'g>, 4>,
    pending: &Pending<'g>,
    mut job_rx: tmpsc::UnboundedReceiver<Job>,
    ready: &Ready,
) -> SshResult<()> {
    let (_kind, io, err) = opened.receive().await;
    let (rx, tx) = io.split();
    let mut client: SftpClient<_, _, SFTP_BUF> = SftpClient::new(rx, tx);

    let work = async {
        client
            .init()
            .await
            .map_err(|e| SshError::fatal(format!("SFTP handshake: {e}")))?;

        let mut buf = [0u8; SFTP_BUF];
        let home = client
            .realpath(".", &mut buf)
            .await
            .ok()
            .map(|p| String::from_utf8_lossy(p).into_owned());

        if let Some(tx) = ready.take() {
            let _ = tx.send(Ok(home));
        }

        let mut state = Handles::default();
        while let Some(job) = job_rx.recv().await {
            run_job(&mut client, ssh, opened, pending, job, &mut state).await;
        }
        // Every handle dropped: the app is done with this host.
        Ok(())
    };

    // The subsystem's stderr still has to be drained or the session stalls.
    match select(work, drain(err)).await {
        Either::First(r) => r,
        Either::Second(_) => Err(SshError::fatal("SFTP channel closed")),
    }
}

/// Open remote handles, keyed by the id the sync side holds.
#[derive(Default)]
struct Handles {
    map: HashMap<HandleId, RemoteHandle>,
    next: HandleId,
}

impl Handles {
    fn insert(&mut self, h: RemoteHandle) -> HandleId {
        self.next += 1;
        self.map.insert(self.next, h);
        self.next
    }

    fn get(&self, id: HandleId) -> SshResult<&RemoteHandle> {
        self.map
            .get(&id)
            .ok_or_else(|| SshError::op("stale remote handle"))
    }
}

/// Maps an SFTP-level failure onto our error type.
///
/// Anything that is not a status reply from the server means the transport is
/// in doubt, so it is reported as fatal and the session gets torn down.
fn sftp_err(op: &str, e: sunset_sftp::error::SftpError) -> SshError {
    use sunset_sftp::error::SftpError;
    let message = format!("{op}: {e}");
    match e {
        // A status reply is the server refusing this one request (no such
        // file, permission denied); the session itself is fine.
        SftpError::FileServerError(_) | SftpError::NoRoom | SftpError::BadHandle => {
            SshError::op(message)
        }
        // Anything else means the transport or the protocol state is in doubt.
        _ => SshError::fatal(message),
    }
}

async fn run_job<'g, R, W>(
    client: &mut SftpClient<R, W, SFTP_BUF>,
    ssh: &'g SSHClient<'static>,
    opened: &EmbassyChannel<SunsetRawMutex, Opened<'g>, 4>,
    pending: &Pending<'g>,
    job: Job,
    state: &mut Handles,
) where
    R: sunset_sftp::embedded_io_async::Read,
    W: sunset_sftp::embedded_io_async::Write,
{
    match job {
        Job::Realpath(path, reply) => {
            let mut buf = [0u8; SFTP_BUF];
            let r = client
                .realpath(&path, &mut buf)
                .await
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .map_err(|e| sftp_err(&format!("realpath {path}"), e));
            let _ = reply.send(r);
        }
        Job::Stat {
            path,
            follow,
            reply,
        } => {
            let r = if follow {
                client.stat(&path).await
            } else {
                client.lstat(&path).await
            };
            let r = r
                .map(|a| FileAttrs::from(&a))
                .map_err(|e| sftp_err(&format!("stat {path}"), e));
            let _ = reply.send(r);
        }
        Job::SetStat { path, attrs, reply } => {
            let a = Attrs {
                permissions: attrs.permissions,
                mtime: attrs.mtime,
                atime: attrs.mtime,
                ..Attrs::default()
            };
            let r = client
                .setstat(&path, &a)
                .await
                .map_err(|e| sftp_err(&format!("setstat {path}"), e));
            let _ = reply.send(r);
        }
        Job::ReadLink(path, reply) => {
            let mut buf = [0u8; SFTP_BUF];
            let r = client
                .readlink(&path, &mut buf)
                .await
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .map_err(|e| sftp_err(&format!("readlink {path}"), e));
            let _ = reply.send(r);
        }
        Job::Symlink {
            target,
            link,
            reply,
        } => {
            let r = client
                .symlink(&target, &link)
                .await
                .map_err(|e| sftp_err(&format!("symlink {link}"), e));
            let _ = reply.send(r);
        }
        Job::OpenDir(path, reply) => {
            let r = client
                .opendir(&path)
                .await
                .map(|h| state.insert(h))
                .map_err(|e| sftp_err(&format!("opendir {path}"), e));
            let _ = reply.send(r);
        }
        Job::ReadDir(id, reply) => {
            let r = read_dir_batch(client, state, id).await;
            let _ = reply.send(r);
        }
        Job::Open { path, mode, reply } => {
            let r = match mode {
                OpenMode::Read => client.open_read(&path).await,
                OpenMode::Write => client.create(&path).await,
            };
            let r = r
                .map(|h| state.insert(h))
                .map_err(|e| sftp_err(&format!("open {path}"), e));
            let _ = reply.send(r);
        }
        Job::Read {
            handle,
            offset,
            len,
            reply,
        } => {
            let r = match state.get(handle) {
                Ok(h) => {
                    let mut buf = vec![0u8; len.min(CHUNK)];
                    match client.read(h, offset, &mut buf).await {
                        Ok(n) => {
                            buf.truncate(n);
                            Ok(buf)
                        }
                        Err(e) => Err(sftp_err("read", e)),
                    }
                }
                Err(e) => Err(e),
            };
            let _ = reply.send(r);
        }
        Job::Write {
            handle,
            offset,
            data,
            reply,
        } => {
            let r = match state.get(handle) {
                Ok(h) => client
                    .write(h, offset, &data)
                    .await
                    .map_err(|e| sftp_err("write", e)),
                Err(e) => Err(e),
            };
            let _ = reply.send(r);
        }
        Job::Close(id, reply) => {
            let r = match state.map.remove(&id) {
                Some(h) => client.close(&h).await.map_err(|e| sftp_err("close", e)),
                // Already gone: closing twice is not an error worth surfacing.
                None => Ok(()),
            };
            let _ = reply.send(r);
        }
        Job::MkDir(path, reply) => {
            let r = client
                .mkdir(&path, &Attrs::default())
                .await
                .map_err(|e| sftp_err(&format!("mkdir {path}"), e));
            let _ = reply.send(r);
        }
        Job::RmDir(path, reply) => {
            let r = client
                .rmdir(&path)
                .await
                .map_err(|e| sftp_err(&format!("rmdir {path}"), e));
            let _ = reply.send(r);
        }
        Job::Remove(path, reply) => {
            let r = client
                .remove(&path)
                .await
                .map_err(|e| sftp_err(&format!("remove {path}"), e));
            let _ = reply.send(r);
        }
        Job::Rename { from, to, reply } => {
            // posix-rename replaces the destination, which is what the app
            // expects of a move. Fall back where the server lacks it.
            let r = if client.extensions().posix_rename {
                client.posix_rename(&from, &to).await
            } else {
                client.rename(&from, &to).await
            };
            let _ = reply.send(r.map_err(|e| sftp_err(&format!("rename {from}"), e)));
        }
        Job::Exec { cmd, reply } => {
            let r = exec_capture(ssh, opened, pending, &cmd).await;
            let _ = reply.send(r);
        }
        Job::ExecStream {
            cmd,
            stdin,
            stdout,
            reply,
        } => {
            let r = exec_stream(ssh, opened, pending, &cmd, stdin, stdout.clone()).await;
            if let Err(ref e) = r {
                // Surface it on the reader too, which may be blocked on it.
                let _ = stdout.send(Err(io::Error::other(e.message.clone()))).await;
            }
            let _ = reply.send(r);
        }
    }
}

async fn read_dir_batch<R, W>(
    client: &mut SftpClient<R, W, SFTP_BUF>,
    state: &Handles,
    id: HandleId,
) -> SshResult<Option<Vec<DirItem>>>
where
    R: sunset_sftp::embedded_io_async::Read,
    W: sunset_sftp::embedded_io_async::Write,
{
    let handle = state.get(id)?;
    let Some(mut entries) = client
        .readdir(handle)
        .await
        .map_err(|e| sftp_err("readdir", e))?
    else {
        return Ok(None);
    };
    let mut out = Vec::new();
    while let Some(e) = entries
        .next()
        .await
        .map_err(|e| sftp_err("readdir entry", e))?
    {
        out.push(DirItem {
            name: String::from_utf8_lossy(e.filename()).into_owned(),
            attrs: FileAttrs::from(e.attrs()),
        });
    }
    Ok(Some(out))
}

/// Opens a channel and asks the progress loop to run `cmd` on it.
async fn start_exec<'g>(
    ssh: &'g SSHClient<'static>,
    opened: &EmbassyChannel<SunsetRawMutex, Opened<'g>, 4>,
    pending: &Pending<'g>,
    cmd: &str,
) -> SshResult<(ChanInOut<'g>, Option<ChanIn<'g>>)> {
    let (io, err) = ssh
        .open_session_nopty()
        .await
        .map_err(|e| SshError::fatal(format!("opening exec channel: {e}")))?;
    pending
        .borrow_mut()
        .insert(io.num().0, (ChanKind::Exec(cmd.to_string()), io, Some(err)));
    let (_kind, io, err) = opened.receive().await;
    Ok((io, err))
}

async fn exec_capture<'g>(
    ssh: &'g SSHClient<'static>,
    opened: &EmbassyChannel<SunsetRawMutex, Opened<'g>, 4>,
    pending: &Pending<'g>,
    cmd: &str,
) -> SshResult<ExecOutput> {
    let (io, err) = start_exec(ssh, opened, pending, cmd).await?;
    let (rx, _tx) = io.split();
    // Both halves have to be read concurrently: the peer can fill either pipe,
    // and a full one blocks the whole session, not just that stream.
    let (stdout, stderr) = embassy_futures::join::join(read_to_end(rx), drain(err)).await;
    Ok(ExecOutput { stdout, stderr })
}

async fn exec_stream<'g>(
    ssh: &'g SSHClient<'static>,
    opened: &EmbassyChannel<SunsetRawMutex, Opened<'g>, 4>,
    pending: &Pending<'g>,
    cmd: &str,
    stdin: Option<tmpsc::Receiver<Vec<u8>>>,
    stdout: tmpsc::Sender<io::Result<Vec<u8>>>,
) -> SshResult<Vec<u8>> {
    use sunset_sftp::embedded_io_async::Write as _;

    let (io, err) = start_exec(ssh, opened, pending, cmd).await?;
    let (mut rx, mut tx) = io.split();

    let feed = async {
        if let Some(mut stdin) = stdin {
            while let Some(chunk) = stdin.recv().await {
                if tx.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            let _ = tx.flush().await;
            // Without this a command reading its input to EOF never returns.
            // The channel stays readable, so it can still reply and exit.
            let _ = tx.send_eof().await;
        }
        core::future::pending::<()>().await
    };

    let pump = async {
        let mut b = vec![0u8; CHUNK];
        let mut wanted = true;
        loop {
            match rx.read(&mut b).await {
                Ok(0) => break,
                Ok(n) => {
                    if wanted && stdout.send(Ok(b[..n].to_vec())).await.is_err() {
                        // The reader gave up early. Keep draining anyway: an
                        // abandoned channel with unread data never closes, and
                        // then nothing else on this connection can proceed.
                        wanted = false;
                    }
                }
                // A closed channel is a normal end of output.
                Err(_) => break,
            }
        }
        // Signals EOF to the blocking reader.
        let _ = stdout.send(Ok(Vec::new())).await;
    };

    // stderr is read alongside so it can never stall the session, but it is
    // not something to wait for: it only reaches EOF once the channel closes,
    // which is what `pump` is already detecting.
    let collected = RefCell::new(Vec::new());
    let watch_stderr = async {
        if let Some(mut e) = err {
            let mut b = [0u8; 4096];
            loop {
                match e.read(&mut b).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut c = collected.borrow_mut();
                        if c.len() + n <= MAX_EXEC_OUTPUT {
                            c.extend_from_slice(&b[..n]);
                        }
                    }
                }
            }
        }
        core::future::pending::<()>().await
    };

    // Only `pump` ever completes; the other two park once their work is done.
    match select(pump, select(watch_stderr, feed)).await {
        Either::First(()) | Either::Second(_) => (),
    }
    Ok(collected.into_inner())
}

async fn drain(r: Option<impl sunset_sftp::embedded_io_async::Read>) -> Vec<u8> {
    match r {
        Some(r) => read_to_end(r).await,
        // No stderr half: never completes, so it loses every select race.
        None => core::future::pending().await,
    }
}

async fn read_to_end(mut r: impl sunset_sftp::embedded_io_async::Read) -> Vec<u8> {
    let mut out = Vec::new();
    let mut b = [0u8; 4096];
    loop {
        match r.read(&mut b).await {
            Ok(0) => break,
            Ok(n) => {
                if out.len() + n <= MAX_EXEC_OUTPUT {
                    out.extend_from_slice(&b[..n]);
                }
            }
            // A closed channel is a normal end of output.
            Err(_) => break,
        }
    }
    out
}

fn load_identities(paths: &[String]) -> Vec<SignKey> {
    let mut keys = Vec::new();
    for p in paths {
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        match SignKey::from_openssh(bytes) {
            Ok(k) => keys.push(k),
            Err(e) => log::warn!("skipping key {p}: {e}"),
        }
    }
    // Keys are offered by popping from the end, so restore the caller's order.
    keys.reverse();
    keys
}

#[cfg(unix)]
mod agent {
    use sunset::SignKey;
    use sunset_stdasync::AgentClient;

    /// Loads agent keys, appending them to the keys that will be offered.
    pub async fn load(keys: &mut Vec<SignKey>) -> Option<AgentClient> {
        let sock = std::env::var("SSH_AUTH_SOCK").ok()?;
        let mut agent = match AgentClient::new(sock).await {
            Ok(a) => a,
            Err(e) => {
                log::warn!("opening ssh-agent: {e}");
                return None;
            }
        };
        match agent.keys().await {
            Ok(ks) => {
                // Agent keys are offered first, matching ssh(1).
                for k in ks {
                    keys.push(k);
                }
                Some(agent)
            }
            Err(e) => {
                log::warn!("listing ssh-agent keys: {e}");
                None
            }
        }
    }
}
