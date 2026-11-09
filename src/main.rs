use rafdev_playlink::app;
use rafdev_playlink::core::{PlayLinkError, Settings};
use rafdev_playlink::diagnostics;

fn main() -> Result<(), PlayLinkError> {
    let settings = Settings::load().unwrap_or_default();
    diagnostics::init_logging(settings.debug_logs);
    tracing::info!("RafDev PlayLink v{} starting", env!("CARGO_PKG_VERSION"));
    app::run(settings)
}
