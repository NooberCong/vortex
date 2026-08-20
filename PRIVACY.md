# Privacy Policy

**Vortex browser extension**
Last updated: 20 August 2026

## The short version

Vortex has no server. There is no account, no telemetry, no analytics, and no
third party. Nothing the extension observes is sent anywhere except to the Vortex
app running on the same machine, over a channel only your own user account can
open.

If you uninstall the app, the extension has nowhere to send anything and goes
inert.

## What Vortex is

Vortex is two programs. The extension is the smaller half: it watches for
downloads and hands them to the app. The app (`vortexd`) is what actually fetches
files, and it runs locally on your computer.

They talk over the browser's native messaging channel — the extension writes to a
local helper program on standard input, and the helper reaches the app over a
named pipe (Windows) or a Unix socket. Both are local to your machine and
restricted to your user account. No network is involved in that hop.

## What the extension observes

To recognise a download before the browser starts one, the extension watches
request and response metadata for pages you visit:

- the URL, method, and request headers of requests the browser makes
- response headers: content type, content length, `Content-Disposition`
- which tab a request belongs to, and that tab's URL and title
- downloads the browser starts: URL, referrer, and size

This is metadata about requests. The extension does not read page content, does
not read the bodies of responses, and does not record what you type.

## What is sent to the Vortex app

When you take a download over — or when the extension hands one to Vortex — it
sends that download's request details: URL, method, headers, and the page URL and
title the download came from, so the file can be named and, if the link later
expires, re-acquired.

Nothing is sent for pages where no download is happening.

## Credentials

Some downloads only work if the request carries the same cookies or authorisation
your browser would have sent. Vortex forwards those for the specific download
being handed over, and handles them as credentials rather than as ordinary data:

- the `Cookie` header travels in a field of its own, so it cannot be confused with
  ordinary headers
- the app keeps credentials **in memory for the life of the job**. Before a job is
  written to Vortex's local database, `cookie`, `authorization` and
  `proxy-authorization` are stripped from it
- credentials are never written to the database, to a log file, or to the metadata
  file beside a partial download
- URLs written to logs have their query string removed, because signed links carry
  credentials in the query

The extension also has permission to read the browser's cookie store. It is used
in one situation only: when a download the browser started has no observed request
to copy a `Cookie` header from, the header is reconstructed for that one URL. It
is not read otherwise, and the cookie store is never enumerated.

## What the extension stores in your browser

- **Session storage**, which your browser clears when it closes: recently observed
  requests (so a download can be handed over with the headers the browser used),
  and a cached copy of your Vortex settings.
- **Your per-site preferences** — the sites you have switched Vortex off for — are
  stored by the app, not by the extension, so the same list is visible in the app's
  settings.

The extension does not use synced storage, so nothing is copied to your browser
account or to other devices.

## Network requests the extension makes

One, and only in one situation. When a download link has expired and Vortex needs a
fresh one, the extension re-requests **that same URL** from the page it came from,
asking for the first byte only and discarding the response body. It is the request
your browser would have made, made from the same page, to the site you were already
on.

The extension makes no other network requests. It contacts no Vortex-operated
server, because none exists. Clicking "Get Vortex" in the extension's popup opens
the project's GitHub releases page in a tab; nothing is sent with it.

## What Vortex does not do

- No analytics, telemetry, crash reporting, or usage statistics
- No advertising, and no ad or tracking networks
- Your data is not sold, rented, or transferred to anyone — there is nobody to
  transfer it to
- No remote code: the extension executes only the code in the reviewed package
- No use of your data for anything unrelated to handing a download to your own
  computer

## The Chrome Web Store build

The package published to the Chrome Web Store is a reduced build. It contains
generic HTTP download capture only — the streaming-media features described in the
project's documentation are not present in it, and neither is the code that
implements them.

## Changes

Material changes to this policy will be published in this file, and its history is
visible in the repository's commit log.

## Contact

Questions and concerns: open an issue at
<https://github.com/NooberCong/vortex/issues>.
