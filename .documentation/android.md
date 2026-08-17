# Android

The Rust stack cross-compiles as-is — libp2p, vodozemac and redb are all pure
Rust, which is why `redb` was chosen over SQLite and why the Olm implementation
is vodozemac rather than a C library. What Android needs beyond that is a set of
platform capabilities Tauri does not expose, and each one is a plugin someone
has to write.

## One-time setup

```bash
# SDK + NDK, then:
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/<version>"

cd app
npm run tauri android init
```

The Rust targets are already installed on this machine:
`aarch64-linux-android`, `armv7-linux-androideabi`, `i686-linux-android`,
`x86_64-linux-android`.

Then `./make android`, or `cd app && npm run tauri android dev` for a device.

`./make android` runs `tauri android init` itself if `app/src-tauri/gen/android`
is missing — that directory is generated and gitignored, so it is a first-run
step here and an every-run step in CI.

## Signing

**Android will not install an unsigned package.** Not "will warn about": will
refuse. So an unsigned APK is a build intermediate rather than something to put
on a release page, and `./make android` says so rather than pretending
otherwise.

There is no store account involved, so the key does not have to be registered
with anybody — it only has to stay the same forever. Android identifies an
installed application by its signature, so a second key means a second
application: an upgrade over the top fails, and the only way through it is for
every user to uninstall and lose their identity, their contacts and their
history. **Back this file up, in more than one place.**

```bash
keytool -genkeypair -v \
  -keystore vega-release.jks \
  -alias vega \
  -keyalg RSA -keysize 4096 \
  -validity 10000 \
  -storetype pkcs12
```

Press enter at the key password prompt to reuse the store password, which is
what the workflow assumes unless told otherwise.

Locally, point `./make android` at it:

```bash
export VEGA_ANDROID_KEYSTORE="$HOME/keys/vega-release.jks"
export VEGA_ANDROID_KEYSTORE_PASS="…"
export VEGA_ANDROID_KEY_ALIAS=vega
./make android            # → release/Vega-android-universal.apk, signed
```

In CI, `release.yml` builds and signs the APK and attaches it to the draft
release. It needs three repository secrets, and skips the whole job with a
warning if the first is absent — a release without an Android key is still a
release:

| Secret | What |
| --- | --- |
| `ANDROID_KEYSTORE_BASE64` | `base64 -w0 vega-release.jks` |
| `ANDROID_KEYSTORE_PASSWORD` | The store password |
| `ANDROID_KEY_ALIAS` | `vega`, above |
| `ANDROID_KEY_PASSWORD` | Only if the key password differs from the store's |

The signature is verified with `apksigner verify` before the APK is uploaded,
because a signing command that exits zero and a package that installs are not
the same claim.

## No store listing

Deliberately. A Play Store entry means a review queue, a developer account tied
to a legal identity, and a company whose rules can change under a messenger
whose whole point is that no third party sits in the middle. The APK on the
release page is the same build, handed over directly, and
[vega-web](../web/index.html) is the page that points at it.

What that costs: no automatic updates, and a "this source is not trusted" prompt
on first install. What it buys: nobody between the build and the phone.

## Use the mobile node profile

A phone must not carry other people's traffic on a metered connection. In
`build_runtime`, pass `NodeConfig::mobile()` instead of `NodeConfig::default()`:

```rust
#[cfg(target_os = "android")]
let config = NodeConfig::mobile();
#[cfg(not(target_os = "android"))]
let config = NodeConfig::default();
```

That turns off UPnP (pointless behind carrier NAT), relay serving, and mailbox
serving, while keeping mDNS so the phone still finds peers on a home network.

## Background delivery: can it be done?

**The service, yes. "Immediate", no — and that half is not a plugin somebody
forgot to write.**

A foreground service keeps the process and the socket alive, and it is ordinary
Kotlin. What it does not do is exempt the app from **Doze**. When the phone is
unplugged, stationary and dark, the system *suspends network access* for
everything not on the exemption list — a foreground service only keeps the app
out of App Standby, it buys nothing here. Traffic resumes during maintenance
windows, which the system schedules less and less often the longer the device
stays untouched.

Every other messenger escapes that with a push service: FCM holds one socket for
the whole device and wakes the app. That is a server learning *something arrived
for this device*, which is the precise thing this project exists not to have.
The serverless way out is
`Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` — one prompt, after which
the app keeps network access and partial wake locks through Doze. Google Play
forbids that request for apps that could use FCM instead; Vega is a sideload, so
no policy applies. It is still the user's switch, not ours.

So the honest target is: immediate while the screen is on, the phone is charging,
or it is moving; **bursty in Doze**; near-immediate for someone who grants the
exemption. The UI should say that rather than implying a message is instant.

## What still needs writing

These are plugins, not tweaks. Each is Kotlin on the Android side with a Rust
binding.

They belong in a Tauri plugin crate carrying its own `android/` library project —
**not** in `gen/android`. That directory is generated and gitignored, and both
`./make android` and `release.yml` run `tauri android init` on every build, so
anything edited in it is lost on the next run. A plugin's own manifest is merged
in by Gradle, which is what makes the permissions and the `<service>` entry
survive a regeneration.

Prior art worth reading before writing any of it:
[`tauri-plugin-background-service`](https://crates.io/crates/tauri-plugin-background-service)
(1.0.1, July 2026) already wraps a `START_STICKY` foreground service with a
persistent notification behind a Rust trait, on top of the same plugin
machinery.

**Foreground service.** Without one, Android kills the socket seconds after the
app leaves the screen and nothing arrives until it is reopened. The service needs
a persistent notification — that is the price Android charges for staying alive —
and should be started when the node comes up and stopped when it goes down.

> **Use `specialUse`, not `dataSync`.** From Android 15, `dataSync` and
> `mediaProcessing` foreground services are capped at **six hours in any
> 24**, after which the system calls `Service.onTimeout()` and the service must
> stop itself; starting another one throws
> `ForegroundServiceStartNotAllowedException` until the user brings the app to
> the foreground. A messenger that goes deaf every afternoon is worse than one
> that is honest about being open-only. `specialUse` carries no timeout and no
> runtime prerequisites. Its `<property>` justification exists for Google Play
> review, not for the OS — and there is no store listing here to review it.

**Multicast lock.** mDNS receives nothing on Android without
`WifiManager.MulticastLock` held. Acquire it while the node is running and
release it on pause; holding it costs battery. It only buys anything on Wi-Fi —
there is no multicast peer to find on a cellular connection — so tying it to the
Wi-Fi state rather than to the node's lifetime is worth doing.

**Doze and battery.** See above: bursty is the design point, not a bug to fix.
The battery-optimisation prompt is worth offering once, in a place where the
answer is clearly the user's, and worth surviving a "no".

**Keystore.** `app/src-tauri/src/keystore.rs` writes a 0600 file. On Android that
file sits in app-private storage, which is better than nothing but is not the
Android Keystore. The interface is two functions wide precisely so this can be
replaced without touching anything else.

### One Tauri bug to plan around

Keeping the process alive with a foreground service walks straight into
[tauri-apps/tauri#15671](https://github.com/tauri-apps/tauri/issues/15671): swipe
the app out of recents while the service holds the process, reopen it, and the
activity comes up blank — the new activity gets a fresh id, Tauri finds no window
for it, and the resume event is dropped. Open as of Tauri 2.11.5. The workaround
is to rebuild the window when a resume arrives with none:

```rust
tauri::RunEvent::Resumed => {
    if app.webview_windows().is_empty() {
        tauri::WebviewWindowBuilder::new(app, "main", tauri::WebviewUrl::default())
            .build()
            .ok();
    }
}
```

See also [#11609](https://github.com/tauri-apps/tauri/issues/11609), the leaked
`MainActivity` with a service running.

## Permissions

The plugin's own `AndroidManifest.xml` will need at least:

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />
<uses-permission android:name="android.permission.WAKE_LOCK" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_SPECIAL_USE" />
<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />
<uses-permission android:name="android.permission.REQUEST_IGNORE_BATTERY_OPTIMIZATIONS" />

<application>
  <service
    android:name=".VegaNodeService"
    android:exported="false"
    android:foregroundServiceType="specialUse">
    <property
      android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE"
      android:value="Peer-to-peer message delivery over a socket this app owns.
                     There is no server to push from." />
  </service>
</application>
```

A `foregroundServiceType` and its matching permission are required from API 34
onward; a service without a declared type will not start. `POST_NOTIFICATIONS`
is a runtime prompt from API 33, and refusing it does not stop the service —
Android shows the notification anyway for a foreground service.

## iOS, for the record

Not on the roadmap, and the reason is structural rather than effort: iOS does not
let a background process hold a listening socket. The options are a push relay —
a server that learns "something arrived for tag X", which reintroduces exactly
what this project exists to avoid — or accepting that messages arrive only while
the app is open. Separately, mDNS on iOS needs a multicast entitlement granted by
Apple on application. Decide which compromise is acceptable before writing iOS
code, not after.
