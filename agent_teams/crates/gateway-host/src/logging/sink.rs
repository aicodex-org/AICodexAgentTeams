//! 非阻塞日志接收与后台批量写入，错误记录保留独立容量，关闭在预算内排空

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

use super::health::LogHealth;
use super::writer::RotatingLogWriter;

pub(super) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

const QUEUE_CAPACITY: usize = 4_096;
const QUEUE_BYTES: usize = 64 * 1024 * 1024;
const ERROR_CAPACITY: usize = 512;
const ERROR_BYTES: usize = 8 * 1024 * 1024;
const WRITE_BATCH: usize = 64;

// 通道不另设容量；预留在复制正文之前完成，并随记录释放，包含正在写入的记录
struct Budget {
    items: AtomicUsize,
    bytes: AtomicUsize,
    maximum_items: usize,
    maximum_bytes: usize,
}

impl Budget {
    fn new(maximum_items: usize, maximum_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            items: AtomicUsize::new(0),
            bytes: AtomicUsize::new(0),
            maximum_items,
            maximum_bytes,
        })
    }

    fn reserve(self: &Arc<Self>, bytes: usize) -> Option<Reservation> {
        self.items
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |items| {
                items
                    .checked_add(1)
                    .filter(|items| *items <= self.maximum_items)
            })
            .ok()?;
        if self
            .bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|used| *used <= self.maximum_bytes)
            })
            .is_err()
        {
            self.items.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Reservation {
            budget: Arc::clone(self),
            bytes,
        })
    }
}

struct Reservation {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.budget.items.fetch_sub(1, Ordering::AcqRel);
    }
}

enum Command {
    Write(Vec<u8>, Reservation),
    Shutdown,
}

// 关闭位与在途提交数共享一个原子状态，最后一个提交者在 Write 后发送 Shutdown
// 请求侧不等待互斥锁或 I/O，关闭也不会越过已获得接收许可的记录
const CLOSING: usize = 1 << (usize::BITS - 1);

struct Ingress {
    sender: mpsc::Sender<Command>,
    state: AtomicUsize,
}

impl Ingress {
    fn enter(&self) -> Option<Admission<'_>> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state < CLOSING - 1).then(|| state + 1)
            })
            .ok()?;
        Some(Admission(self))
    }

    fn close(&self) {
        if self.state.fetch_or(CLOSING, Ordering::AcqRel) == 0 {
            let _ = self.sender.send(Command::Shutdown);
        }
    }
}

struct Admission<'a>(&'a Ingress);

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.0.state.fetch_sub(1, Ordering::AcqRel) == (CLOSING | 1) {
            let _ = self.0.sender.send(Command::Shutdown);
        }
    }
}

// 文件与 stdout 共用相同的容量、错误观测和关闭协议
pub(super) enum LogOutput {
    File(Box<RotatingLogWriter>),
    Stdout(io::Stdout),
}

impl LogOutput {
    fn sync(&mut self) -> io::Result<()> {
        match self {
            Self::File(writer) => writer.sync(),
            Self::Stdout(writer) => writer.flush(),
        }
    }
}

impl Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(writer) => writer.write(bytes),
            Self::Stdout(writer) => writer.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(writer) => writer.flush(),
            Self::Stdout(writer) => writer.flush(),
        }
    }
}

#[derive(Clone)]
pub(super) struct LogSink {
    ingress: Arc<Ingress>,
    regular: Arc<Budget>,
    errors: Arc<Budget>,
    important: bool,
    health: Arc<LogHealth>,
}

pub(super) struct SinkGuard {
    ingress: Arc<Ingress>,
    worker: Option<JoinHandle<()>>,
    completed: Mutex<mpsc::Receiver<()>>,
    health: Arc<LogHealth>,
}

impl LogSink {
    pub(super) fn spawn(
        writer: LogOutput,
        name: &'static str,
        health: Arc<LogHealth>,
    ) -> io::Result<(Self, SinkGuard)> {
        let (sender, receiver) = mpsc::channel();
        let worker_health = Arc::clone(&health);
        let ingress = Arc::new(Ingress {
            sender,
            state: AtomicUsize::new(0),
        });
        let (finished, completed) = mpsc::channel();
        let worker = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // 无论正常返回还是 panic，都即时报告线程退出，唤醒关闭等待
                let _completion = Completion {
                    finished,
                    health: Arc::clone(&worker_health),
                };
                let mut writer = writer;
                let mut reservations = Vec::with_capacity(WRITE_BATCH);
                'consume: while let Ok(mut command) = receiver.recv() {
                    for index in 0..WRITE_BATCH {
                        match command {
                            Command::Write(bytes, reservation) => {
                                if let Err(error) = writer.write_all(&bytes) {
                                    worker_health.write_failed("write", error.kind());
                                }
                                reservations.push(reservation);
                            }
                            Command::Shutdown => break 'consume,
                        }
                        if index + 1 == WRITE_BATCH {
                            break;
                        }
                        match receiver.try_recv() {
                            Ok(next) => command = next,
                            Err(_) => break,
                        }
                    }
                    if let Err(error) = writer.flush() {
                        worker_health.write_failed("flush", error.kind());
                    }
                    // 缓冲刷入文件后才归还额度，慢输出不能让在途记录逃逸预算
                    reservations.clear();
                }
                if let Err(error) = writer.sync() {
                    worker_health.write_failed("sync", error.kind());
                }
                drop(writer);
            })?;
        Ok((
            Self {
                ingress: Arc::clone(&ingress),
                regular: Budget::new(QUEUE_CAPACITY, QUEUE_BYTES),
                errors: Budget::new(ERROR_CAPACITY, ERROR_BYTES),
                important: false,
                health: Arc::clone(&health),
            },
            SinkGuard {
                ingress,
                worker: Some(worker),
                completed: Mutex::new(completed),
                health,
            },
        ))
    }
}

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }

    fn make_writer_for(&'a self, metadata: &tracing::Metadata<'_>) -> Self::Writer {
        let mut writer = self.clone();
        writer.important = *metadata.level() <= tracing::Level::WARN;
        writer
    }
}

impl Write for LogSink {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let Some(_admission) = self.ingress.enter() else {
            self.health.rejected(self.important);
            return Ok(buffer.len());
        };
        let bytes = buffer.len().saturating_add(std::mem::size_of::<Command>());
        let reservation = self
            .regular
            .reserve(bytes)
            .or_else(|| self.important.then(|| self.errors.reserve(bytes)).flatten());
        if let Some(reservation) = reservation {
            if self
                .ingress
                .sender
                .send(Command::Write(buffer.to_vec(), reservation))
                .is_err()
            {
                self.health.rejected(self.important);
            }
        } else {
            self.health.rejected(self.important);
        }
        // tracing 的 writer 失败回退可能同步写 stderr；缺口由独立健康状态报告
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SinkGuard {
    pub(super) fn begin_shutdown(&self) {
        self.ingress.close();
    }

    pub(super) fn finish_shutdown(&mut self, deadline: Instant) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        self.begin_shutdown();
        match self
            .completed
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                // 完成通知在用户 I/O 和 writer 析构之后发送，join 不再等待磁盘
                // panic 已由 Completion 标记，不重复累计
                let _ = worker.join();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.health
                    .write_failed("shutdown", io::ErrorKind::TimedOut);
                // 操作系统同步 I/O 无法安全取消；脱离线程，让进程能够完成退出
                // 原队列仍保持有界，后续记录会被入口拒收
                drop(worker);
            }
        }
    }
}

impl Drop for SinkGuard {
    fn drop(&mut self) {
        self.finish_shutdown(Instant::now() + SHUTDOWN_TIMEOUT);
    }
}

struct Completion {
    finished: mpsc::Sender<()>,
    health: Arc<LogHealth>,
}

impl Drop for Completion {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.health.write_failed("worker", io::ErrorKind::Other);
        }
        let _ = self.finished.send(());
    }
}
