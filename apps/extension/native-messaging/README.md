# Native messaging registration

The extension reaches `vortexd` through `vortex-host`, and a browser will only start
`vortex-host` if it finds a manifest describing it. These two files are that manifest, in
the two shapes the platforms use.

They live beside the extension rather than with the installer because the thing that must
not drift is the pair: **the id in the manifest and the id of the build**. The platform
forbids wildcards in `allowed_origins` and `allowed_extensions` (01 §Security boundaries),
so a rebuilt extension with a new id is an extension that silently cannot connect, and the
symptom — capture goes passive and stays passive — looks exactly like a crashed daemon.

| | Chrome, Edge, Brave, Vivaldi, Opera | Firefox |
|---|---|---|
| Manifest | `chrome.json` | `firefox.json` |
| Key | `allowed_origins` | `allowed_extensions` |
| Identity | `chrome-extension://<32-char id>/` | `vortex@vortex.download` |

`path` must be absolute in both. `%INSTALL_DIR%` and `%EXTENSION_ID%` are the two things
that vary per install — and neither is substituted at install time by a script. These files
are **documentation of a shape**, not templates that ship.

## What actually writes them

`crates/vortex-setup`. The manifests are generated in Rust from one table of browsers, and
a test in that crate renders them with the placeholders above and compares the result to
these two files — so a change to what the installer writes fails the build until these are
updated to match.

It runs from two places, and they are the same code:

| | |
|---|---|
| `vortexd --register` | The installer's post-install hook, and the thing to run by hand when capture has gone quiet. `--unregister` is the uninstaller's. |
| Every `vortexd` start | A reconciliation, not an install step. Someone who installs Brave a month after Vortex has a browser with no manifest, and the symptom is silence. |

`vortex-app` does not register. It owns no state, and this is state.

Registration is per-browser: one browser with a locked registry key does not cost the other
five, and every outcome is reported rather than folded into a success.

## Where they go

**Windows** — a per-user registry value whose default data is the path of a manifest under
`%LOCALAPPDATA%\Vortex\native-messaging\`. Deliberately not under the install directory: a
per-machine MSI would put that in `Program Files`, which a user-session daemon cannot write
to, and reconciling on every start is the point.

```
HKCU\Software\Google\Chrome\NativeMessagingHosts\io.vortex.host
HKCU\Software\Chromium\NativeMessagingHosts\io.vortex.host
HKCU\Software\Microsoft\Edge\NativeMessagingHosts\io.vortex.host
HKCU\Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\io.vortex.host
HKCU\Software\Vivaldi\NativeMessagingHosts\io.vortex.host
HKCU\Software\Mozilla\NativeMessagingHosts\io.vortex.host
```

Opera has no row of its own: on Windows it reads Chrome's key, so the Chrome registration
covers it, and writing a key Opera does not read would look like support and provide none.

There is no "is this browser installed" check on Windows, deliberately. A value under
`HKCU\Software\Vivaldi` when Vivaldi is not installed costs nothing, is invisible, and
means that installing Vivaldi tomorrow works immediately.

**macOS / Linux** — the manifest *is* the registration, in the browser's own
`NativeMessagingHosts` directory under `~/Library/Application Support/…` or `~/.config/…`
(and `~/.mozilla/native-messaging-hosts`, which is neither). Here the browser's own
configuration directory has to already exist, because creating `~/.config/vivaldi` for
software the user does not have is rude — and the per-start reconciliation means installing
it later still works, one restart on.

## Extension ids

An extension's Chromium id is the first sixteen bytes of the SHA-256 of its public key,
mapped onto `a`–`p`. Without a key, Chrome hashes the *folder* an unpacked build was loaded
from instead — a different id on every machine, and therefore an id nobody can pin.

So the build declares one. `apps/extension/identity.ts` holds the public key,
`wxt.config.ts` puts it in the manifest, and the id that follows from it is compiled into
`vortex_setup::CHROMIUM_IDS`:

```
lbapnpokpngacdfbhhmgjckoilhfkjia
```

Which means **nothing has to be typed**. Load the unpacked build, run `vortexd --register`,
and the id in the manifest is the id the browser will use.
`apps/extension/tests/identity.test.ts` fails if the key, the derived id and the Rust
constant ever stop agreeing.

The store build carries no key: that id belongs to the listing and is assigned when the item
is created. It becomes a *second* entry in `CHROMIUM_IDS` — a developer's build and a user's
install both have to reach the same daemon.

`VORTEX_EXTENSION_ID` (comma-separated) still adds ids at run time, for a build made
somewhere else or a store id being tried before it is committed:

```sh
VORTEX_EXTENSION_ID=abcdefghijklmnopabcdefghijklmnop vortexd --register
```

Anything that is not 32 characters from `a`–`p` is rejected with a warning, because a
malformed id is worse than a missing one — the browser accepts the manifest, matches
nothing, and starts no host.

Firefox never needed any of this. Its identity is `browser_specific_settings.gecko.id`, and
Gecko has no `key` field.

## Checking it worked

The extension goes passive when it cannot reach the daemon and says nothing about it, by
design — so a broken registration is quiet. `vortex-host` itself is not: run it directly
and it prints one line to stderr and exits if the daemon is unreachable, which separates
"the manifest is wrong" from "the daemon is not running".

`cargo run -p vortexd` on a machine that also has Vortex installed will point every browser
at `target/…/vortex-host.exe`, because that is where the daemon it just started lives. That
is usually what a developer wants and always what a developer forgets. `vortexd
--no-integrate` opts out, and re-running the installer puts it back.
