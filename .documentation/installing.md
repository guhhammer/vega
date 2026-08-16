# Installing

Vega is a desktop application. There is nothing to sign up for and no server to
point it at — the first launch generates an account on the device and starts
looking for peers.

**Nothing here has been audited.** Read [`../SECURITY.md`](../SECURITY.md) and
the [threat model](threat-model.md) before trusting it with anything that
matters.

## Download

Every release attaches these four files, plus `SHA256SUMS.txt`:

| Platform | File |
|---|---|
| Linux (any distribution) | `Vega-linux-x86_64.AppImage` |
| Debian, Ubuntu | `Vega-linux-amd64.deb` |
| macOS (Intel and Apple silicon) | `Vega-macos-universal.dmg` |
| Windows | `Vega-windows-x86_64-setup.exe` |

Grab them from the [latest release][latest]. Those names never change between
versions, so a link written once keeps working:

```
https://github.com/guhhammer/vega/releases/latest/download/Vega-linux-x86_64.AppImage
```

The same page also carries Tauri's version-stamped names (`Vega_0.1.0_amd64.deb`
and so on). They are the same bytes; the stable names exist so links do not rot.

[latest]: https://github.com/guhhammer/vega/releases/latest

### Neither the macOS nor the Windows build is signed

Signing certificates cost money per year and prove only that someone paid for
one. This project has neither, so both operating systems will warn on first
open. What that means in practice is under each platform below.

That warning is the operating system saying "I cannot verify who built this",
which is exactly true. The checksums below are what you can verify instead, and
they are worth more than a certificate if you check them against a source other
than the page you downloaded from.

## Verify the download

`SHA256SUMS.txt` on the release lists every file. Download it next to the
installer and check:

```bash
# Linux
sha256sum --check --ignore-missing SHA256SUMS.txt

# macOS
shasum -a 256 --check --ignore-missing SHA256SUMS.txt
```

```powershell
# Windows (PowerShell) — compare the printed hash against the line in the file
Get-FileHash .\Vega-windows-x86_64-setup.exe -Algorithm SHA256
```

`--ignore-missing` is what lets you check one file against a list covering all
four. A line reading `OK` is the whole result; anything else means the file is
not what CI built, and you should not run it.

## Linux

### AppImage — any distribution

One file, no installation, no root:

```bash
chmod +x Vega-linux-x86_64.AppImage
./Vega-linux-x86_64.AppImage
```

If it exits complaining about FUSE, your distribution ships only FUSE 3 and the
AppImage runtime wants FUSE 2. Either install the compatibility package
(`libfuse2` on Debian and Ubuntu) or skip the mount entirely:

```bash
./Vega-linux-x86_64.AppImage --appimage-extract-and-run
```

### .deb — Debian and Ubuntu

```bash
sudo apt install ./Vega-linux-amd64.deb
```

Use `apt`, not `dpkg -i`: the package depends on the system WebKit and GTK
libraries, and `apt` will pull them in where `dpkg` only complains about them.

The build runs on Ubuntu 22.04, so glibc 2.35 is the floor. Anything that old or
newer is fine; a distribution older than that needs the source build.

Uninstall with `sudo apt remove vega`.

## macOS

Open the `.dmg` and drag Vega to Applications. The first launch is refused,
because the app is unsigned and unnotarized:

> "Vega" cannot be opened because the developer cannot be verified.

Right-click the app in Applications and pick **Open**, then **Open** again in
the dialog. That is the documented way to run an unsigned app, and it is only
needed once — macOS remembers the decision.

On macOS 15 and later the right-click route may not appear. Then it is
**System Settings → Privacy & Security**, scroll to the bottom, and
**Open Anyway** next to the message about Vega.

If it was quarantined some other way and neither route works:

```bash
xattr -d com.apple.quarantine /Applications/Vega.app
```

Only run that on a file whose checksum you have already verified — it removes
the warning by discarding the very flag the warning is based on.

The `.dmg` is universal: one download carries both Intel and Apple silicon
builds, so there is no wrong choice.

## Windows

Run `Vega-windows-x86_64-setup.exe`. SmartScreen will interrupt with:

> Windows protected your PC

Click **More info**, then **Run anyway**. The installer is unsigned, which is
what SmartScreen is reporting.

It installs per-user, into `%LOCALAPPDATA%\Vega`, so there is no administrator
prompt and nothing is written outside your own profile. Uninstall from
**Settings → Apps → Installed apps**.

## Where Vega keeps its data

| Platform | Path |
|---|---|
| Linux | `~/.local/share/dev.guhhammer.vega/` |
| macOS | `~/Library/Application Support/dev.guhhammer.vega/` |
| Windows | `%APPDATA%\dev.guhhammer.vega\` |

Three things live there:

- `vega.redb` — messages, contacts, sessions. Encrypted; the key is held by the
  platform keystore (Secret Service, Keychain, Credential Manager) and falls
  back to a `0600` file where no keystore exists. The app logs which backing it
  chose at startup.
- `device.key` — this device's identity.
- `seeds.json` — optional, and you create it. A plain JSON array of multiaddrs
  naming bootstrap nodes; see [running-a-seed.md](running-a-seed.md).

**Deleting that directory destroys the account.** There is no server holding a
copy and no recovery: the account *is* the keypair in those files. Nobody can
reset it for you, which is the same property that means nobody can seize it.

An account is one device today — device linking is not implemented, so the
directory cannot be copied to a second machine and used from both.

## First run

Two machines on one network find each other over mDNS with no configuration.
Copy the invite from one (**My invite**), paste it into the other
(**Add contact**), and send a message.

Across the internet, one of the two needs a bootstrap address to start from —
run a seed and list it in `seeds.json`. That is
[running-a-seed.md](running-a-seed.md).

Verify a contact by comparing safety numbers over a channel that is not Vega. An
invite that reached you over a channel someone else controls could be theirs;
the safety number is what catches it.

## Building from source instead

Requires Rust (the version in [`../rust-toolchain.toml`](../rust-toolchain.toml)
is fetched automatically), Node 20 or newer, and the Tauri system dependencies
for your platform — see [tauri.app/start/prerequisites][prereq].

```bash
git clone https://github.com/guhhammer/vega
cd vega
./make dist
```

That leaves this machine's installers in `release/`, named exactly as the
downloads above, with a `SHA256SUMS.txt` beside them. Tauri links against the
system webview and cannot cross-compile, so a machine builds only its own
platform's bundles.

To run it without packaging, `./make dev`. Note that `cargo build` alone
produces a binary that proves the code compiles but is not an application — the
frontend is not embedded, so the window opens on a connection error. Use `dev`
or `dist`.

[prereq]: https://tauri.app/start/prerequisites/
