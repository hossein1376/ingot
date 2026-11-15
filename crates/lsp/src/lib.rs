//! The Ingot language server (LSP.md, step L2).
//!
//! The adapter translates LSP messages to `ingot serve` request lines and back. It
//! links the driver and calls [`ingot_driver::serve::Server::handle`] in process, but
//! sees only the serve protocol: the boundary of tools.md §6 is what it builds and
//! parses. Positions, URIs and diagnostics live here; the compiler itself does not.

pub mod position;
pub mod server;
pub mod transport;
pub mod uri;

pub use ingot_diag::json;
pub use ingot_diag::json::Json;
pub use position::Encoding;
pub use server::Server;

use std::io::{Read, Write};
use std::sync::mpsc;

/// Run the server on `input`/`output` until end of input or an `exit` notification.
/// Returns the process exit status (0 after a `shutdown`, 1 for `exit` without one).
pub fn run<R: Read + Send + 'static, W: Write>(input: R, output: W) -> u8 {
    let (sender, receiver) = mpsc::channel();
    // Detached: on `exit` the process ends with the main thread even if the reader is
    // blocked on stdin. End of input ends the reader by itself.
    std::thread::spawn(move || transport::read_loop(input, sender));
    let mut writer = transport::MessageWriter::new(output);
    server_loop(&receiver, &mut writer)
}

fn server_loop<W: Write>(
    receiver: &mpsc::Receiver<transport::Incoming>,
    writer: &mut transport::MessageWriter<W>,
) -> u8 {
    let mut server = Server::new();
    loop {
        let first = match receiver.recv() {
            Ok(item) => item,
            Err(_) => return 0,
        };
        let mut batch = Vec::new();
        match first {
            transport::Incoming::Message(message) => batch.push(message),
            transport::Incoming::Malformed(message) => server.log(1, &message),
        }
        while let Ok(item) = receiver.try_recv() {
            match item {
                transport::Incoming::Message(message) => batch.push(message),
                transport::Incoming::Malformed(message) => server.log(1, &message),
            }
        }
        let exit = server.handle_batch(&batch);
        if writer.send_all(&server.take_outbox()).is_err() {
            return 1;
        }
        if let Some(status) = exit {
            return status;
        }
    }
}
