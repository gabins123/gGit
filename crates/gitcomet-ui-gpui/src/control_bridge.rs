//! Dev-only control bridge: lets an agent drive the running app and capture it
//! without OS input or focus changes. See `docs/control-bridge.md`.
//!
//! Debug builds only (the module and its call site are
//! `cfg(debug_assertions)`) and opt-in through `GITCOMET_CONTROL_DIR`. The
//! directory is polled for `<name>.cmd` files; each is executed on the UI
//! thread against the main window and answered with `<name>.result`.

use crate::view::GitCometView;
use futures::channel::oneshot;
use futures::future::{self, Either};
use gpui::{AnyWindowHandle, App, AppContext, AsyncApp, Keystroke, WindowHandle};
use std::path::{Path, PathBuf};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Lets the store's reducer thread and the UI poller apply the state change a
/// keystroke caused before the next one reads it (the test harness gets this
/// from `run_until_parked`).
const KEY_SETTLE: Duration = Duration::from_millis(75);
const MAX_WAIT_MS: u64 = 30_000;
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// A command is one short line; anything larger is refused unread.
const MAX_COMMAND_BYTES: u64 = 64 * 1024;
/// `PrintWindow` reads DWM's composed surface, which trails `present()` slightly.
const PRESENT_SETTLE: Duration = Duration::from_millis(50);

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Keys(Vec<String>),
    Screenshot(String),
    State,
    Wait(u64),
}

/// `ok <summary>` plus optional payload lines, or an error message.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    summary: String,
    payload: Option<String>,
}

impl Outcome {
    fn summary(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            payload: None,
        }
    }
}

type CommandResult = Result<Outcome, String>;

fn parse_command(text: &str) -> Result<Command, String> {
    let mut words = text.split_whitespace();
    let name = words.next().ok_or("empty command")?;
    let args: Vec<&str> = words.collect();
    match (name, args.as_slice()) {
        ("keys", []) => Err("keys needs at least one keystroke".into()),
        ("keys", tokens) => Ok(Command::Keys(
            tokens.iter().map(|t| t.to_string()).collect(),
        )),
        ("screenshot", [path]) => Ok(Command::Screenshot(path.to_string())),
        ("screenshot", _) => Err("usage: screenshot <path> (no spaces in the path)".into()),
        ("state", []) => Ok(Command::State),
        ("state", _) => Err("state takes no arguments".into()),
        ("wait", [ms]) => ms
            .parse()
            .map(Command::Wait)
            .map_err(|_| format!("wait: invalid milliseconds '{ms}'")),
        ("wait", _) => Err("usage: wait <ms>".into()),
        (other, _) => Err(format!("unknown command '{other}'")),
    }
}

fn format_result(result: &CommandResult) -> String {
    match result {
        Ok(outcome) => {
            let mut text = String::from("ok");
            if !outcome.summary.is_empty() {
                text.push(' ');
                text.push_str(&outcome.summary);
            }
            text.push('\n');
            if let Some(payload) = &outcome.payload {
                text.push_str(payload);
                text.push('\n');
            }
            text
        }
        // The first line is the verdict; keep messages on one line.
        Err(message) => format!("error: {}\n", message.replace(['\r', '\n'], " ")),
    }
}

fn parse_keys(tokens: &[String]) -> Result<Vec<Keystroke>, String> {
    tokens
        .iter()
        .map(|token| {
            Keystroke::parse(token).map_err(|err| format!("invalid keystroke '{token}': {err}"))
        })
        .collect()
}

/// `window.dispatch_keystroke` without leasing the root view, so handlers can
/// update it. `Ok(false)` means nothing handled the key.
fn dispatch_one(
    window: AnyWindowHandle,
    cx: &mut impl AppContext,
    keystroke: Keystroke,
) -> Result<bool, String> {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_keystroke(keystroke, cx)
    })
    .map_err(|err| format!("window unavailable: {err}"))
}

fn keys_outcome(total: usize, unhandled: &[&str]) -> Outcome {
    let mut summary = format!("dispatched {total}");
    if !unhandled.is_empty() {
        summary.push_str(&format!(" (unhandled: {})", unhandled.join(" ")));
    }
    Outcome::summary(summary)
}

/// Dispatches every token back to back. The live bridge settles between keys
/// instead; tests sync the store snapshot themselves.
#[cfg(test)]
fn keys_unsettled(
    window: AnyWindowHandle,
    cx: &mut impl AppContext,
    tokens: &[String],
) -> CommandResult {
    let keystrokes = parse_keys(tokens)?;
    let mut unhandled = Vec::new();
    for (token, keystroke) in tokens.iter().zip(keystrokes) {
        if !dispatch_one(window, cx, keystroke)? {
            unhandled.push(token.as_str());
        }
    }
    Ok(keys_outcome(tokens.len(), &unhandled))
}

async fn keys_settled(
    window: AnyWindowHandle,
    cx: &mut AsyncApp,
    tokens: &[String],
) -> CommandResult {
    let keystrokes = parse_keys(tokens)?;
    let mut unhandled = Vec::new();
    for (token, keystroke) in tokens.iter().zip(keystrokes) {
        if !dispatch_one(window, cx, keystroke)? {
            unhandled.push(token.as_str());
        }
        cx.background_executor().timer(KEY_SETTLE).await;
    }
    Ok(keys_outcome(tokens.len(), &unhandled))
}

fn state(window: AnyWindowHandle, cx: &mut impl AppContext) -> CommandResult {
    cx.update_window(window, |root, window, cx| {
        let view = root
            .downcast::<GitCometView>()
            .map_err(|_| "root view is not GitCometView".to_string())?;
        Ok(view.update(cx, |view, cx| view.control_bridge_state(window, cx)))
    })
    .map_err(|err| format!("window unavailable: {err}"))?
    .map(|json| Outcome {
        summary: String::new(),
        payload: Some(json.to_string()),
    })
}

type Pixels = (u32, u32, Vec<u8>); // width, height, RGBA

/// The window's Win32 handle. Screenshots capture the window's own pixels with
/// `PrintWindow`; other platforms are not implemented yet.
#[cfg(target_os = "windows")]
fn window_hwnd(window: AnyWindowHandle, cx: &mut impl AppContext) -> Result<isize, String> {
    cx.update_window(window, |_, window, _| crate::app::window_hwnd(window))
        .map_err(|err| format!("window unavailable: {err}"))?
        .ok_or_else(|| "window has no Win32 handle".to_string())
}

#[cfg(not(target_os = "windows"))]
fn window_hwnd(_: AnyWindowHandle, _: &mut impl AppContext) -> Result<isize, String> {
    Err("screenshot is only implemented on Windows".into())
}

/// Blocking; `PrintWindow` messages the window's own thread, so call it off the
/// UI thread (or with the UI thread free to pump messages).
#[cfg(target_os = "windows")]
fn grab(hwnd: isize) -> Result<Pixels, String> {
    gitcomet_win32_window_utils::capture_window(hwnd).map(|p| (p.width, p.height, p.rgba))
}

#[cfg(not(target_os = "windows"))]
fn grab(_: isize) -> Result<Pixels, String> {
    Err("screenshot is only implemented on Windows".into())
}

fn save_png((width, height, rgba): Pixels, path: &Path) -> CommandResult {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let image = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "captured pixel buffer has the wrong size".to_string())?;
    image
        .save_with_format(path, image::ImageFormat::Png)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(Outcome::summary(format!(
        "{width}x{height} {}",
        path.display()
    )))
}

/// Captures the window as it is now, blocking. For tests.
#[cfg(test)]
fn capture(window: AnyWindowHandle, cx: &mut impl AppContext, path: &Path) -> CommandResult {
    save_png(grab(window_hwnd(window, cx)?)?, path)
}

/// Completes once a frame that started drawing after this call has finished.
/// Frame callbacks run before their frame draws, so the callback is chained:
/// the refresh dirties the window, frame N draws the current state, and the
/// inner callback fires at the start of frame N + 1 with N as the rendered
/// frame.
async fn after_fresh_frame(window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<(), String> {
    let (tx, rx) = oneshot::channel();
    cx.update_window(window, |_, window, _| {
        window.refresh();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, _| {
                let _ = tx.send(());
            });
        });
    })
    .map_err(|err| format!("window unavailable: {err}"))?;
    match future::select(rx, cx.background_executor().timer(FRAME_TIMEOUT)).await {
        Either::Left((Ok(()), _)) => Ok(()),
        Either::Left((Err(_), _)) => Err("window closed before the next frame".into()),
        Either::Right(_) => Err("timed out waiting for a frame (window minimized?)".into()),
    }
}

async fn screenshot(window: AnyWindowHandle, cx: &mut AsyncApp, path: &Path) -> CommandResult {
    after_fresh_frame(window, cx).await?;
    let hwnd = window_hwnd(window, cx)?;
    // Let DWM compose the frame that was just presented.
    cx.background_executor().timer(PRESENT_SETTLE).await;
    // Capture and PNG encode/write both stay off the UI thread.
    let path = path.to_path_buf();
    cx.background_executor()
        .spawn(async move { save_png(grab(hwnd)?, &path) })
        .await
}

async fn execute(
    window: AnyWindowHandle,
    dir: &Path,
    cx: &mut AsyncApp,
    command: Command,
) -> CommandResult {
    match command {
        Command::Keys(tokens) => keys_settled(window, cx, &tokens).await,
        Command::State => state(window, cx),
        Command::Wait(ms) => {
            let ms = ms.min(MAX_WAIT_MS);
            cx.background_executor()
                .timer(Duration::from_millis(ms))
                .await;
            Ok(Outcome::summary(format!("waited {ms}ms")))
        }
        Command::Screenshot(path) => screenshot(window, cx, &dir.join(path)).await,
    }
}

/// Executes one command line synchronously and returns the formatted result,
/// for tests: keys are not settled between strokes, screenshots capture the
/// current rendered frame without waiting for a new one, and `wait` is a no-op.
#[cfg(test)]
pub(crate) fn run_for_test(
    window: AnyWindowHandle,
    cx: &mut impl AppContext,
    line: &str,
) -> String {
    let result = parse_command(line).and_then(|command| match command {
        Command::Keys(tokens) => keys_unsettled(window, cx, &tokens),
        Command::State => state(window, cx),
        Command::Wait(ms) => Ok(Outcome::summary(format!("waited {ms}ms"))),
        Command::Screenshot(path) => capture(window, cx, Path::new(&path)),
    });
    format_result(&result)
}

/// The command file's text, refusing one over [`MAX_COMMAND_BYTES`].
fn read_command(path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_COMMAND_BYTES + 1).read_to_string(&mut text))
        .map_err(|err| format!("cannot read command file: {err}"))?;
    if text.len() as u64 > MAX_COMMAND_BYTES {
        return Err(format!("command file is over {MAX_COMMAND_BYTES} bytes"));
    }
    Ok(text)
}

/// `*.cmd` files in name order.
fn pending_commands(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "cmd") && path.is_file())
        .collect();
    paths.sort();
    paths
}

/// Temp name then rename, so a reader never sees a partial result.
fn write_result(cmd_path: &Path, text: &str) -> std::io::Result<()> {
    let result = cmd_path.with_extension("result");
    let temp = cmd_path.with_extension("result.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, &result)
}

async fn poll(window: AnyWindowHandle, dir: PathBuf, cx: &mut AsyncApp) {
    loop {
        cx.background_executor().timer(POLL_INTERVAL).await;
        if cx.update_window(window, |_, _, _| ()).is_err() {
            return; // the main window is gone
        }
        for cmd_path in pending_commands(&dir) {
            let result = match read_command(&cmd_path) {
                Err(message) => Err(message),
                Ok(text) => match parse_command(&text) {
                    Err(message) => Err(message),
                    Ok(command) => execute(window, &dir, cx, command).await,
                },
            };
            if let Err(err) = write_result(&cmd_path, &format_result(&result)) {
                eprintln!("control bridge: cannot write result for {cmd_path:?}: {err}");
            }
            // Always consume the command, or a failure would rerun it forever.
            let _ = std::fs::remove_file(&cmd_path);
        }
    }
}

/// Starts polling `GITCOMET_CONTROL_DIR`, if set. No-op otherwise.
pub(crate) fn start(window: WindowHandle<GitCometView>, cx: &mut App) {
    let Some(dir) = std::env::var_os("GITCOMET_CONTROL_DIR").filter(|dir| !dir.is_empty()) else {
        return;
    };
    // Later windows (File > New Window) are not bridged: one poller per directory.
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let dir = std::path::absolute(&dir).unwrap_or_else(|_| PathBuf::from(dir));
    if let Err(err) = std::fs::create_dir_all(&dir) {
        eprintln!("control bridge: cannot create {}: {err}", dir.display());
        return;
    }
    eprintln!("control bridge: watching {}", dir.display());
    let window = AnyWindowHandle::from(window);
    cx.spawn(async move |cx| poll(window, dir, cx).await)
        .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(tokens: &[&str]) -> Command {
        Command::Keys(tokens.iter().map(|t| t.to_string()).collect())
    }

    #[test]
    fn parses_valid_commands() {
        assert_eq!(
            parse_command("keys 1 ] shift-j\n"),
            Ok(keys(&["1", "]", "shift-j"]))
        );
        assert_eq!(
            parse_command("  screenshot   shot.png "),
            Ok(Command::Screenshot("shot.png".into()))
        );
        assert_eq!(parse_command("state"), Ok(Command::State));
        assert_eq!(parse_command("wait 250"), Ok(Command::Wait(250)));
    }

    #[test]
    fn rejects_malformed_commands() {
        for bad in [
            "",
            "   \n",
            "keys",
            "state now",
            "screenshot",
            "screenshot a b",
            "wait",
            "wait soon",
            "wait -1",
            "dance",
        ] {
            assert!(parse_command(bad).is_err(), "{bad:?} should be rejected");
        }
        assert_eq!(
            parse_command("dance 1"),
            Err("unknown command 'dance'".into())
        );
        assert_eq!(parse_command(""), Err("empty command".into()));
    }

    #[test]
    fn invalid_keystrokes_are_reported_not_panicked() {
        let err = parse_keys(&["1".into(), "a-b".into()]).unwrap_err();
        assert!(err.starts_with("invalid keystroke 'a-b'"), "{err}");
        assert_eq!(
            parse_keys(&["enter".into(), "shift-j".into()])
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn formats_results() {
        assert_eq!(format_result(&Ok(Outcome::summary(""))), "ok\n");
        assert_eq!(
            format_result(&Ok(keys_outcome(3, &["x", "y"]))),
            "ok dispatched 3 (unhandled: x y)\n"
        );
        let with_payload = Outcome {
            summary: String::new(),
            payload: Some("{\"a\":1}".into()),
        };
        assert_eq!(format_result(&Ok(with_payload)), "ok\n{\"a\":1}\n");
        assert_eq!(
            format_result(&Err("bad\nthing".into())),
            "error: bad thing\n"
        );
    }

    #[test]
    fn pending_commands_are_sorted_and_filtered() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["b.cmd", "a.cmd", "a.result", "c.txt", "d.cmd.tmp"] {
            std::fs::write(dir.path().join(name), "state").unwrap();
        }
        std::fs::create_dir(dir.path().join("z.cmd")).unwrap();
        let names: Vec<_> = pending_commands(dir.path())
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.cmd", "b.cmd"]);
    }

    #[test]
    fn an_oversize_command_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("big.cmd");
        std::fs::write(&cmd, format!("keys {}", "a ".repeat(40_000))).unwrap();
        let err = read_command(&cmd).unwrap_err();
        assert!(err.contains("over 65536 bytes"), "{err}");
        assert!(format_result(&Err(err)).starts_with("error: "));
        std::fs::write(&cmd, "state\n").unwrap();
        assert_eq!(read_command(&cmd).unwrap(), "state\n");
    }

    #[test]
    fn result_is_written_atomically_next_to_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("0001.cmd");
        write_result(&cmd, "ok\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("0001.result")).unwrap(),
            "ok\n"
        );
        assert!(!dir.path().join("0001.result.tmp").exists());
    }
}
