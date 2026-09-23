//! The application: one GPUI window over the engine — panes, tabs, sessions and the palette.
#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod clipboard;

pub mod actions;
pub mod bars;
pub mod bridge;
pub mod bundle;
pub mod chrome;
pub mod configuration_file;
pub mod follow;
pub mod grid;
pub mod host_ui;
pub mod input;
pub mod layout;
pub mod lifecycle;
pub mod menu;
pub mod navigation;
pub mod palette;
pub mod prompt;
mod pump;
pub mod session_tabs;
pub mod settings;
pub mod settings_window;
pub mod splits;
pub mod ssh_config;
pub mod stage;
pub mod status;
pub mod subscription;
pub mod surface;
pub mod tab_actions;
pub mod tab_label;
pub mod theme;
pub mod vt;
pub mod wake;
pub mod window;
