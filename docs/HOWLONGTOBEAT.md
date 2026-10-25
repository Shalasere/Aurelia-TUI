# HowLongToBeat

Selecting a game loads completion times alongside the existing ProtonDB and
store metadata. Main + Extras is the total for that play style, not extra time
added to Main Story. Times are rounded to the nearest minute; a positive time
shorter than a minute displays as `1m`. A dash means no submitted time.

`t` and **Actions → HowLongToBeat page** open the same verified game page using
your platform's default browser. The full URL is available through this action
rather than taking another row in the detail pane. On Linux this requires
`xdg-open`; macOS uses `open`, and Windows uses `start`.

## Matching and failures

The lookup searches the public HLTB website by game title, prioritizes exact
normalized title/alias matches, and checks up to three candidates. A result is
shown only if its game page contains the selected Steam app ID in
`profile_steam` or `profile_steam_alt`. This is deliberately conservative:
**No verified match** can mean HLTB's Steam association is missing or the match
was outside the checked candidates. There is no title-only fallback.

**Unavailable** means the lookup failed, not that the game has no entry.
Press `t` to see the failure reason in the status notice. **Cached** means a
previously verified result is displayed because its refresh failed; it does
not necessarily mean your network is offline. HTTP, response-format, and cache
write errors also go through the project's diagnostic logger (stderr when
redirected).

Press `r` to refresh the library and retry failed lookups. Lookups run once per
game per library load, after input has settled, with at most one worker active.
Each network lookup has a 30-second budget and eight-second request timeouts.
HTTP 429 stops the lookup and defers further network lookups for one minute;
refresh after that interval to retry.

## Cache

Entries live at `~/.cache/aurelia-tui/hltb/<steam-app-id>.json`, beneath
`AURELIA_TUI_CACHE_DIR` when overridden. Positive matches are reused for seven
days and negative matches for one day, checked when the next lookup starts.
In-memory results remain unchanged until the library is reloaded. Failed
requests are never stored as negative matches. Expired positive results remain
available if a refresh fails; their timestamps are not extended.

To force a lookup, remove just that game's cache entry and press `r`. For Portal:

```bash
rm ~/.cache/aurelia-tui/hltb/400.json
```

Cache files are versioned, and malformed or incompatible entries are ignored.
Writes use a temporary file in the same directory followed by a rename.

## Maintaining and testing

The provider in `src/interface/hltb.rs` uses public website search and game
pages, whose structure can change. It discovers the search endpoint from the
homepage's Next.js scripts and obtains a short-lived token through that
endpoint's `/init` route. It rediscovers the endpoint once after HTTP
401/403/404; it does not retry rate limits or server errors this way. It checks
at most twelve script references and reuses the HTTP client and endpoint.
The fixtures contain only the public fields needed for matching and times;
no authentication tokens are recorded. The token in HTTP tests is synthetic.

Provider and cache tests live in `src/interface/hltb_tests.rs`, model/render
regressions in `src/interface/game.rs`, and menu tests in `tests/hltb_view.rs`.
The render regression includes installation data, long descriptions, cached
results, and small panes. Run the tests without external services:

```bash
cargo test --locked
```

An opt-in live check covers Portal, Lost Ruins, and Graveyard Keeper. It needs
internet access but no Steam login:

```bash
cargo test --locked live_steam_matches -- --ignored --nocapture
```

Browser launching belongs to `util::browser`, with off-thread results handled
by the browse model; completion-time formatting belongs to `ui::hltb`. The
provider does not render UI or launch processes. Native TLS remains unchanged.

On Linux, a terminal smoke test covers `t` and the Actions menu with both
successful and failing browser launchers. It uses a fake Steam backend, a
seeded HLTB cache, and an `xdg-open` stub; it does not open your real browser.
The existing ProtonDB lookup can still run in the background.

```bash
cargo build --release --locked
python3 scripts/test-hltb-terminal.py target/release/aurelia-tui
```
