//! Integration tests for the msgpack-RPC client against a real `nvim --clean --embed`.
//!
//! The crate is a binary, so the module under test is included by path. Every
//! test returns early (with a printed skip) when `nvim` is not on PATH.
#![allow(dead_code)]

#[path = "../src/nvim/rpc.rs"]
mod rpc;

use rmpv::Value;
use rpc::{Incoming, Rpc};
use std::path::Path;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(5);

fn nvim_available() -> bool {
    std::process::Command::new("nvim")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Spawn a clean embedded nvim, or `None` (with a printed skip) without nvim.
fn spawn() -> Option<Rpc> {
    if !nvim_available() {
        eprintln!("skip: nvim not on PATH");
        return None;
    }
    let cwd = std::env::temp_dir();
    Some(Rpc::spawn("nvim", "", Path::new(&cwd), &["--clean"]).expect("spawn nvim"))
}

fn s(text: &str) -> Value {
    Value::from(text)
}

/// Poll `try_recv` until `pick` accepts a message or `limit` passes.
fn wait_for<R>(
    rpc: &Rpc,
    limit: Duration,
    mut pick: impl FnMut(Incoming) -> Option<R>,
) -> Option<R> {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        match rpc.try_recv() {
            Some(msg) => {
                if let Some(found) = pick(msg) {
                    return Some(found);
                }
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    None
}

fn channel_id(rpc: &Rpc) -> i64 {
    let info = rpc
        .request("nvim_get_api_info", vec![], T)
        .expect("api info");
    info.as_array().unwrap()[0].as_i64().unwrap()
}

fn signal(pid: i64, sig: &str) {
    std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .status()
        .expect("kill");
}

fn pid_of(rpc: &Rpc) -> i64 {
    rpc.request("nvim_eval", vec![s("getpid()")], T)
        .expect("pid")
        .as_i64()
        .unwrap()
}

/// Kills `pid` with SIGKILL after `after` unless dropped first, so a regression
/// that blocks on a stalled child fails the test instead of hanging it.
struct Watchdog {
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn new(pid: i64, after: Duration) -> Watchdog {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancel);
        let handle = std::thread::spawn(move || {
            let end = Instant::now() + after;
            while Instant::now() < end {
                if flag.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            signal(pid, "-9");
        });
        Watchdog {
            cancel,
            handle: Some(handle),
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Larger than any pipe buffer (64 KiB by default on Linux).
fn big() -> Value {
    s(&"x".repeat(256 * 1024))
}

#[test]
fn rpc_eval_returns_value() {
    let Some(rpc) = spawn() else { return };
    let v = rpc.request("nvim_eval", vec![s("1+41")], T).expect("eval");
    assert_eq!(v.as_i64(), Some(42));
}

#[test]
fn rpc_error_is_err() {
    let Some(rpc) = spawn() else { return };
    let err = rpc
        .request("nvim_command", vec![s("bogus")], T)
        .expect_err("bogus command must fail");
    assert!(err.to_string().contains("E492"), "got: {err}");
}

#[test]
fn rpc_notification_arrives() {
    let Some(rpc) = spawn() else { return };
    let chan = channel_id(&rpc);
    let lua = format!("vim.rpcnotify({chan}, 'ping', 7)");
    rpc.request("nvim_exec_lua", vec![s(&lua), Value::Array(vec![])], T)
        .expect("exec_lua");
    let got = wait_for(&rpc, T, |m| match m {
        Incoming::Notification { method, params } if method == "ping" => Some(params),
        _ => None,
    })
    .expect("ping notification");
    assert_eq!(got, vec![Value::from(7)]);
}

#[test]
fn rpc_request_from_nvim_is_answered() {
    let Some(rpc) = spawn() else { return };
    let chan = channel_id(&rpc);
    let lua =
        format!("vim.defer_fn(function() vim.g.answer = vim.rpcrequest({chan}, 'ask') end, 10)");
    rpc.request("nvim_exec_lua", vec![s(&lua), Value::Array(vec![])], T)
        .expect("exec_lua");
    let id = wait_for(&rpc, T, |m| match m {
        Incoming::Request { id, method, .. } if method == "ask" => Some(id),
        _ => None,
    })
    .expect("request from nvim");
    rpc.respond(id, Ok(s("pong"))).expect("respond");
    let end = Instant::now() + T;
    loop {
        if let Ok(v) = rpc.request("nvim_get_var", vec![s("answer")], T) {
            assert_eq!(v.as_str(), Some("pong"));
            break;
        }
        assert!(Instant::now() < end, "nvim never received the answer");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn rpc_error_response_to_nvim_request_is_delivered() {
    let Some(rpc) = spawn() else { return };
    let chan = channel_id(&rpc);
    let lua = format!(
        "vim.defer_fn(function() local ok, err = pcall(vim.rpcrequest, {chan}, 'ask'); \
         vim.g.ok = ok; vim.g.err = tostring(err) end, 10)"
    );
    rpc.request("nvim_exec_lua", vec![s(&lua), Value::Array(vec![])], T)
        .expect("exec_lua");
    let id = wait_for(&rpc, T, |m| match m {
        Incoming::Request { id, .. } => Some(id),
        _ => None,
    })
    .expect("request from nvim");
    rpc.respond(id, Err("nope".into())).expect("respond");
    let end = Instant::now() + T;
    loop {
        if let Ok(v) = rpc.request("nvim_get_var", vec![s("err")], T) {
            assert!(v.as_str().unwrap().contains("nope"), "got {v:?}");
            break;
        }
        assert!(Instant::now() < end, "nvim never received the error");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn rpc_child_exit_closes_channel() {
    let Some(rpc) = spawn() else { return };
    let waker = rpc.waker();
    // nvim quits before it can answer; the outcome of this call is irrelevant.
    let _ = rpc.request("nvim_command", vec![s("qall!")], Duration::from_secs(2));
    let code = wait_for(&rpc, Duration::from_secs(2), |m| match m {
        Incoming::Exited(code) => Some(code),
        _ => None,
    })
    .expect("Exited within 2 s");
    assert_eq!(code, Some(0));
    assert!(waker.take(), "waker must have been raised by the exit");
    let started = Instant::now();
    assert!(rpc.request("nvim_eval", vec![s("1")], T).is_err());
    assert!(rpc.notify("nvim_eval", vec![s("1")]).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "request hung after exit"
    );
}

#[test]
fn rpc_killed_child_reports_exit_without_code() {
    let Some(rpc) = spawn() else { return };
    let pid = rpc
        .request("nvim_eval", vec![s("getpid()")], T)
        .expect("pid")
        .as_i64()
        .unwrap();
    std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .expect("kill");
    let code = wait_for(&rpc, Duration::from_secs(2), |m| match m {
        Incoming::Exited(code) => Some(code),
        _ => None,
    })
    .expect("Exited within 2 s");
    assert_eq!(code, None, "a signal death has no exit code");
}

#[test]
fn rpc_pending_request_fails_when_child_exits() {
    let Some(rpc) = spawn() else { return };
    // Blocks nvim for 10 s; the request must fail on kill, not wait for its timeout.
    let pid = rpc
        .request("nvim_eval", vec![s("getpid()")], T)
        .expect("pid")
        .as_i64()
        .unwrap();
    std::thread::scope(|scope| {
        let killer = scope.spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status()
                .expect("kill");
        });
        let started = Instant::now();
        let res = rpc.request("nvim_command", vec![s("sleep 10")], Duration::from_secs(8));
        assert!(res.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "request waited for its timeout"
        );
        killer.join().unwrap();
    });
}

#[test]
fn rpc_request_times_out() {
    let Some(rpc) = spawn() else { return };
    let started = Instant::now();
    let err = rpc
        .request(
            "nvim_command",
            vec![s("sleep 3")],
            Duration::from_millis(100),
        )
        .expect_err("must time out");
    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn rpc_shutdown_stops_nvim() {
    let Some(rpc) = spawn() else { return };
    let pid = rpc
        .request("nvim_eval", vec![s("getpid()")], T)
        .expect("pid")
        .as_i64()
        .unwrap();
    let started = Instant::now();
    rpc.shutdown(Duration::from_secs(3));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "nvim process still alive after shutdown"
    );
}

#[test]
fn rpc_shutdown_kills_unresponsive_nvim() {
    let Some(rpc) = spawn() else { return };
    let pid = rpc
        .request("nvim_eval", vec![s("getpid()")], T)
        .expect("pid")
        .as_i64()
        .unwrap();
    // Busy nvim cannot process qall!; shutdown must fall back to kill.
    rpc.notify("nvim_command", vec![s("sleep 30")])
        .expect("notify");
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    rpc.shutdown(Duration::from_millis(500));
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "nvim survived shutdown"
    );
}

#[test]
fn rpc_nvim_appname_is_set_only_when_non_empty() {
    if !nvim_available() {
        eprintln!("skip: nvim not on PATH");
        return;
    }
    let cwd = std::env::temp_dir();
    let named = Rpc::spawn("nvim", "nvim-notes-test", &cwd, &["--clean"]).expect("spawn");
    let v = named
        .request("nvim_eval", vec![s("$NVIM_APPNAME")], T)
        .expect("eval");
    assert_eq!(v.as_str(), Some("nvim-notes-test"));
    // An inherited NVIM_APPNAME would make the second half meaningless.
    if std::env::var_os("NVIM_APPNAME").is_some() {
        return;
    }
    let plain = Rpc::spawn("nvim", "", &cwd, &["--clean"]).expect("spawn");
    let v = plain
        .request("nvim_eval", vec![s("exists('$NVIM_APPNAME')")], T)
        .expect("eval");
    assert_eq!(
        v.as_i64(),
        Some(0),
        "empty appname must leave the variable unset"
    );
}

#[test]
fn rpc_spawn_missing_binary_is_err() {
    let cwd = std::env::temp_dir();
    let res = Rpc::spawn("definitely-not-a-real-nvim-binary", "", &cwd, &[]);
    assert!(res.is_err());
}

#[test]
fn rpc_stderr_is_kept_for_error_messages() {
    let Some(rpc) = spawn() else { return };
    rpc.request(
        "nvim_exec_lua",
        vec![s("io.stderr:write('boom\\n')"), Value::Array(vec![])],
        T,
    )
    .expect("exec_lua");
    let end = Instant::now() + T;
    while !rpc.stderr_tail().contains("boom") {
        assert!(Instant::now() < end, "stderr never arrived");
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = rpc.request("nvim_command", vec![s("qall!")], Duration::from_secs(2));
    wait_for(&rpc, Duration::from_secs(2), |m| {
        matches!(m, Incoming::Exited(_)).then_some(())
    })
    .expect("exit");
    let err = rpc.request("nvim_eval", vec![s("1")], T).unwrap_err();
    assert!(err.to_string().contains("boom"), "got: {err}");
}

#[test]
fn rpc_request_times_out_when_stdin_is_full() {
    let Some(rpc) = spawn() else { return };
    let pid = pid_of(&rpc);
    signal(pid, "-STOP");
    let _dog = Watchdog::new(pid, Duration::from_millis(3000));
    let started = Instant::now();
    let err = rpc
        .request("nvim_eval", vec![big()], Duration::from_millis(100))
        .expect_err("stalled nvim cannot answer");
    let took = started.elapsed();
    assert!(err.to_string().contains("timed out"), "got: {err}");
    assert!(took < Duration::from_millis(1000), "request took {took:?}");
}

#[test]
fn rpc_notify_does_not_block_when_stdin_is_full() {
    let Some(rpc) = spawn() else { return };
    let pid = pid_of(&rpc);
    signal(pid, "-STOP");
    let _dog = Watchdog::new(pid, Duration::from_millis(3000));
    let started = Instant::now();
    rpc.notify("nvim_eval", vec![big()]).expect("queued");
    rpc.respond(7, Ok(big())).expect("queued");
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn rpc_backlog_for_a_stalled_nvim_is_bounded() {
    let Some(rpc) = spawn() else { return };
    let pid = pid_of(&rpc);
    signal(pid, "-STOP");
    let _dog = Watchdog::new(pid, Duration::from_millis(4000));
    let started = Instant::now();
    let mut refused = None;
    for n in 0..256 {
        let payload = s(&"x".repeat(1024 * 1024));
        if let Err(e) = rpc.notify("nvim_eval", vec![payload]) {
            refused = Some((n, e.to_string()));
            break;
        }
    }
    let (n, msg) = refused.expect("a stalled nvim must eventually refuse more input");
    assert!(n < 64, "accepted {n} MiB before refusing");
    assert!(msg.contains("not accepting input"), "got: {msg}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn rpc_shutdown_is_bounded_when_stdin_is_full() {
    let Some(rpc) = spawn() else { return };
    let pid = pid_of(&rpc);
    signal(pid, "-STOP");
    let _dog = Watchdog::new(pid, Duration::from_millis(4000));
    // Fill the pipe so the qall! queued by shutdown cannot be written either.
    let started = Instant::now();
    let _ = rpc.notify("nvim_eval", vec![big()]);
    rpc.shutdown(Duration::from_millis(300));
    let took = started.elapsed();
    assert!(took < Duration::from_millis(1500), "shutdown took {took:?}");
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "stalled nvim survived shutdown"
    );
}

#[test]
fn rpc_stream_stays_intact_after_a_timed_out_large_request() {
    let Some(rpc) = spawn() else { return };
    let pid = pid_of(&rpc);
    signal(pid, "-STOP");
    let _dog = Watchdog::new(pid, Duration::from_millis(6000));
    let err = rpc
        .request("nvim_eval", vec![big()], Duration::from_millis(100))
        .expect_err("stalled");
    assert!(err.to_string().contains("timed out"), "got: {err}");
    signal(pid, "-CONT");
    // The abandoned frame is still delivered whole, so the next call works.
    let v = rpc
        .request("nvim_eval", vec![s("1+1")], T)
        .expect("stream corrupted by the abandoned request");
    assert_eq!(v.as_i64(), Some(2));
}
