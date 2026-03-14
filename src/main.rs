fn main() -> pylon::error::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = pylon::config::CockpitConfig::default();

    // Create tokio runtime for async polling (usage API + session discovery).
    // Multi-thread with 2 workers: one for HTTP/usage, one for session polling.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| pylon::error::CockpitError::Render(format!("tokio runtime: {e}")))?;

    let rt_handle = rt.handle().clone();

    let event_loop = winit::event_loop::EventLoop::new()
        .map_err(|e| pylon::error::CockpitError::Render(format!("Event loop: {e}")))?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

    let mut app = pylon::app::App::new(config, rt_handle);
    event_loop
        .run_app(&mut app)
        .map_err(|e| pylon::error::CockpitError::Render(format!("Event loop: {e}")))?;

    drop(rt);

    Ok(())
}
