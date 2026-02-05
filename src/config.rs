use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "kmsvnc",
    about = "KMS-based VNC server with touch & keyboard input"
)]
pub struct Config {
    /// DRM device path (e.g. /dev/dri/card0). Auto-detects if not specified.
    #[arg(short, long)]
    pub device: Option<String>,

    /// VNC listen port
    #[arg(short, long, default_value_t = 5900)]
    pub port: u16,

    /// Maximum frames per second
    #[arg(short, long, default_value_t = 30)]
    pub fps: u32,

    /// VNC listen address
    #[arg(short, long, default_value = "0.0.0.0")]
    pub listen: String,

    /// VNC password for authentication (Type 2). No auth if omitted.
    #[arg(long)]
    pub password: Option<String>,
}
