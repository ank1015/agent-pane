# Optional frontend verification

Install this directory's npm dependencies and Chromium (`npm install`,
`npm run install-browser`) on the worker host. Configure absolute
`SITES_BROWSER_NODE` and `SITES_BROWSER_SCRIPT` paths; the script is `verify.mjs`
from this directory. Browser binaries must be installed for the worker OS user.
Without these settings `browser.verify` is absent; the prompt says so.

The trusted helper launches a fresh Chromium context with its native sandbox,
no worker environment credentials, no persistent profile, no arbitrary target
URL or model JavaScript. It obtains the live site's signed content URL from the
leased Platform adapter. Navigation is limited to GET requests in that release's
content directory. External assets, WebSockets and backend requests are blocked.
Reports include load status, bounded DOM text, page errors and selector checks.
This checks frontend rendering only, not layout screenshots or authenticated
end-to-end interactions. `sites.invoke` verifies the real backend separately.
The result lists blocked resources so it cannot be mistaken for a full browser
integration test. A watchdog closes the browser on timeout.
