//! `vela-studio [WORKSPACE] [--port N] [--no-open]`: starts Vela Studio on
//! 127.0.0.1 for the workspace folder and opens it in the browser.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

use tokio::net::TcpListener;
use vela_studio::{Config, Studio, Token, router};

const USAGE: &str = "usage: vela-studio [WORKSPACE] [--port N] [--no-open]";

struct Options {
    workspace: PathBuf,
    port: u16,
    open: bool,
}

fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{}\n\nStarts Vela Studio for WORKSPACE (default: the current folder) on 127.0.0.1\n\
                      and opens it in the browser. The port is random unless --port is given.", USAGE);
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("error: {}\n{}", message, USAGE);
            return ExitCode::from(2);
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: cannot start: {}", error);
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(options));
    // Don't wait on blocking work that may never finish (a read stuck on a
    // file another program holds).
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {}", message);
            ExitCode::FAILURE
        }
    }
}

/// Returns `Ok(None)` when help was requested.
fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut workspace = None;
    let mut port = 0;
    let mut open = true;
    let mut only_folders = false;
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // After `--`, an argument is the folder even if it starts with `-`.
            "--" if !only_folders => only_folders = true,
            _ if only_folders && workspace.is_some() => return Err("only one workspace folder can be given".to_string()),
            _ if only_folders => workspace = Some(PathBuf::from(arg)),
            "-h" | "--help" => return Ok(None),
            "--no-open" => open = false,
            "--port" => {
                let value = args.next().ok_or("--port needs a number")?;
                port = value.parse().map_err(|_| format!("'{}' is not a port number", value))?;
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option '{}'", arg)),
            _ if workspace.is_some() => return Err("only one workspace folder can be given".to_string()),
            _ => workspace = Some(PathBuf::from(arg)),
        }
    }
    Ok(Some(Options { workspace: workspace.unwrap_or_else(|| PathBuf::from(".")), port, open }))
}

async fn run(options: Options) -> Result<(), String> {
    // Loopback only: there is deliberately no option to listen elsewhere.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, options.port))
        .await
        .map_err(|e| format!("cannot listen on 127.0.0.1:{}: {}", options.port, e))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let token = Token::generate().map_err(|e| e.to_string())?;
    let config = Config::new(options.workspace.clone(), port, token.clone());
    let studio = Studio::new(config).map_err(|e| format!("cannot open workspace '{}': {}", options.workspace.display(), e))?;

    // The token travels in the fragment, which browsers never send to servers.
    let url = format!("http://127.0.0.1:{}/#token={}", port, token.as_str());
    println!("Vela Studio is running for {}", studio.workspace_root().display());
    println!("  {}", url);
    println!("Press Ctrl+C to stop.");
    if options.open {
        // The browser gets a one-time code instead of the token: its command
        // line can be read by other users of this machine.
        let opened = studio
            .launch_code()
            .is_ok_and(|code| open_browser(&format!("http://127.0.0.1:{}/#launch={}", port, code)));
        if !opened {
            println!("(Couldn't open a browser; open the address above.)");
        }
    }

    let sessions = studio.sessions.clone();
    axum::serve(listener, router(studio))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            println!("\nStopping…");
            sessions.shutdown().await;
        })
        .await
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut terminate) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn open_browser(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(windows) {
        let mut command = Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    } else {
        Command::new("xdg-open")
    };
    match command.arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
        Ok(mut child) => {
            // Waited for, so it doesn't linger as a zombie while the server runs.
            std::thread::spawn(move || child.wait());
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn arguments_choose_the_workspace_port_and_browser() {
        let defaults = parse(&[]).unwrap().unwrap();
        assert_eq!((defaults.workspace, defaults.port, defaults.open), (PathBuf::from("."), 0, true));
        let given = parse(&["examples", "--port", "4000", "--no-open"]).unwrap().unwrap();
        assert_eq!((given.workspace, given.port, given.open), (PathBuf::from("examples"), 4000, false));
        assert!(parse(&["--help"]).unwrap().is_none());
        let dashed = parse(&["--no-open", "--", "-folder"]).unwrap().unwrap();
        assert_eq!((dashed.workspace, dashed.open), (PathBuf::from("-folder"), false));
    }

    #[test]
    fn bad_arguments_are_reported() {
        assert_eq!(parse(&["--port"]).err().unwrap(), "--port needs a number");
        assert_eq!(parse(&["--port", "99999"]).err().unwrap(), "'99999' is not a port number");
        assert_eq!(parse(&["--host", "0.0.0.0"]).err().unwrap(), "unknown option '--host'");
        assert_eq!(parse(&["a", "b"]).err().unwrap(), "only one workspace folder can be given");
    }
}
