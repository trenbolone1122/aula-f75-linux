use std::{fs, io::ErrorKind, os::unix::net::UnixListener, process::Command};

#[test]
fn help_ignores_the_running_instance() {
    let dir = std::env::temp_dir().join(format!("f75-cli-test-{}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let socket = UnixListener::bind(dir.join("f75-studio.sock")).unwrap();
    socket.set_nonblocking(true).unwrap();
    let run = |arg| {
        Command::new(env!("CARGO_BIN_EXE_f75-studio"))
            .arg(arg)
            .env("XDG_RUNTIME_DIR", &dir)
            .output()
            .unwrap()
    };
    let help = run("help");
    let flag = run("--help");
    assert!(help.status.success() && flag.status.success());
    assert!(help.stderr.is_empty() && flag.stderr.is_empty());
    assert_eq!(help.stdout, flag.stdout);
    assert!(String::from_utf8(help.stdout).unwrap().contains("--tray"));
    assert_eq!(socket.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    assert!(!run("probe").status.success());
    assert!(socket.accept().is_ok());
    fs::remove_file(dir.join("f75-studio.sock")).unwrap();
    fs::remove_dir(dir).unwrap();
}
