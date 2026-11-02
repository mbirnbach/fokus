# Updates

Fokus looks for a newer release on GitHub a few seconds after it starts and then every six hours
(Settings → Updates turns this off). When it finds one, it downloads the file for this
computer in the background, checks its signature and puts it in place of the installed app.
The running app and a job on the laser carry on; *Restart to update* in the top bar (or simply
the next start) brings up the new version. The code is in [`fokus/src/update.rs`](../fokus/src/update.rs).

| Installed from | The update |
|---|---|
| macOS DMG | replaces `Fokus.app` (it must also carry our Developer ID signature) |
| Windows installer | runs the new installer silently when Fokus closes |
| Windows zip | replaces `fokus.exe` |
| Linux AppImage | replaces the AppImage file |
| Linux tar.gz | replaces the `fokus` binary |

Development builds (`cargo run`) never update themselves.

## The signing key

The release builds sign every download with a private key (`<file>.sig`, Ed25519). The app
carries the public half, [`fokus/update-key.pub`](../fokus/update-key.pub), and installs only
files with a valid signature, so nobody but our release builds can push an update to it. A
release published without the key fails to build.

Making it, once:

```bash
cargo run -p fokus-sign -- keygen ~/fokus-update-key.txt
gh secret set UPDATE_SIGNING_KEY -R mbirnbach/fokus < ~/fokus-update-key.txt
```

`keygen` writes the private key to the file you name (and nowhere else) and the public key to
`fokus/update-key.pub`, which is committed. Keep a backup of the private key somewhere safe,
like a password manager. Installed apps accept only updates signed with this key; with a new
key, everyone would have to download the next version by hand once.

Signing locally: `UPDATE_SIGNING_KEY=$(cat ~/fokus-update-key.txt) scripts/package.sh <target>`.

## Trying it

`FOKUS_UPDATE_PRETEND_VERSION=0.0.1` makes an installed Fokus act as if it were that version, so
it updates to the latest release. `FOKUS_UPDATE_URL` points it at another release description (the
JSON of GitHub's *latest release* API) for a local test. `FOKUS_UPDATE_DEV=1` lets a debug build
use the updater.
