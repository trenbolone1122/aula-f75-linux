//! One running copy: a second launch asks the first to show its window (or quit) over a socket.
use crate::worker::Core;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;

fn path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("f75-studio.sock")
}

/// True if another copy is running (and got the message).
pub fn signal(msg: &str) -> bool {
    match UnixStream::connect(path()) {
        Ok(mut s) => s.write_all(msg.as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// Serializa a checagem e o bind para que dois lançamentos não disputem o socket.
pub fn claim(message: &str) -> std::io::Result<Option<UnixListener>> {
    let p = path();
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(p.with_extension("lock"))?;
    lock.lock()?;
    match UnixStream::connect(&p) {
        Ok(mut stream) => {
            stream.write_all(message.as_bytes())?;
            return Ok(None);
        }
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
            std::fs::remove_file(&p)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    UnixListener::bind(p).map(Some)
}

pub fn listen(listener: UnixListener, core: Arc<Core>) {
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut msg = String::new();
            let _ = s.read_to_string(&mut msg);
            match msg.trim() {
                "quit" => core.request_quit(),
                "ping" => {}
                "show" => core.request_show(),
                _ => {}
            }
        }
    });
}
