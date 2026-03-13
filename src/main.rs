fn main() -> claude_cockpit::error::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = claude_cockpit::config::CockpitConfig::default();

    let event_loop = winit::event_loop::EventLoop::new()
        .map_err(|e| claude_cockpit::error::CockpitError::Render(format!("Event loop: {e}")))?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);

    let mut app = claude_cockpit::app::App::new(config);
    event_loop
        .run_app(&mut app)
        .map_err(|e| claude_cockpit::error::CockpitError::Render(format!("Event loop: {e}")))?;

    Ok(())
}
