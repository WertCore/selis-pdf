# The options page and the first-run guide (SL-4.EXT.07)

The rule, in one sentence: **the options page says what this extension does,
and every sentence on it is checked against the code that does it.**

The page is `options.html`, declared by `options_page` in `manifest.json`,
reachable from the browser's extension menu and from
`chrome.runtime.openOptionsPage()`. The other documents here say what the
package is allowed to *do* (`PERMISSIONS.md`), what it is allowed to *ship*
(`BUNDLING.md`), what it is allowed to *weigh* (`SIZE.md`) and what it *reuses*
(`REUSE.md`); this one is about what it tells the user, which is the part a
store reviewer reads first and the part that goes stale quietly.

## What is on it, and why that and nothing more

| Row | What it is | Why it is allowed to exist |
|---|---|---|
| Welcome guide | Shown once, on a fresh install | The only row with a trigger. See below. |
| Privacy summary | Prose | Claims no account, no analytics, no server, no upload. All four are true today, and the last is true of the *viewer* too: the document is fetched with the browser's own credentials and rendered locally. |
| Share anonymous usage data | One switch, **off by default** | The switch is real — it writes `selis.telemetry.enabled`, the key `ext/adapter.ts` reads — and the port behind it is **inert**: no endpoint, no permission to reach one, `record` is a no-op. So the help text says, in the string itself, that nothing is recorded or transmitted. A page whose switch implied collection would be a false privacy disclosure, and SL-4.EXT.11 has to defend "does not collect user data" to a reviewer. |
| What the browser has allowed | The capability list, built at boot from `ALLOWED_PERMISSIONS` | Written from the approved set rather than typed into the HTML, so a permission added to the manifest fails `options-strings.test.ts` until somebody writes the sentence for it. That is the sign-off made loud rather than quiet. |
| No website access | One sentence, rendered because `ALLOWED_HOST_PERMISSIONS` is empty | The empty case is its own string, not a formatted list. A task that adds a host permission has to delete a line here on purpose, and the sentence stops being true visibly rather than in a diff nobody reads. |
| Chinese, Japanese and Korean text | **No control.** A measured refusal | FONT.10-F1: the full payload is 11 162 268 resident bytes against this extension's 8 MiB store budget, so `installCjkChunk` refuses the install that would cross it, and there is no producer to fetch it from either. A switch that refuses is a control that lies about why, and "coming soon" is a promise the extension cannot keep. The sizes are placeholders filled from `ext/cjk-payload.ts` and checked against `assets/cjk/manifest.json`. |
| Settings | "Show the welcome guide", "Reset all settings" | Both are local. Replay is deliberately **not** a stored-state change: rewriting the first-run flag to "unseen" would let any later visit re-arm it. Reset removes the one key it owns and the button's help text says exactly that, so no confirmation dialog is needed over a consent flag whose default is the safe side. |

## What is deliberately not on it

- **A CJK install button.** See above. This is the row most likely to be
  "just added" by a later task, and it is the one that would make the store
  listing a claim the extension cannot honour.
- **An interception toggle.** The ruleset is installed on every install event
  and the page does not touch it. Turning PDF redirection off is a policy
  question (EXT.02, `PERMISSIONS.md`), and a switch on a privacy page that
  quietly stops a security-shaped feature is worse than no switch.
- **A `file://` toggle.** The browser's "Allow access to file URLs" is a
  browser setting, not an extension setting, and explaining it is SL-4.EXT.09's
  scope. The welcome guide says local files are not opened, rather than
  pretending a control exists.
- **A deep link into the web app.** That is SL-4.EXT.08, and `openExternal` is
  narrowed to `http(s)` for it (ADR-P0020). This page opens nothing.
- **Anything that would need a permission.** No `tabs`, no `scripting`, no
  `clipboardRead`, no `notifications`, no host access. `options-page.test.ts`
  asserts the shipped page and worker name none of those APIs, so "just add a
  link to our website" fails the suite rather than the review.

## The first-run trigger, stated once

`planFirstRun(reason, seen)` in `src/options-state.ts` is the whole rule:

| Event | Flag | Result |
|---|---|---|
| fresh install | not seen | the guide is offered |
| fresh install | seen | declined — never nagged |
| update / browser upgrade / shared-module update | either | declined |
| a reason this build does not recognise | either | declined (fail closed) |
| any of the above | missing, empty or corrupt value | treated as *not seen* |

Two decisions in that table are worth arguing with, so they are written down
rather than left in the code:

1. **Seen is recorded when the guide is rendered, not when it is dismissed.**
   The guide states facts and changes no setting, so there is no consent to
   withhold, and making dismissal load-bearing would re-show the guide to
   everyone who closes the tab without reading it — forever. The failure this
   trades away is "a user who closed the tab in the first 200 ms never read the
   guide", and the recovery is the "Show the welcome guide" button.
2. **A corrupt flag means "not seen".** The alternative loses the guide
   permanently the first time anything other than this module writes that key.

The flag lives in `chrome.storage.local` because an MV3 service worker has no
`localStorage` and the worker is the context that hears `onInstalled`. It is
tens of bytes in a store the `storage` permission (SL-4.EXT.05) already bought;
**no permission was added for this task**.

## Accessibility and i18n

- Every user-visible string is a key in `src/options-strings.ts`. The markup
  contains no text at all, and `options-page.test.ts` fails on any that does —
  so a rename is a catalogue change and a translator never opens an HTML file.
- The one switch has a bound `<label>` and an `aria-describedby` help text, so
  it is announced with the explanation rather than as a bare checkbox.
- Sections are `<section aria-labelledby>`; the guide's heading takes focus
  when the panel opens, so a screen reader announces where the user has arrived.
- One polite live region (`role="status"`) carries every outcome, including
  "that change could not be saved on this device".
- `options.css` uses only ui-kit tokens, and the test checks both halves: no
  colour literal, and every `--selis-*` name it uses is one `tokens.css`
  defines. It also pins `.selis-section[hidden] { display: none }`, without
  which the page's own `display: flex` would beat the user-agent's `[hidden]`
  rule and the first-run flag would control nothing.

## Verifying it

```
pnpm --filter @selis/extension build     # tsc + pack + bundled-only gate + size gate
pnpm --filter @selis/extension test      # the above, then the suite
pnpm --filter @selis/extension typecheck
```

The page itself is not exercised in a browser by any gate — there is no
headless-browser harness in this repository, and adding one is out of scope for
this task. What *is* checked is everything below the DOM: the trigger, the
stores, the catalogue, the manifest wiring, the stylesheet's tokens, and the
absence of any capability the page does not have. The DOM wiring itself
(`src/options-boot.ts`) is the part a reviewer has to read, and it is written
to be read: small functions, one dependency object, every failure guarded.
