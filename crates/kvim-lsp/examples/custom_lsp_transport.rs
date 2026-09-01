//! Create restartable LSP transports from caller-owned stream endpoints.

use std::sync::{Arc, Mutex};

use kvim_lsp::{LspError, ServerProcess, Transport, TransportFactory};
use tokio::io::duplex;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), LspError> {
    let endpoints = Arc::new(Mutex::new(vec![duplex(64).0, duplex(64).0]));
    let mut factory = TransportFactory::Custom(Box::new(move || {
        let endpoint = endpoints
            .lock()
            .expect("the endpoint queue is not poisoned")
            .pop()
            .ok_or(LspError::NotInstalled)?;
        let (output, input) = tokio::io::split(endpoint);
        Ok(Transport::new(input, output))
    }));

    let (process, streams) = ServerProcess::open(&mut factory, |report| {
        eprintln!("{report:?}");
    })?;
    println!("connected with caller-supplied transport");
    drop(streams);
    process.close(kvim_lsp::ServerCloseIntent::Immediate).await;
    Ok(())
}
