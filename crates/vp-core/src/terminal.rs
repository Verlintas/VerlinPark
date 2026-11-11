use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::Mutex;
use uuid::Uuid;
use vp_protocol::types::{
    TerminalChunk, TerminalCloseResult, TerminalOpenResult, TerminalReadResult,
    TerminalResizeResult, TerminalWriteResult,
};

use crate::tools::{ToolContext, sandboxed_command};

const MAX_BUFFER_BYTES: usize = 512 * 1024;
const MAX_READ_CHUNKS: usize = 200;
const MAX_WRITE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct TerminalManager {
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
    sessions: Mutex<HashMap<String, String>>,
    open_lock: Mutex<()>,
}

struct Terminal {
    session_id: String,
    master: StdMutex<Box<dyn MasterPty + Send>>,
    writer: StdMutex<Box<dyn Write + Send>>,
    child: StdMutex<Box<dyn Child + Send + Sync>>,
    output: Arc<StdMutex<TerminalBuffer>>,
    closed: AtomicBool,
    last_used: StdMutex<Instant>,
}

struct TerminalBuffer {
    chunks: VecDeque<(u64, Vec<u8>)>,
    next_seq: u64,
    bytes: usize,
    dropped: u64,
    status: String,
    exit_code: Option<i32>,
    readers: usize,
    pending_exit: Option<Option<i32>>,
}

impl TerminalBuffer {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            next_seq: 1,
            bytes: 0,
            dropped: 0,
            status: "running".to_string(),
            exit_code: None,
            readers: 1,
            pending_exit: None,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes.len());
        self.chunks.push_back((seq, bytes.to_vec()));
        while self.bytes > MAX_BUFFER_BYTES {
            let Some((_, chunk)) = self.chunks.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(chunk.len());
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    fn finish(&mut self, exit_code: Option<i32>) {
        if self.status != "running" {
            return;
        }
        self.exit_code = exit_code;
        self.status = match exit_code {
            Some(0) => "exited".to_string(),
            Some(_) => "failed".to_string(),
            None => "exited".to_string(),
        };
    }

    fn mark_exit(&mut self, exit_code: Option<i32>) {
        self.pending_exit = Some(exit_code);
        if self.readers == 0 {
            self.finish(exit_code);
        }
    }

    fn reader_done(&mut self) {
        self.readers = self.readers.saturating_sub(1);
        if self.readers == 0
            && let Some(exit_code) = self.pending_exit.take()
        {
            self.finish(exit_code);
        }
    }
}

impl Terminal {
    fn touch(&self) {
        if let Ok(mut last_used) = self.last_used.lock() {
            *last_used = Instant::now();
        }
    }

    fn refresh_status(&self) {
        let exit = {
            let Ok(mut child) = self.child.lock() else {
                return;
            };
            match child.try_wait() {
                Ok(Some(status)) => Some(i32::try_from(status.exit_code()).ok()),
                Ok(None) => None,
                Err(_) => Some(None),
            }
        };
        if self.closed.load(Ordering::SeqCst) {
            if let Ok(mut output) = self.output.lock() {
                output.status = "closed".to_string();
            }
            return;
        }
        if let Some(exit_code) = exit
            && let Ok(mut output) = self.output.lock()
        {
            output.mark_exit(exit_code);
        }
    }

    fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            if let Some(pid) = child.process_id().and_then(|pid| i32::try_from(pid).ok()) {
                // SAFETY: the PTY child is a session/process-group leader on Unix.
                // ESRCH is the expected no-op after exit.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
            let _ = child.kill();
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.kill();
    }
}

impl TerminalManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn open(
        &self,
        session_id: &str,
        context: ToolContext,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalOpenResult> {
        let _open_guard = self.open_lock.lock().await;
        let existing = self.sessions.lock().await.get(session_id).cloned();
        if let Some(existing) = existing {
            let _ = self.close(&existing).await;
        }
        let worktree = context.root.to_string_lossy().to_string();
        let command = sandboxed_command(&context).await?;
        let std_command = command.as_std();
        let program = std_command.get_program().to_os_string();
        let args = std_command
            .get_args()
            .map(|arg| arg.to_os_string())
            .collect::<Vec<_>>();
        let envs = std_command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
            .collect::<Vec<_>>();
        let cwd = std_command
            .get_current_dir()
            .map(PathBuf::from)
            .unwrap_or_else(|| context.root.clone());

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening PTY")?;
        let mut builder = CommandBuilder::new(program);
        builder.args(args);
        builder.env_clear();
        for (key, value) in envs {
            if let Some(value) = value {
                builder.env(key, value);
            }
        }
        builder.cwd(cwd);
        builder.args([OsString::from("/bin/sh"), OsString::from("-l")]);
        let child = pair
            .slave
            .spawn_command(builder)
            .context("spawning terminal shell")?;
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .context("cloning PTY reader")?;
        let writer = pair.master.take_writer().context("taking PTY writer")?;
        let output = Arc::new(StdMutex::new(TerminalBuffer::new()));
        let id = Uuid::now_v7().to_string();
        let terminal = Arc::new(Terminal {
            session_id: session_id.to_string(),
            master: StdMutex::new(pair.master),
            writer: StdMutex::new(writer),
            child: StdMutex::new(child),
            output: output.clone(),
            closed: AtomicBool::new(false),
            last_used: StdMutex::new(Instant::now()),
        });
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if let Ok(mut output) = output.lock() {
                            output.append(&buffer[..read]);
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Ok(mut output) = output.lock() {
                output.reader_done();
            }
        });
        self.terminals.lock().await.insert(id.clone(), terminal);
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), id.clone());
        Ok(TerminalOpenResult {
            terminal_id: id,
            worktree,
        })
    }

    pub async fn write(&self, terminal_id: &str, data: &str) -> Result<TerminalWriteResult> {
        let bytes = BASE64.decode(data).context("decoding terminal input")?;
        if bytes.len() > MAX_WRITE_BYTES {
            bail!("terminal write exceeds {MAX_WRITE_BYTES} bytes");
        }
        let terminal = self.get(terminal_id).await?;
        terminal.touch();
        if terminal.closed.load(Ordering::SeqCst) {
            bail!("terminal is closed");
        }
        let mut writer = terminal
            .writer
            .lock()
            .map_err(|_| anyhow!("terminal writer mutex poisoned"))?;
        writer.write_all(&bytes).context("writing terminal input")?;
        writer.flush().context("flushing terminal input")?;
        Ok(TerminalWriteResult { written: true })
    }

    pub async fn read(&self, terminal_id: &str, after_seq: u64) -> Result<TerminalReadResult> {
        let terminal = self.get(terminal_id).await?;
        terminal.touch();
        terminal.refresh_status();
        let output = terminal
            .output
            .lock()
            .map_err(|_| anyhow!("terminal output mutex poisoned"))?;
        let chunks = output
            .chunks
            .iter()
            .filter(|(seq, _)| *seq > after_seq)
            .take(MAX_READ_CHUNKS)
            .map(|(seq, data)| TerminalChunk {
                seq: *seq,
                data: BASE64.encode(data),
            })
            .collect();
        Ok(TerminalReadResult {
            terminal_id: terminal_id.to_string(),
            chunks,
            next_seq: output.next_seq.saturating_sub(1),
            status: output.status.clone(),
            exit_code: output.exit_code,
            dropped: output.dropped,
        })
    }

    pub async fn resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalResizeResult> {
        let terminal = self.get(terminal_id).await?;
        terminal.touch();
        let master = terminal
            .master
            .lock()
            .map_err(|_| anyhow!("terminal master mutex poisoned"))?;
        master
            .resize(PtySize {
                rows: rows.clamp(1, 500),
                cols: cols.clamp(1, 500),
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resizing PTY")?;
        Ok(TerminalResizeResult { resized: true })
    }

    /// Close terminals that have not been read/written/resized for `idle`.
    pub async fn sweep_idle(&self, idle: Duration) -> usize {
        let ids = {
            let terminals = self.terminals.lock().await;
            terminals
                .iter()
                .filter(|(_, terminal)| {
                    !terminal.closed.load(Ordering::SeqCst)
                        && terminal
                            .last_used
                            .lock()
                            .map(|last_used| last_used.elapsed() >= idle)
                            .unwrap_or(false)
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        for id in &ids {
            let _ = self.close(id).await;
        }
        ids.len()
    }

    pub async fn close(&self, terminal_id: &str) -> Result<TerminalCloseResult> {
        let terminal = self.terminals.lock().await.remove(terminal_id);
        let Some(terminal) = terminal else {
            return Ok(TerminalCloseResult {
                closed: false,
                exit_code: None,
            });
        };
        self.sessions.lock().await.remove(&terminal.session_id);
        terminal.closed.store(true, Ordering::SeqCst);
        terminal.kill();
        terminal.refresh_status();
        let exit_code = terminal
            .output
            .lock()
            .map(|output| output.exit_code)
            .unwrap_or(None);
        Ok(TerminalCloseResult {
            closed: true,
            exit_code,
        })
    }

    async fn get(&self, terminal_id: &str) -> Result<Arc<Terminal>> {
        self.terminals
            .lock()
            .await
            .get(terminal_id)
            .cloned()
            .ok_or_else(|| anyhow!("terminal not found: {terminal_id}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;
    use tempfile::TempDir;

    fn context(root: &Path) -> ToolContext {
        ToolContext {
            root: root.to_path_buf(),
            project_root: root.to_path_buf(),
            sandbox_root: root.join("scratch"),
            secret_roots: Vec::new(),
            network_allowed: false,
            home: root.join("home"),
            restrict_home: false,
            read_jail: false,
            git_branch: None,
            remote: None,
            session_id: None,
            process_supervisor: None,
        }
    }

    fn git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        dir
    }

    async fn wait_for(manager: &TerminalManager, id: &str, needle: &str) -> String {
        let mut seq = 0;
        let mut text = String::new();
        for _ in 0..100 {
            let result = manager.read(id, seq).await.unwrap();
            seq = result.next_seq;
            for chunk in result.chunks {
                let bytes = BASE64.decode(chunk.data).unwrap();
                text.push_str(&String::from_utf8_lossy(&bytes));
            }
            if text.contains(needle) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("terminal output did not contain {needle:?}: {text}");
    }

    #[tokio::test]
    async fn runs_interactive_commands_in_a_pty() {
        let dir = git_repo();
        let manager = TerminalManager::new();
        let opened = manager
            .open("session-1", context(dir.path()), 80, 24)
            .await
            .unwrap();
        let input = BASE64.encode(b"echo terminal-hello\n");
        manager.write(&opened.terminal_id, &input).await.unwrap();
        wait_for(&manager, &opened.terminal_id, "terminal-hello").await;

        let input = BASE64.encode(b"exit 0\n");
        manager.write(&opened.terminal_id, &input).await.unwrap();
        let mut status = String::new();
        for _ in 0..100 {
            let result = manager.read(&opened.terminal_id, 0).await.unwrap();
            status = result.status;
            if status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "exited");
        let closed = manager.close(&opened.terminal_id).await.unwrap();
        assert!(closed.closed);
    }
}
