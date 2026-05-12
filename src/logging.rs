use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

const ROTATION_INTERVAL: Duration = Duration::from_secs(5 * 3600);

struct WriterState {
    file: Option<File>,
    opened_at: Instant,
    dir: PathBuf,
    prefix: String,
}

#[derive(Clone)]
pub struct RotatingFileWriter {
    state: Arc<Mutex<WriterState>>,
}

impl RotatingFileWriter {
    pub fn new(dir: PathBuf, prefix: &str) -> io::Result<Self> {
        let mut state = WriterState {
            file: None,
            opened_at: Instant::now(),
            dir,
            prefix: prefix.to_string(),
        };
        // Open the initial log file eagerly so errors surface early.
        state.open_next_file()?;
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
        })
    }
}

impl WriterState {
    fn open_next_file(&mut self) -> io::Result<()> {
        let now = chrono::Local::now();
        let filename = format!("{}.{}.log", self.prefix, now.format("%Y-%m-%dT%H-%M-%S"));
        let path = self.dir.join(&filename);
        fs::create_dir_all(&self.dir)?;
        self.file = Some(File::create(&path)?);
        self.opened_at = Instant::now();
        Ok(())
    }

    fn maybe_rotate(&mut self) -> io::Result<()> {
        if self.file.is_none() || self.opened_at.elapsed() >= ROTATION_INTERVAL {
            self.open_next_file()?;
        }
        Ok(())
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.maybe_rotate()?;
        match self.file.as_mut() {
            Some(f) => f.write(buf),
            None => io::stderr().write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

pub struct RotatingWriterHandle {
    state: Arc<Mutex<WriterState>>,
}

impl Write for RotatingWriterHandle {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}

impl<'a> MakeWriter<'a> for RotatingFileWriter {
    type Writer = RotatingWriterHandle;

    fn make_writer(&'a self) -> Self::Writer {
        RotatingWriterHandle {
            state: Arc::clone(&self.state),
        }
    }
}

/// Initializes the tracing subscriber with:
/// - stderr output (colored, with env filter)
/// - file output to `logs/{service_name}.{timestamp}.log`, rotated every 5 hours
///
/// Returns a guard that must be kept alive for the program's lifetime.
/// Dropping it flushes and shuts down the background file writer.
pub fn init_logging(service_name: &str) -> tracing_appender::non_blocking::WorkerGuard {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let file_writer = RotatingFileWriter::new(PathBuf::from("logs"), service_name)
        .expect("create rotating file writer");

    let (non_blocking, guard) = tracing_appender::non_blocking::NonBlocking::new(file_writer);

    let stderr_layer = tracing_subscriber::fmt::layer().with_filter(env_filter);

    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(non_blocking);

    tracing_subscriber::registry()
        .with(stderr_layer)
        .with(file_layer)
        .init();

    guard
}
