use std::fs;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::process::id;

pub struct PidLock {
    lock_path: PathBuf,
    #[allow(dead_code)]
    fd: Option<RawFd>,
}

impl PidLock {
    pub fn acquire() -> anyhow::Result<PidLock> {
        let lock_path = Self::get_lock_path();

        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }

        #[cfg(target_os = "linux")]
        {
            match Self::acquire_pidfd() {
                Ok(fd) => {
                    tracing::info!("Acquired PID lock with pidfd (pid: {})", id());
                    return Ok(PidLock {
                        lock_path,
                        fd: Some(fd),
                    });
                }
                Err(e) => tracing::debug!("pidfd_open not available: {}", e),
            }
        }

        Self::acquire_fallback(&lock_path)
    }

    #[cfg(target_os = "linux")]
    fn acquire_pidfd() -> anyhow::Result<RawFd> {
        const SYS_PIDFD_OPEN: u64 = 434;
        const PIDFD_CLOEXEC: u32 = 0o2000000;

        let pid = unsafe { libc::getpid() };
        let ret = unsafe { libc::syscall(SYS_PIDFD_OPEN as i64, pid as i64, PIDFD_CLOEXEC as i64) };

        if ret < 0 {
            return Err(anyhow::anyhow!(
                "pidfd_open failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        Ok(ret as RawFd)
    }

    fn acquire_fallback(lock_path: &PathBuf) -> anyhow::Result<PidLock> {
        if let Some(mut file) = Self::create_lock_file(lock_path)? {
            use std::io::Write;
            file.write_all(id().to_string().as_bytes())?;
            file.flush()?;
            tracing::info!(
                "Acquired PID lock at {} (fd: {:?})",
                lock_path.display(),
                file.as_raw_fd()
            );
            return Ok(PidLock {
                lock_path: lock_path.clone(),
                fd: Some(file.as_raw_fd()),
            });
        }

        if let Ok(content) = fs::read_to_string(lock_path) {
            if let Ok(old_pid) = content.trim().parse::<u32>() {
                if Self::is_process_running(old_pid) {
                    anyhow::bail!("Another instance is already running (PID: {old_pid}). Exiting.");
                }
                tracing::warn!("Stale lock file found (PID {old_pid} not running), removing it");
                fs::remove_file(lock_path)?;
            }
        }

        if let Some(mut file) = Self::create_lock_file(lock_path)? {
            use std::io::Write;
            file.write_all(id().to_string().as_bytes())?;
            file.flush()?;
            tracing::info!("Acquired PID lock at {} (fallback)", lock_path.display());
            return Ok(PidLock {
                lock_path: lock_path.clone(),
                fd: None,
            });
        }

        anyhow::bail!("Failed to acquire PID lock")
    }

    fn get_lock_path() -> PathBuf {
        dirs::runtime_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("sysaud")
            .join("sysaud.pid")
    }

    fn create_lock_file(path: &PathBuf) -> anyhow::Result<Option<std::fs::File>> {
        Ok(fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .ok())
    }

    fn is_process_running(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        let path = format!("/proc/{pid}");
        fs::read_dir(&path).is_ok()
    }

    fn cleanup_lock_file(&self) {
        if self.lock_path.exists() {
            if let Ok(content) = fs::read_to_string(&self.lock_path) {
                if content.trim() == id().to_string() {
                    if let Err(e) = fs::remove_file(&self.lock_path) {
                        tracing::warn!("Failed to remove lock file: {}", e);
                    } else {
                        tracing::debug!("Released PID lock");
                    }
                }
            }
        }
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        if let Some(fd) = self.fd {
            #[cfg(target_os = "linux")]
            {
                unsafe { libc::close(fd) };
            }
        }

        self.cleanup_lock_file();
    }
}
