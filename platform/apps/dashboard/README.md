# dashboard

The React and Vite client for Agent Pane. During local development, Vite
proxies `/api/*` requests to the platform server at `http://127.0.0.1:3100`.

```sh
pnpm install
pnpm dev
```

TanStack Query owns server state and caching. Zustand owns client-only UI state
under `src/stores/`; server responses should not be copied into Zustand.

The Machines page shows registered hosts and E2B sandbox accounts, not sandbox
hosts. Account creation uses a non-retrying mutation, updates the account cache,
then refetches it. Account queries also refresh on focus, reconnect, and every
15 seconds while visible. The API key stays in transient form/request state,
is masked by default, and is cleared after submission or closing the dialog.

The Add Sandbox dialog reuses the previous dashboard's control and dialog styles
(`src/styles/dialog.css`) and Hugeicons visibility icons. E2B is preselected;
the account-name and API-key steps use Back/Next/Finish, with keyboard focus
management and disabled navigation while creation is pending.

The Providers page reads `GET /api/providers` using TanStack Query: 30-second
freshness, 10-minute inactive cache retention, 15-second polling while visible,
and stale refetches on focus/reconnect. Requests use the query's abort signal and
the shared bounded retry policy. Cached rows remain visible during background
refreshes and refresh failures; errors offer Retry. The table's layout, icons,
badges, dates, and styles are ported from the old dashboard. `reauth_required`
accounts display “Sign-in required.” Add and row actions are disabled until
their APIs and workflows are implemented; account names are not broken links.
