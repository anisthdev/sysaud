use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "sysaud")]
#[command(about = "Linux system event sound and notification daemon", long_about = None)]
pub struct Args {
    #[arg(short, long, default_value_t = false)]
    pub verbose: bool,
    #[arg(long)]
    pub no_sound: bool,
    #[arg(long)]
    pub no_notify: bool,
    #[arg(long)]
    pub json: bool,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Check,
    ListEvents,
}

impl Args {
    pub fn load_config(&self) -> anyhow::Result<crate::config::Config> {
        crate::config::load(self.config.clone())
    }
}

pub fn list_events() -> Vec<&'static str> {
    vec![
        "power.ac-plugged",
        "power.ac-unplugged",
        "power.battery-low",
        "power.battery-critical",
        "power.battery-action",
        "power.battery-level",
        "network.wifi-connected",
        "network.network-connected",
        "network.network-disconnected",
        "network.vpn-connected",
        "network.vpn-disconnected",
        "bluetooth.device-connected",
        "bluetooth.device-disconnected",
        "bluetooth.adapter-on",
        "bluetooth.adapter-off",
        "usb.device-plugged",
        "usb.device-unplugged",
        "session.login",
        "session.screen-locked",
        "session.screen-unlocked",
        "session.suspend",
        "session.resume",
        "display.brightness-changed",
        "disk.mounted",
        "disk.unmounted",
    ]
}
