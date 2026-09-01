use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use kvim_lsp::{
    LaunchedServer, ServerLaunchRequest, ServerLauncher, ServerProcessHandle, ServerTerminate,
    ServerWait, TransportFactory, WorkspaceRoot,
};
use tokio::io::duplex;

struct Lifecycle(Arc<AtomicBool>);

impl Drop for Lifecycle {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

impl ServerProcessHandle for Lifecycle {
    fn wait(&mut self) -> ServerWait {
        Box::pin(async {
            #[cfg(unix)]
            return Ok(ExitStatus::from_raw(0));
            #[cfg(not(unix))]
            compile_error!("kvim supports macOS and Linux");
        })
    }

    fn terminate(&mut self) -> ServerTerminate<'_> {
        Box::pin(async { Ok(()) })
    }
}

struct Launcher(Arc<AtomicBool>);

impl ServerLauncher for Launcher {
    fn launch(
        &mut self,
        _request: &ServerLaunchRequest,
    ) -> Result<LaunchedServer, kvim_lsp::ServerLaunchError> {
        let (input, _) = duplex(64);
        let (output, _) = duplex(64);
        let (errors, _) = duplex(64);
        Ok(LaunchedServer::new(input, output, errors, Lifecycle(Arc::clone(&self.0))))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = WorkspaceRoot::new(std::env::current_dir()?.canonicalize()?)?;
    let request = ServerLaunchRequest::new("server".into(), vec!["--stdio".into()], root)?;
    assert_eq!(request.program(), "server");
    let _default = TransportFactory::process(request.clone());
    let cleanup = Arc::new(AtomicBool::new(false));
    let launcher = Launcher(Arc::clone(&cleanup));
    let (input, _) = duplex(64);
    let (output, _) = duplex(64);
    let (errors, _) = duplex(64);
    let launched = LaunchedServer::new(
        input,
        output,
        errors,
        Lifecycle(Arc::clone(&cleanup)),
    );
    let _injected = TransportFactory::process_with(request, launcher);
    drop(launched);
    assert!(cleanup.load(Ordering::Relaxed));
    Ok(())
}
