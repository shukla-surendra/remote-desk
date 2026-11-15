//! Terminal frontend: prints ID/password, asks consent on stdin.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::future::BoxFuture;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    sync::mpsc,
};

use crate::core::{format_id, Frontend, Status};

pub struct CliFrontend {
    last: Mutex<Status>,
    timeout: Duration,
    /// One stdin reader for the process; prompts take turns.
    lines: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>,
}

impl CliFrontend {
    /// Must be called inside a Tokio runtime.
    pub fn new(consent_timeout: Duration) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Self {
            last: Mutex::new(Status::default()),
            timeout: consent_timeout,
            lines: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }
}

impl Frontend for CliFrontend {
    fn status(&self, s: &Status) {
        let mut last = self.last.lock().unwrap();
        if s.one_time_password != last.one_time_password {
            if let Some(p) = &s.one_time_password {
                println!("\n  One-time password: {p}");
            }
        }
        if s.device_id != last.device_id || (s.online && !last.online) {
            if let Some(id) = &s.device_id {
                println!("\n  Device ID: {}\n", format_id(id));
            }
        }
        *last = s.clone();
    }

    fn consent(&self, peer: String) -> BoxFuture<'static, bool> {
        let lines = self.lines.clone();
        let timeout = self.timeout;
        Box::pin(async move {
            let mut lines = lines.lock().await;
            while lines.try_recv().is_ok() {} // drop stale keystrokes
            println!(
                "Allow remote session from {peer}? [y/N] ({}s)",
                timeout.as_secs()
            );
            match tokio::time::timeout(timeout, lines.recv()).await {
                Ok(Some(l)) => l.trim().eq_ignore_ascii_case("y"),
                _ => {
                    println!("No answer; session rejected.");
                    false
                }
            }
        })
    }
}
