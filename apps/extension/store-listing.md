# Chrome Web Store listing copy

Everything the Developer Dashboard asks for in prose, kept in the repository so it
can be reviewed alongside the code it describes and reused verbatim on the next
submission.

**These paragraphs describe the store build and only the store build.** Upload the
package produced by `npm run zip:store`, never the one from `npm run zip` — the
default build contains the media pipeline, and every justification below would then
be an understatement of what the reviewer is holding. `tests/store-build.test.ts`
is what keeps the two apart.

Privacy policy URL: `https://github.com/NooberCong/vortex/blob/main/PRIVACY.md`

---

## Product details

**Category:** Workflow & Planning. Not Entertainment — the package contains a
download manager and nothing else, and filing it beside media extensions invites
the review it was built to avoid.

**Language:** English (United States).

**Homepage URL:** `https://github.com/NooberCong/vortex`
**Support URL:** `https://github.com/NooberCong/vortex/issues`
**Official URL:** leave as None. It requires Search Console ownership of the
domain, and a repository path cannot be verified.

**Mature content:** No.

### Description

```
Vortex is a download manager for people whose downloads are slow, or break.

This extension is the browser half. It notices a download starting, hands it to
the Vortex app running on your computer, and gets out of the way. The app does
the fetching.

REQUIRES THE VORTEX DESKTOP APP — free, open source, Windows, macOS and Linux.
This extension does nothing on its own.
Get it: https://github.com/NooberCong/vortex/releases/latest

WHAT IT DOES
- Parallel transfers: one file, many connections at once
- Real resume: an interrupted download continues where it stopped, including
  after a reboot or a crash
- A queue that survives a restart
- Files behind a login still download. The request is handed over with the
  headers and cookies your browser would have sent, so a transfer that needs
  your session still works
- An expired link is renewed from the page it came from instead of failing

HOW IT WORKS
When your browser starts a download, the extension checks that the Vortex app
is actually answering, passes the request to it, and cancels the browser's own
copy so the file is not fetched twice. If the app is not running, nothing is
taken over and your browser downloads exactly as it always did. That rule is
deliberate: a download cancelled for an app that never picks it up is a file
you have simply lost.

PRIVACY
There is no Vortex server and no account, because there is nowhere to send
anything. Nothing leaves your machine except to the app you installed, over a
local channel. No analytics, no tracking, no ads.
Policy: https://github.com/NooberCong/vortex/blob/main/PRIVACY.md

OPEN SOURCE
Every line, including this extension: https://github.com/NooberCong/vortex

Subscription services that use DRM are excluded in the extension's manifest.
On those sites it is never loaded and takes nothing over.
```

### Graphic assets

- **Store icon:** `apps/extension/public/icon/128.png` — 128x128 PNG, already
  the right size and shape.
- **Screenshots:** at least one, 1280x800 or 640x400, JPEG or 24-bit PNG **with
  no alpha channel**. The popup is 380 px wide, so it has to be composed onto
  the canvas rather than cropped to it.
- **Promo tiles and video:** skip. They matter for featuring, not for approval.

Nothing in a screenshot may show a capability this package does not have.
A screenshot taken on a streaming site, or showing the overlay from the full
build, shows a reviewer functionality that is not in the uploaded zip — which
is a rejection on accuracy grounds before it is anything else.

## Single purpose

Vortex hands downloads from the browser to the Vortex download manager, an
application the user installs separately on the same computer. The extension
recognises a download the browser is about to start, sends that request's details
to the local application over native messaging, and cancels the browser's own
transfer so the application can fetch the file instead — in parallel, resumably,
and into a queue that survives a restart. Every feature in the package exists to
serve that one flow.

## Permission justifications

### `nativeMessaging`

This is the entire point of the extension. Downloads are handed to `vortexd`, an
application on the same machine, through the native messaging host `io.vortex.host`.
No other channel between a browser extension and a local application exists.
Without this permission the extension has nowhere to send anything and does
nothing at all.

### `webRequest`

A download has to be recognised *before* the browser commits to saving it, and the
evidence for that lives in request and response headers: `Content-Disposition`,
`Content-Type`, and `Content-Length`. The extension observes `onBeforeRequest`,
`onSendHeaders`, `onHeadersReceived`, `onCompleted` and `onErrorOccurred` to build
a short-lived record of requests, so that when a download begins it can be replayed
by the application with the same headers the browser used — a transfer replayed as
a bare GET is refused by most content delivery networks.

Observation only. The extension does not request `webRequestBlocking`, holds up no
request, and modifies no request or response.

### `downloads`

To take a download over, the extension has to see it start (`onCreated`,
`onDeterminingFilename`), confirm the application is actually reachable, and only
then cancel the browser's copy (`cancel`, `erase`) so the file is not fetched
twice. `search` is used to read back the browser's own record of a download it is
about to hand over. Nothing is cancelled unless the application has answered a
liveness check first — a cancelled download that Vortex then fails to fetch would
be a file the user simply loses.

### `cookies`

Many downloads are only served to a signed-in session, and a transfer replayed
without the browser's credentials returns 403. Cookies for the download's own URL
are forwarded to the local application so it can complete the request the user
already authorised.

Used narrowly: cookies are read for one specific URL, and only when the download
the browser started has no observed request to copy the `Cookie` header from. The
cookie store is never enumerated. Credentials are held in memory by the application
for the life of the job and are stripped before any job is written to its local
database or to a log.

### `storage`

Session storage holds the short-lived record of observed requests described under
`webRequest`, plus a cached copy of the application's settings, so that the
decision to take a download over does not have to wait on a round trip while the
browser is already fetching. Cleared when the browser closes. Synced storage is
not used, so nothing is copied to the user's browser account or to other devices.

### `alarms`

One alarm, once a minute. The extension's connection to the local application can
die while the service worker is idle, and a dead connection is indistinguishable
from a working one until something is asked of it. The alarm re-checks liveness so
that a stale connection is discovered on a timer rather than in front of the user's
next download. It is not used to keep the service worker alive.

### `tabs`

Three things, all of them about the download in front of the user. The tab a
download came from supplies the page title and address, which is how a file gets a
meaningful name and how an expired link is re-acquired from the page that issued
it. The toolbar popup needs to know which tab it was opened over in order to report
on it. And opening the application's download page in a new tab is how the popup
answers a user whose machine does not have the application installed.

### Host permissions (`<all_urls>`)

A download can begin on any site, and the extension cannot know in advance which
one. The host permission is what allows request metadata to be observed wherever a
download starts, and what allows the content script to re-request an expired link
from the page that issued it. It is not used to read or alter page content.

A list of DRM-protected subscription services is excluded in the manifest
(`exclude_matches`) and again in the extension's own checks. On those origins no
content script is injected and no download is taken over. The exclusion is visible
in the package rather than asserted in this box, which is the point of doing it
that way.

### Remote code

None. The extension executes only the JavaScript in the uploaded package. It loads
no scripts from any server, evaluates no strings as code, and fetches no code at
runtime.

## Data usage disclosures

- **Does not** collect personally identifiable information
- **Does not** collect health, financial, authentication, personal communications,
  or location information
- **Does not** collect user activity or website content for any purpose of its own
- Web history and website credentials are transmitted **only to the user's own
  computer**, to the Vortex application they installed, for the sole purpose of
  completing the download they asked for
- Data is not sold or transferred to third parties; there is no server and no third
  party
- Data is not used or transferred for purposes unrelated to the single purpose above
- Data is not used or transferred to determine creditworthiness or for lending

## Reviewer notes

Two things in the package will look larger than they are, and both are deliberate.

**A content script on all sites.** In this build its only function is to re-request
an expired download URL from the page that issued it — one `fetch` for the first
byte, with the response body discarded — because a link signed for the browser's
session cannot be renewed from anywhere else. It draws no interface and reads no
page content.

**A list of streaming service domains in the manifest.** These are exclusions.
Vortex is a general-purpose download manager, and the list is where it declines to
operate: no content script is injected on those origins and no download is taken
over there. It is in the manifest so the refusal is a property of the package a
reviewer can read, rather than a claim in a text box.
