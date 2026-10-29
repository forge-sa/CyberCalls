//! Watches Hyprland for the Telegram call window. Its title is the peer's display name.

use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::Msg;

#[derive(Debug)]
pub enum WindowEvent {
    Opened { addr: String, title: String },
    Retitled { addr: String, title: String },
    Closed { addr: String },
}

fn socket_dir() -> Result<PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("not running under Hyprland")?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR unset")?;
    Ok(PathBuf::from(runtime).join("hypr").join(sig))
}

/// The main window is titled "AyuGram", "AyuGram (3)" or "<chat> – (3)"; the call window is just
/// the peer's name. Separate chat windows can look the same, so callers also require call audio.
fn is_candidate(title: &str) -> bool {
    let t = clean_title(title);
    !(t.is_empty()
        || t.contains("AyuGram")
        || t.contains("Telegram")
        || t.contains(" – (")
        || t == "Media viewer")
}

/// Strips the bidi isolation marks Qt puts around names.
pub fn clean_title(title: &str) -> String {
    title
        .chars()
        .filter(|c| !matches!(c, '\u{200e}' | '\u{200f}' | '\u{2066}'..='\u{2069}'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Blocks forever, forwarding call-window events for `class`.
pub fn run(class: String, tx: Sender<Msg>) -> Result<()> {
    let dir = socket_dir()?;

    // windows that were already open before we started (e.g. restarted mid-call)
    let mut req = UnixStream::connect(dir.join(".socket.sock")).context("hyprland request socket")?;
    req.write_all(b"j/clients")?;
    let mut reply = String::new();
    req.read_to_string(&mut reply)?;
    let clients: Vec<serde_json::Value> = serde_json::from_str(&reply).context("parsing clients")?;
    let mut ours: Vec<String> = Vec::new();
    for c in clients {
        if c["class"] == class.as_str() {
            let addr = c["address"].as_str().unwrap_or_default().trim_start_matches("0x").to_owned();
            let title = c["title"].as_str().unwrap_or_default().to_owned();
            ours.push(addr.clone());
            if is_candidate(&title) {
                tx.send(Msg::Window(WindowEvent::Opened { addr, title: clean_title(&title) }))?;
            }
        }
    }

    let events = UnixStream::connect(dir.join(".socket2.sock")).context("hyprland event socket")?;
    for line in BufReader::new(events).lines() {
        let line = line?;
        let Some((ev, data)) = line.split_once(">>") else { continue };
        match ev {
            "openwindow" => {
                // ADDR,WORKSPACE,CLASS,TITLE — the title may itself contain commas
                let mut f = data.splitn(4, ',');
                let (Some(addr), Some(_), Some(cls), Some(title)) = (f.next(), f.next(), f.next(), f.next()) else {
                    continue;
                };
                if cls != class {
                    continue;
                }
                ours.push(addr.to_owned());
                if is_candidate(title) {
                    tx.send(Msg::Window(WindowEvent::Opened { addr: addr.into(), title: clean_title(title) }))?;
                }
            }
            "windowtitlev2" => {
                let Some((addr, title)) = data.split_once(',') else { continue };
                if ours.iter().any(|a| a == addr) {
                    let ev = if is_candidate(title) {
                        WindowEvent::Retitled { addr: addr.into(), title: clean_title(title) }
                    } else {
                        WindowEvent::Closed { addr: addr.into() }
                    };
                    tx.send(Msg::Window(ev))?;
                }
            }
            "closewindow" => {
                if let Some(i) = ours.iter().position(|a| a == data) {
                    ours.swap_remove(i);
                    tx.send(Msg::Window(WindowEvent::Closed { addr: data.into() }))?;
                }
            }
            _ => {}
        }
    }
    anyhow::bail!("hyprland event socket closed")
}
