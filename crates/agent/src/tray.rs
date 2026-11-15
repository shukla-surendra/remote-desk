//! macOS menu-bar frontend (Remote Desk.app).
//!
//! The Cocoa event loop owns the main thread; the agent runs on a Tokio runtime in a
//! background thread and pushes status updates through an `EventLoopProxy`.
//! Dialogs use `osascript`, with all text passed as `argv` (never spliced into the
//! script) so viewer-controlled strings can't inject AppleScript.

use std::{
    io::Write,
    process::{Command as Proc, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use tao::{
    event::{Event, StartCause},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::macos::{ActivationPolicy, EventLoopExtMacOS},
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{error, warn};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIconBuilder,
};

use crate::core::{format_id, Command, Frontend, Status, MIN_PASSWORD_LEN};

enum UserEvent {
    Status(Status),
    Menu(MenuEvent),
    Fatal(String),
}

struct TrayFrontend {
    proxy: Mutex<EventLoopProxy<UserEvent>>,
    consent_timeout: Duration,
}

impl Frontend for TrayFrontend {
    fn status(&self, status: &Status) {
        let _ = self
            .proxy
            .lock()
            .unwrap()
            .send_event(UserEvent::Status(status.clone()));
    }

    fn consent(&self, peer: String) -> BoxFuture<'static, bool> {
        let secs = self.consent_timeout.as_secs().to_string();
        Box::pin(async move {
            let msg = format!(
                "Allow a remote session from {peer}?\n\nThe viewer will be able to see and control this Mac."
            );
            let script = r#"on run argv
    activate
    set r to display dialog (item 1 of argv) with title "Remote Desk" buttons {"Deny", "Allow"} default button "Deny" cancel button "Deny" giving up after ((item 2 of argv) as integer) with icon caution
    if gave up of r then return "timeout"
    return button returned of r
end run"#;
            let out = tokio::process::Command::new("osascript")
                .args(["-e", script, "--", &msg, &secs])
                .output()
                .await;
            matches!(out, Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "Allow")
        })
    }
}

/// Show a blocking alert (usable before the event loop starts).
pub fn alert(message: &str) {
    let script = r#"on run argv
    activate
    display alert "Remote Desk" message (item 1 of argv) as critical
end run"#;
    let _ = Proc::new("osascript")
        .args(["-e", script, "--", message])
        .status();
}

fn prompt_password() -> Option<String> {
    let script = r#"on run argv
    activate
    set r to display dialog (item 1 of argv) default answer "" with hidden answer with title "Remote Desk" buttons {"Cancel", "Save"} default button "Save" cancel button "Cancel"
    return text returned of r
end run"#;
    let msg = format!(
        "Set a password for unattended access (at least {MIN_PASSWORD_LEN} characters).\n\nIt replaces the one-time password and is remembered across restarts."
    );
    let out = Proc::new("osascript")
        .args(["-e", script, "--", &msg])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let pw = String::from_utf8(out.stdout).ok()?;
    let pw = pw.strip_suffix('\n').unwrap_or(&pw).to_owned();
    (!pw.is_empty()).then_some(pw)
}

fn copy_to_clipboard(text: &str) {
    let child = Proc::new("pbcopy").stdin(Stdio::piped()).spawn();
    if let Ok(mut c) = child {
        if let Some(stdin) = c.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = c.wait();
    }
}

struct Items {
    menu: Menu,
    device_id: MenuItem,
    password: MenuItem,
    status: MenuItem,
    set_password: MenuItem,
    end_session: MenuItem,
    open_viewer: Option<MenuItem>,
    quit: MenuItem,
}

impl Items {
    fn new(viewer_url: Option<&str>) -> Self {
        let menu = Menu::new();
        let device_id = MenuItem::new("Device ID: connecting…", false, None);
        let password = MenuItem::new("Password: —", false, None);
        let status = MenuItem::new("Starting…", false, None);
        let set_password = MenuItem::new("Set Unattended Password…", true, None);
        let end_session = MenuItem::new("End Session", false, None);
        let open_viewer = viewer_url.map(|_| MenuItem::new("Open Web Viewer", true, None));
        let quit = MenuItem::new("Quit Remote Desk", true, None);
        let sep = || PredefinedMenuItem::separator();
        let _ = menu.append_items(&[
            &device_id,
            &password,
            &sep(),
            &status,
            &end_session,
            &sep(),
            &set_password,
        ]);
        if let Some(o) = &open_viewer {
            let _ = menu.append(o);
        }
        let _ = menu.append_items(&[&sep(), &quit]);
        Self {
            menu,
            device_id,
            password,
            status,
            set_password,
            end_session,
            open_viewer,
            quit,
        }
    }

    fn apply(&self, s: &Status) {
        match &s.device_id {
            Some(id) => {
                self.device_id
                    .set_text(format!("Device ID: {}   (click to copy)", format_id(id)));
                self.device_id.set_enabled(true);
            }
            None => {
                self.device_id.set_text("Device ID: connecting…");
                self.device_id.set_enabled(false);
            }
        }
        match &s.one_time_password {
            Some(p) => {
                self.password
                    .set_text(format!("Password: {p}   (click to copy)"));
                self.password.set_enabled(true);
            }
            None => {
                self.password.set_text("Password: unattended password set");
                self.password.set_enabled(false);
            }
        }
        let status = match (&s.session, s.online, &s.last_error) {
            (Some(sess), _, _) if sess.accepted => format!("● In session with {}", sess.peer),
            (Some(sess), _, _) => format!("Request from {} — waiting for approval", sess.peer),
            (None, true, _) => "● Ready for connections".to_owned(),
            (None, false, Some(e)) => format!("○ Offline — {}", truncate(e, 60)),
            (None, false, None) => "○ Connecting…".to_owned(),
        };
        self.status.set_text(status);
        self.end_session
            .set_enabled(s.session.as_ref().is_some_and(|x| x.accepted));
    }
}

/// Runs the menu-bar app; never returns. `start` runs on a background Tokio runtime.
pub fn run(
    start: impl FnOnce(Arc<dyn Frontend>) -> BoxFuture<'static, anyhow::Result<()>> + Send + 'static,
    commands: UnboundedSender<Command>,
    viewer_url: Option<String>,
) -> ! {
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory);

    let menu_proxy = Mutex::new(event_loop.create_proxy());
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.lock().unwrap().send_event(UserEvent::Menu(e));
    }));

    let frontend: Arc<dyn Frontend> = Arc::new(TrayFrontend {
        proxy: Mutex::new(event_loop.create_proxy()),
        consent_timeout: Duration::from_secs(30),
    });
    let fatal_proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        if let Err(e) = rt.block_on(start(frontend)) {
            error!("agent stopped: {e:#}");
            let _ = fatal_proxy.send_event(UserEvent::Fatal(format!("{e:#}")));
        }
    });

    let items = Items::new(viewer_url.as_deref());
    let mut current = Status::default();
    let mut tray = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // The status item must be created once the app has finished launching.
            Event::NewEvents(StartCause::Init) => {
                match TrayIconBuilder::new()
                    .with_menu(Box::new(items.menu.clone()))
                    .with_icon_templated(tray_icon())
                    .with_tooltip("Remote Desk")
                    .build()
                {
                    Ok(t) => tray = Some(t),
                    Err(e) => {
                        alert(&format!("Could not create the menu bar item: {e}"));
                        *control_flow = ControlFlow::Exit;
                    }
                }
            }
            Event::UserEvent(UserEvent::Status(s)) => {
                items.apply(&s);
                if let (Some(t), Some(id)) = (&tray, &s.device_id) {
                    let _ = t.set_tooltip(Some(format!("Remote Desk — {}", format_id(id))));
                }
                current = s;
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                let id: &MenuId = &e.id;
                if id == items.quit.id() {
                    *control_flow = ControlFlow::Exit;
                } else if id == items.device_id.id() {
                    if let Some(d) = &current.device_id {
                        copy_to_clipboard(d);
                    }
                } else if id == items.password.id() {
                    if let Some(p) = &current.one_time_password {
                        copy_to_clipboard(p);
                    }
                } else if id == items.end_session.id() {
                    let _ = commands.send(Command::EndSession);
                } else if id == items.set_password.id() {
                    let commands = commands.clone();
                    std::thread::spawn(move || {
                        if let Some(pw) = prompt_password() {
                            let _ = commands.send(Command::SetPassword(pw));
                        }
                    });
                } else if items.open_viewer.as_ref().is_some_and(|o| id == o.id()) {
                    if let Some(url) = &viewer_url {
                        if let Err(e) = Proc::new("open").arg(url).status() {
                            warn!("open viewer: {e}");
                        }
                    }
                }
            }
            Event::UserEvent(UserEvent::Fatal(msg)) => {
                alert(&format!("Remote Desk stopped: {msg}"));
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars()
            .take(max - 1)
            .chain(std::iter::once('…'))
            .collect()
    }
}

/// 44×44 (22pt @2x) template image: a monitor outline. macOS tints it for light/dark.
fn tray_icon() -> Icon {
    const N: u32 = 44;
    let mut px = vec![0u8; (N * N * 4) as usize];
    let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32| {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * N + x) * 4) as usize;
                px[i + 3] = 255;
            }
        }
    };
    // Screen bezel (outline, 3px).
    fill(4, 7, 40, 10);
    fill(4, 29, 40, 32);
    fill(4, 7, 7, 32);
    fill(37, 7, 40, 32);
    // Stand and base.
    fill(19, 32, 25, 36);
    fill(13, 36, 31, 39);
    Icon::from_rgba(px, N, N).expect("valid icon")
}
