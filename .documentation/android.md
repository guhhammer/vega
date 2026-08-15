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

## What still needs writing

These are plugins, not tweaks. Each is Kotlin on the Android side with a Rust
binding.

**Foreground service.** Without one, Android kills the socket seconds after the
app leaves the screen and nothing arrives until it is reopened. The service needs
a persistent notification — that is the price Android charges for staying alive —
and should be started when the node comes up and stopped when it goes down.

**Multicast lock.** mDNS receives nothing on Android without
`WifiManager.MulticastLock` held. Acquire it while the node is running and
release it on pause; holding it costs battery.

**Doze and battery.** Doze mode throttles wakeups aggressively when the screen is
off. Expect delivery to become bursty rather than immediate. Requesting a battery
optimisation exemption is possible but users reasonably distrust it, so treat
bursty delivery as the normal case and make the UI honest about it rather than
pretending messages are instant.

**Keystore.** `app/src-tauri/src/keystore.rs` writes a 0600 file. On Android that
file sits in app-private storage, which is better than nothing but is not the
Android Keystore. The interface is two functions wide precisely so this can be
replaced without touching anything else.

## Permissions

`AndroidManifest.xml` will need at least:

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_DATA_SYNC" />
<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />
```

`FOREGROUND_SERVICE_DATA_SYNC` and its `foregroundServiceType` are required from
API 34 onward; a service without a declared type will not start.

## iOS, for the record

Not on the roadmap, and the reason is structural rather than effort: iOS does not
let a background process hold a listening socket. The options are a push relay —
a server that learns "something arrived for tag X", which reintroduces exactly
what this project exists to avoid — or accepting that messages arrive only while
the app is open. Separately, mDNS on iOS needs a multicast entitlement granted by
Apple on application. Decide which compromise is acceptable before writing iOS
code, not after.
