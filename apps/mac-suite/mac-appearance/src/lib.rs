// SPDX-License-Identifier: GPL-3.0-only

//! Automatic light/dark switching and dynamic wallpaper for COSMIC.
//!
//! The library holds everything the `mac-appearance` daemon and the settings
//! page in `mac-theme` share: the config files, the offline sun calculation,
//! the transition schedule and the writers for the COSMIC config.

pub mod apply;
pub mod config;
pub mod schedule;
pub mod solar;
