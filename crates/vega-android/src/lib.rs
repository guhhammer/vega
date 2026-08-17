//! Background delivery on Android.
//!
//! Android kills the socket seconds after the app leaves the screen unless a
//! foreground service is holding the process up, and mDNS receives nothing
//! without a multicast lock. This crate is the plugin that does both, plus the
//! one prompt that decides whether delivery in the background is bursty or
//! immediate.
//!
//! ## What it can and cannot buy
//!
//! The service keeps the process alive. It does **not** exempt the app from
//! Doze, which suspends network access for anything not on the exemption list —
//! a foreground service only keeps an app out of App Standby. So with the
//! service running and nothing else, a phone that is unplugged, still and dark
//! delivers in Doze's maintenance windows, which the system schedules more and
//! more rarely the longer it stays untouched.
//!
//! The way out that every other messenger takes is a push service, which is a
//! server learning *something arrived for this device* — the exact thing this
//! project exists without. The way out that stays serverless is the battery
//! optimisation exemption: [`request_battery_exemption`] opens the system
//! prompt, and an exempted app keeps network access through Doze. It is the
//! user's switch, asked once, and everything here works without it.
//!
//! ## Where the work happens
//!
//! The service runs in the app's own process and holds no sockets of its own.
//! The node is already running there; the service exists to raise that
//! process's priority so the system stops killing it, and to hold the multicast
//! lock while Wi-Fi is up. Nothing about delivery moves into Kotlin.
//!
//! Off Android every function here is a no-op that reports "not running", so
//! the app can call them unconditionally.

use serde::{Deserialize, Serialize};
use tauri::{
    plugin::{Builder, TauriPlugin},
    AppHandle, Runtime,
};

/// What went wrong talking to the Android side.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The plugin call itself failed — the Kotlin side threw, or the plugin was
    /// never registered because this is not Android.
    #[error("android plugin call failed: {0}")]
    Plugin(String),
}

/// The result of asking the Android side to do something.
pub type Result<T> = std::result::Result<T, Error>;

/// How the persistent notification should read while the node is up.
///
/// Android charges a visible, undismissable notification for the privilege of
/// staying alive. Since the user cannot get rid of it, it may as well say
/// something true: that the app is listening, and that a sleeping phone is slow
/// rather than broken.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    /// First line. Short — Android truncates hard.
    pub title: String,
    /// Second line.
    pub body: String,
    /// Label on the action that stops the service.
    pub stop_label: String,
}

impl Default for Notice {
    fn default() -> Self {
        Self {
            title: "Vega is listening".into(),
            body: "Messages arrive slowly while the phone sleeps.".into(),
            stop_label: "Stop".into(),
        }
    }
}

/// Whether the service is up, as reported by the Android side.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// The foreground service is running.
    pub running: bool,
    /// The multicast lock is held, which needs the service *and* a Wi-Fi
    /// connection. False on cellular is normal: there is no multicast peer to
    /// find there anyway.
    pub multicast_held: bool,
    /// The user has taken this app out of battery optimisation, so Doze will
    /// not suspend its network access. Delivery is bursty when this is false.
    pub battery_exempt: bool,
}

#[cfg(target_os = "android")]
mod imp {
    use super::{Notice, Result, State};
    use tauri::{
        plugin::{PluginApi, PluginHandle},
        AppHandle, Manager, Runtime,
    };

    /// The identifier the Kotlin side is registered under. It has to match the
    /// package in `android/src/main/java/...` exactly.
    const PLUGIN_PACKAGE: &str = "dev.guhhammer.vega.background";
    /// The `@TauriPlugin` class inside that package.
    const PLUGIN_CLASS: &str = "BackgroundPlugin";

    /// Handle to the registered Kotlin plugin, kept in Tauri's state.
    pub struct Android<R: Runtime>(PluginHandle<R>);

    impl<R: Runtime> std::fmt::Debug for Android<R> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Android").finish_non_exhaustive()
        }
    }

    /// Register the Kotlin plugin and stash its handle.
    pub fn setup<R: Runtime>(app: &AppHandle<R>, api: PluginApi<R, ()>) -> Result<()> {
        let handle = api
            .register_android_plugin(PLUGIN_PACKAGE, PLUGIN_CLASS)
            .map_err(|e| super::Error::Plugin(e.to_string()))?;
        app.manage(Android(handle));
        Ok(())
    }

    /// Call a command on the Kotlin side.
    fn call<R: Runtime, A: serde::Serialize, T: serde::de::DeserializeOwned>(
        app: &AppHandle<R>,
        command: &str,
        payload: A,
    ) -> Result<T> {
        app.try_state::<Android<R>>()
            .ok_or_else(|| super::Error::Plugin("plugin was never registered".into()))?
            .0
            .run_mobile_plugin(command, payload)
            .map_err(|e| super::Error::Plugin(e.to_string()))
    }

    /// The payload for a command that takes none.
    ///
    /// `()` would serialise to `null`, and the Kotlin side is handed a JSON
    /// document rather than an optional. An empty object costs two bytes and
    /// cannot be the thing that breaks.
    #[derive(serde::Serialize)]
    struct NoArgs {}

    pub fn start<R: Runtime>(app: &AppHandle<R>, notice: Notice) -> Result<State> {
        call(app, "start", notice)
    }

    pub fn stop<R: Runtime>(app: &AppHandle<R>) -> Result<State> {
        call(app, "stop", NoArgs {})
    }

    pub fn state<R: Runtime>(app: &AppHandle<R>) -> Result<State> {
        call(app, "state", NoArgs {})
    }

    pub fn request_battery_exemption<R: Runtime>(app: &AppHandle<R>) -> Result<State> {
        call(app, "requestBatteryExemption", NoArgs {})
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    //! Every desktop build gets these. They are not errors: a desktop process
    //! is not killed for being in the background, so "did not need a foreground
    //! service" is the honest answer rather than "unsupported".

    use super::{Notice, Result, State};
    use tauri::{plugin::PluginApi, AppHandle, Runtime};

    const IDLE: State = State {
        running: false,
        multicast_held: false,
        battery_exempt: true,
    };

    pub fn setup<R: Runtime>(_app: &AppHandle<R>, _api: PluginApi<R, ()>) -> Result<()> {
        Ok(())
    }

    pub fn start<R: Runtime>(_app: &AppHandle<R>, _notice: Notice) -> Result<State> {
        Ok(IDLE)
    }

    pub fn stop<R: Runtime>(_app: &AppHandle<R>) -> Result<State> {
        Ok(IDLE)
    }

    pub fn state<R: Runtime>(_app: &AppHandle<R>) -> Result<State> {
        Ok(IDLE)
    }

    pub fn request_battery_exemption<R: Runtime>(_app: &AppHandle<R>) -> Result<State> {
        Ok(IDLE)
    }
}

/// Register the plugin. Add it to the builder on every platform; off Android it
/// does nothing at all.
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("vega-android")
        .setup(|app, api| {
            imp::setup(app, api)?;
            Ok(())
        })
        .build()
}

/// Start the foreground service, and with it the multicast lock.
///
/// Idempotent: starting a service that is already running re-delivers the
/// notification and changes nothing else, which is what makes it safe to call
/// on every resume.
pub fn start<R: Runtime>(app: &AppHandle<R>, notice: Notice) -> Result<State> {
    imp::start(app, notice)
}

/// Stop the service and drop the notification.
///
/// Delivery falls back to "only while the app is open" until something calls
/// [`start`] again. This is what the notification's own stop action does, and
/// the app is expected to survive it rather than treat it as a fault.
pub fn stop<R: Runtime>(app: &AppHandle<R>) -> Result<State> {
    imp::stop(app)
}

/// Ask the Android side what is actually running.
pub fn state<R: Runtime>(app: &AppHandle<R>) -> Result<State> {
    imp::state(app)
}

/// Open the system prompt that takes this app out of battery optimisation.
///
/// This is the only thing that makes background delivery prompt rather than
/// bursty without a push server. Ask once, in a place where the answer is
/// obviously the user's, and take no for an answer.
///
/// The dialog is asynchronous: this returns as soon as it is on screen, so the
/// [`State`] it reports is the state *before* the answer. Call [`state`] again
/// on the next resume to find out what the user said.
pub fn request_battery_exemption<R: Runtime>(app: &AppHandle<R>) -> Result<State> {
    imp::request_battery_exemption(app)
}
