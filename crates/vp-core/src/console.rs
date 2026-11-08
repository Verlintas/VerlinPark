use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::sync::Mutex;
use uuid::Uuid;
use vp_protocol::types::{
    ConsoleChunk, ConsoleCloseResult, ConsoleOpenResult, ConsoleReadResult, ConsoleSignalResult,
};

use crate::tools::{ToolContext, sandboxed_command};

const MAX_BUFFER_BYTES: usize = 256 * 1024;
const MAX_READ_CHUNKS: usize = 200;
const MAX_WRITE_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub struct ConsoleManager {
    consoles: Mutex<HashMap<String, Arc<Console>>>,
    sessions: Mutex<HashMap<String, String>>,
    open_lock: Mutex<()>,
}

struct Console {
    session_id: String,
    pid: Option<i32>,
    child: StdMutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    output: Arc<StdMutex<ConsoleBuffer>>,
    closed: AtomicBool,
    last_used: StdMutex<Instant>,
}

struct ConsoleBuffer {
    chunks: VecDeque<ConsoleChunk>,
    next_seq: u64,
    bytes: usize,
    dropped: u64,
    status: String,
    exit_code: Option<i32>,
    readers: usize,
    pending_exit: Option<Option<i32>>,
}

impl ConsoleBuffer {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            next_seq: 1,
            bytes: 0,
            dropped: 0,
            status: "running".to_string(),
            exit_code: None,
            readers: 2,
            pending_exit: None,
        }
    }

    fn append(&mut self, stream: &str, text: String) {
        if text.is_empty() {
            return;
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        self.bytes = self.bytes.saturating_add(text.len());
        self.chunks.push_back(ConsoleChunk {
            seq,
            stream: stream.to_string(),
            text,
        });
        while self.bytes > MAX_BUFFER_BYTES {
            let Some(chunk) = self.chunks.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(chunk.text.len());
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

impl Console {
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
            let Some(child) = child.as_mut() else {
                return;
            };
            match child.try_wait() {
                Ok(Some(status)) => Some(status.code()),
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
        if let Some(pid) = self.pid {
            // SAFETY: negative pid targets the process group created by
            // sandboxed_command. ESRCH is the expected no-op after exit.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        if let Ok(mut child) = self.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.start_kill();
        }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        self.kill();
    }
}

impl ConsoleManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn open(&self, session_id: &str, context: ToolContext) -> Result<ConsoleOpenResult> {
        let _open_guard = self.open_lock.lock().await;
        let existing = self.sessions.lock().await.get(session_id).cloned();
        if let Some(existing) = existing {
            let _ = self.close(&existing).await;
        }
        let worktree = context.root.to_string_lossy().to_string();
        let mut command = sandboxed_command(&context).await?;
        command.arg("/bin/sh").arg("-s").stdin(Stdio::piped());
        let mut child = command.spawn().context("spawning console shell")?;
        let pid = child.id().and_then(|pid| i32::try_from(pid).ok());
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("console has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("console has no stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("console has no stderr"))?;
        let output = Arc::new(StdMutex::new(ConsoleBuffer::new()));
        let id = Uuid::now_v7().to_string();
        let console = Arc::new(Console {
            session_id: session_id.to_string(),
            pid,
            child: StdMutex::new(Some(child)),
            stdin: Mutex::new(Some(stdin)),
            output: output.clone(),
            closed: AtomicBool::new(false),
            last_used: StdMutex::new(Instant::now()),
        });
        tokio::spawn(read_stream(stdout, "stdout", output.clone()));
        tokio::spawn(read_stream(stderr, "stderr", output));
        self.consoles.lock().await.insert(id.clone(), console);
        self.sessions
            .lock()
            .await
            .insert(session_id.to_string(), id.clone());
        Ok(ConsoleOpenResult {
            console_id: id,
            worktree,
        })
    }

    pub async fn write(&self, console_id: &str, data: &str) -> Result<bool> {
        if data.len() > MAX_WRITE_BYTES {
            bail!("console write exceeds {MAX_WRITE_BYTES} bytes");
        }
        let console = self.get(console_id).await?;
        console.touch();
        if console.closed.load(Ordering::SeqCst) {
            bail!("console is closed");
        }
        let mut stdin = console.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            bail!("console stdin is closed");
        };
        stdin
            .write_all(data.as_bytes())
            .await
            .context("writing console input")?;
        stdin.flush().await.context("flushing console input")?;
        Ok(true)
    }

    pub async fn read(&self, console_id: &str, after_seq: u64) -> Result<ConsoleReadResult> {
        let console = self.get(console_id).await?;
        console.touch();
        console.refresh_status();
        let output = console
            .output
            .lock()
            .map_err(|_| anyhow!("console output mutex poisoned"))?;
        let chunks = output
            .chunks
            .iter()
            .filter(|chunk| chunk.seq > after_seq)
            .take(MAX_READ_CHUNKS)
            .cloned()
            .collect();
        Ok(ConsoleReadResult {
            console_id: console_id.to_string(),
            chunks,
            next_seq: output.next_seq.saturating_sub(1),
            status: output.status.clone(),
            exit_code: output.exit_code,
            dropped: output.dropped,
        })
    }

    pub async fn signal(&self, console_id: &str, signal: &str) -> Result<ConsoleSignalResult> {
        let console = self.get(console_id).await?;
        console.touch();
        let signal = match signal {
            "int" => libc::SIGINT,
            "term" => libc::SIGTERM,
            "kill" => libc::SIGKILL,
            other => bail!("unknown console signal: {other}"),
        };
        let signaled = if let Some(pid) = console.pid {
            // SAFETY: negative pid targets the console process group.
            unsafe { libc::kill(-pid, signal) == 0 }
        } else {
            false
        };
        Ok(ConsoleSignalResult { signaled })
    }

    /// Close consoles that have not been read/written for `idle`. This bounds
    /// resource use when the app crashes or a client forgets to close a tab.
    pub async fn sweep_idle(&self, idle: Duration) -> usize {
        let ids = {
            let consoles = self.consoles.lock().await;
            consoles
                .iter()
                .filter(|(_, console)| {
                    !console.closed.load(Ordering::SeqCst)
                        && console
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

    pub async fn close(&self, console_id: &str) -> Result<ConsoleCloseResult> {
        let console = self.consoles.lock().await.remove(console_id);
        let Some(console) = console else {
            return Ok(ConsoleCloseResult {
                closed: false,
                exit_code: None,
            });
        };
        self.sessions.lock().await.remove(&console.session_id);
        console.closed.store(true, Ordering::SeqCst);
        console.kill();
        console.refresh_status();
        let exit_code = console
            .output
            .lock()
            .map(|output| output.exit_code)
            .unwrap_or(None);
        Ok(ConsoleCloseResult {
            closed: true,
            exit_code,
        })
    }

    async fn get(&self, console_id: &str) -> Result<Arc<Console>> {
        self.consoles
            .lock()
            .await
            .get(console_id)
            .cloned()
            .ok_or_else(|| anyhow!("console not found: {console_id}"))
    }
}

async fn read_stream<R>(mut reader: R, stream: &'static str, output: Arc<StdMutex<ConsoleBuffer>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0u8; 8192];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                let text = decode_utf8_stream(&mut pending, &buffer[..read]);
                if !text.is_empty()
                    && let Ok(mut output) = output.lock()
                {
                    output.append(stream, text);
                }
            }
            Err(_) => break,
        }
    }
    if !pending.is_empty()
        && let Ok(mut output) = output.lock()
    {
        output.append(stream, String::from_utf8_lossy(&pending).to_string());
    }
    if let Ok(mut output) = output.lock() {
        output.reader_done();
    }
}

fn decode_utf8_stream(pending: &mut Vec<u8>, bytes: &[u8]) -> String {
    pending.extend_from_slice(bytes);
    let mut text = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                text.push_str(valid);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    text.push_str(&String::from_utf8_lossy(&pending[..valid_up_to]));
                    pending.drain(..valid_up_to);
                }
                let Some(invalid_len) = error.error_len() else {
                    break;
                };
                text.push('\u{FFFD}');
                pending.drain(..invalid_len.min(pending.len()));
            }
        }
    }
    text
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

    async fn wait_for<F>(manager: &ConsoleManager, id: &str, predicate: F) -> String
    where
        F: Fn(&str) -> bool,
    {
        let mut seq = 0;
        for _ in 0..100 {
            let result = manager.read(id, seq).await.unwrap();
            seq = result.next_seq;
            let text = result
                .chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<String>();
            if predicate(&text) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("console output did not match");
    }

    #[tokio::test]
    async fn runs_commands_in_the_session_worktree() {
        let dir = git_repo();
        let manager = ConsoleManager::new();
        let opened = manager
            .open("session-1", context(dir.path()))
            .await
            .unwrap();
        manager.write(&opened.console_id, "pwd\n").await.unwrap();
        let output = wait_for(&manager, &opened.console_id, |text| {
            text.contains(dir.path().to_string_lossy().as_ref())
        })
        .await;
        assert!(output.contains(dir.path().to_string_lossy().as_ref()));

        manager.write(&opened.console_id, "exit 0\n").await.unwrap();
        let mut status = String::new();
        for _ in 0..100 {
            let result = manager.read(&opened.console_id, 0).await.unwrap();
            status = result.status;
            if status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "exited");
        let closed = manager.close(&opened.console_id).await.unwrap();
        assert!(closed.closed);
        assert_eq!(closed.exit_code, Some(0));
    }

    #[tokio::test]
    async fn keeps_writes_inside_the_worktree() {
        let dir = git_repo();
        let other = TempDir::new().unwrap();
        let outside = other.path().join("outside.txt");
        let manager = ConsoleManager::new();
        let opened = manager
            .open("session-1", context(dir.path()))
            .await
            .unwrap();
        manager
            .write(
                &opened.console_id,
                &format!("echo nope > {}\n", outside.display()),
            )
            .await
            .unwrap();
        manager.write(&opened.console_id, "exit 0\n").await.unwrap();
        for _ in 0..100 {
            let result = manager.read(&opened.console_id, 0).await.unwrap();
            if result.status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!outside.exists());
        manager.close(&opened.console_id).await.unwrap();
    }

    #[tokio::test]
    async fn sweeps_idle_consoles() {
        let dir = git_repo();
        let manager = ConsoleManager::new();
        let opened = manager
            .open("session-1", context(dir.path()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(manager.sweep_idle(Duration::from_millis(1)).await, 1);
        assert!(manager.read(&opened.console_id, 0).await.is_err());
    }
}
