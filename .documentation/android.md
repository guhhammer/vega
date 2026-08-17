# Android

The Rust stack cross-compiles as-is — libp2p, vodozemac and redb are all pure
Rust, which is why `redb` was chosen over SQLite and why the Olm implementation
is vodozemac rather than a C library. What Android needs beyond that is a set of
platform capabilities Tauri does not expose, and each one is a plugin someone
has to write. `crates/vega-android` is that plugin: a foreground service, a
multicast lock, and the one prompt that decides whether background delivery is
bursty or prompt.

**It has never run on a phone.** It compiles for `aarch64-linux-android` and its
call sites are type-checked by `./make check`; nothing beyond that has been
established, and the download page and the release notes still say delivery
happens only while the app is open. [Verifying it](#verifying-it) is what has to
happen before either of them changes.

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

## How the plugin is put together

`crates/vega-android` is a Tauri plugin: Kotlin on the Android side, a Rust
binding, and a Gradle library project under `android/`.

It is a crate rather than an edit to `gen/android` because that directory is
generated and gitignored, and both `./make android` and `release.yml` run
`tauri android init` on every build — anything written there is lost on the next
run. A plugin's own manifest is merged in by Gradle, which is what makes the
permissions and the `<service>` entry survive a regeneration.

```
crates/vega-android/
  src/lib.rs                    start / stop / state / request_battery_exemption
  build.rs                      links the Gradle project in; declares no commands
  android/src/main/
    AndroidManifest.xml         permissions and the <service>, merged by Gradle
    java/.../BackgroundPlugin.kt  the @TauriPlugin surface
    java/.../NodeService.kt       the foreground service and the multicast lock
```

**Nothing here is reachable from JavaScript.** The app drives it from Rust
through `PluginHandle::run_mobile_plugin`, so the plugin declares no ACL
commands and `app/src-tauri/capabilities/default.json` still grants no plugin
surface at all. Off Android every function is a no-op returning "not running",
which is why the desktop `./make check` still type-checks all of its call sites.

The service holds no socket and moves no message. The node is already running in
that process; the service exists so the system stops killing it, and to hold the
multicast lock while Wi-Fi is up.

Related prior art, if this ever needs replacing:
[`tauri-plugin-background-service`](https://crates.io/crates/tauri-plugin-background-service)
(1.0.1, July 2026) wraps the same idea behind a general-purpose Rust trait.

**Foreground service.** Without one, Android kills the socket seconds after the
app leaves the screen and nothing arrives until it is reopened. The service is
`START_STICKY` with `stopWithTask="false"` — swiping the app out of recents is
not a request to stop receiving mail — and carries a persistent, silent
notification with a stop action, because that notification is the price Android
charges for staying alive and the user should be able to decline to pay it.

> **`specialUse`, not `dataSync`.** From Android 15, `dataSync` and
> `mediaProcessing` foreground services are capped at **six hours in any
> 24**, after which the system calls `Service.onTimeout()` and the service must
> stop itself; starting another one throws
> `ForegroundServiceStartNotAllowedException` until the user brings the app to
> the foreground. A messenger that goes deaf every afternoon is worse than one
> that is honest about being open-only. `specialUse` carries no timeout and no
> runtime prerequisites. Its `<property>` justification exists for Google Play
> review, not for the OS — and there is no store listing here to review it.

**Multicast lock.** mDNS receives nothing without `WifiManager.MulticastLock`
held, and holding it costs battery. It is tied to the Wi-Fi state through a
`ConnectivityManager.NetworkCallback` rather than to the service's lifetime:
there is no multicast peer to find on a cellular connection, so most of its cost
is avoidable for nothing given up.

**No wake lock, on purpose.** Doze ignores wake locks anyway, and outside Doze
an arriving packet wakes the CPU on its own — the Wi-Fi driver sees to that. A
permanently held partial wake lock would cost battery to buy nothing, so the
plugin does not ask for one.

**Doze and battery.** See above: bursty is the design point, not a bug to fix.
`request_battery_exemption` opens the system dialog; the app should offer it
once, where the answer is obviously the user's, and survive a no. The dialog is
asynchronous, so the state it returns is the state *before* the answer — read
`state()` again on the next resume.

## What still needs writing

**Keystore.** `app/src-tauri/src/keystore.rs` writes a 0600 file. On Android that
file sits in app-private storage, which is better than nothing but is not the
Android Keystore. The interface is two functions wide precisely so this can be
replaced without touching anything else.

**A run on real hardware.** Everything above is written and type-checks; none of
it has been on a phone. See [Verifying it](#verifying-it) for what that has to
show before the README, the release notes or the download page may say anything
has changed.

### One Tauri bug to plan around

Keeping the process alive with a foreground service walks straight into
[tauri-apps/tauri#15671](https://github.com/tauri-apps/tauri/issues/15671): swipe
the app out of recents while the service holds the process, reopen it, and the
activity comes up blank — the new activity gets a fresh id, Tauri finds no window
for it, and the resume event is dropped. Open as of Tauri 2.11.5, which is the
version pinned here.

`on_run_event` in `app/src-tauri/src/lib.rs` carries the workaround: it rebuilds
the window when a resume arrives with none, and it is also where
`ExitRequested` is answered with `prevent_exit()` — without which the process
dies with the last activity and the service has nothing left to hold up. Both
are Android-only; `prevent_exit` on the desktop would mean an app that cannot be
quit.

See also [#11609](https://github.com/tauri-apps/tauri/issues/11609), the leaked
`MainActivity` with a service running.

## Verifying it

None of this is real until a phone shows it. `./make android`, install, then:

```bash
# Is the service up, and with which type?
adb shell dumpsys activity services dev.guhhammer.vega | grep -i foreground

# Is the multicast lock held? (Wi-Fi only — expect nothing on cellular.)
adb shell dumpsys wifi | grep -i -A3 multicast

# Force Doze and watch what delivery does. Unplug first: charging exits Doze.
adb shell dumpsys deviceidle force-idle
adb shell dumpsys deviceidle step   # walk it through the states
adb shell dumpsys deviceidle unforce

# Battery exemption, as the dialog would set it.
adb shell dumpsys deviceidle whitelist +dev.guhhammer.vega
```

What has to hold before any user-facing text changes:

1. A message sent to a locked, screen-off phone on Wi-Fi arrives without the app
   being opened.
2. Swiping the app out of recents and reopening it shows the conversation, not a
   white screen (#15671, above).
3. The notification's stop action stops the service, and reopening the app
   starts it again.
4. It survives more than six hours — the failure `dataSync` would have caused.
   `adb shell am compat enable FGS_INTRODUCE_TIME_LIMITS dev.guhhammer.vega`
   forces the timeout behaviour on without waiting for the day to pass.
5. Battery drain over a night is something you would accept on your own phone.

## Permissions

All of them live in
`crates/vega-android/android/src/main/AndroidManifest.xml`, which Gradle merges
into the app's:

| Permission | For |
| --- | --- |
| `INTERNET`, `ACCESS_NETWORK_STATE` | the socket, and watching the Wi-Fi state |
| `CHANGE_WIFI_MULTICAST_STATE` | the multicast lock, without which mDNS is deaf |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_SPECIAL_USE` | the service and its type |
| `POST_NOTIFICATIONS` | the runtime prompt from API 33 |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | opening the Doze exemption dialog — holding it grants nothing, the user's answer does |

There is deliberately no `WAKE_LOCK`; see above for why one would cost battery
and buy nothing.

A `foregroundServiceType` and its matching permission are required from API 34
onward; a service without a declared type will not start. `POST_NOTIFICATIONS`
is a runtime prompt from API 33, and refusing it does not stop the service —
Android shows the notification anyway for a foreground service, which is why the
plugin asks for it and then carries on regardless of the answer.

## iOS, for the record

Not on the roadmap, and the reason is structural rather than effort: iOS does not
let a background process hold a listening socket. The options are a push relay —
a server that learns "something arrived for tag X", which reintroduces exactly
what this project exists to avoid — or accepting that messages arrive only while
the app is open. Separately, mDNS on iOS needs a multicast entitlement granted by
Apple on application. Decide which compromise is acceptable before writing iOS
code, not after.
