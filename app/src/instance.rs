//! One Transcribe per user: a second launch opens History in the first.
use crate::shell::UiEvent;
use futures::channel::mpsc::UnboundedSender;
use interprocess::local_socket::{
    GenericNamespaced, ListenerOptions, Name, Stream, ToNsName, prelude::*,
};
use std::io::{BufRead, BufReader, Write};

fn name() -> std::io::Result<Name<'static>> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    format!("app.transcribe.desktop.{user}.sock").to_ns_name::<GenericNamespaced>()
}

/// True when another instance took the request.
pub fn forward() -> bool {
    let Ok(name) = name() else { return false };
    let Ok(mut stream) = Stream::connect(name) else {
        return false;
    };
    stream.write_all(b"show\n").is_ok()
}

pub fn listen(events: UnboundedSender<UiEvent>) {
    let listener = match name().and_then(|name| ListenerOptions::new().name(name).create_sync()) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Single instance unavailable: {error}");
            return;
        }
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().filter_map(Result::ok) {
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok()
                && line.trim() == "show"
                && events.unbounded_send(UiEvent::Show).is_err()
            {
                return;
            }
        }
    });
}
