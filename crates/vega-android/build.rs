//! Plugin build hook.
//!
//! The command list is empty on purpose. Nothing here is reachable from
//! JavaScript: the app drives the service from Rust through
//! `PluginHandle::run_mobile_plugin`, so there is no ACL surface to grant and
//! `app/src-tauri/capabilities/default.json` stays as it is — no plugin
//! permissions at all.

const COMMANDS: &[&str] = &[];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .android_path("android")
        .build();
}
