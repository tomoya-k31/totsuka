> 🌐 **English** · [日本語](click-to-focus-setup.ja.md)

<!-- generated-from: ai-docs/operations/click-to-focus-setup.md sha256:249a4afe70b35be017281ed4507c45195d52c5273a9a90e70659f153425bb99d -->

# Click a notification to open the task's pane

By default, clicking a totsuka notification opens **Script Editor** and nothing else happens. That is not a bug you can configure away in macOS: the default backend posts notifications through `osascript`, and macOS hands the notification to whatever owns `osascript`.

Switching to the `terminal-notifier` backend makes a click bring your terminal to the front and focus the pane of the task the notification is about.

Takes about five minutes. macOS only.

## 1. Install terminal-notifier

```bash
brew install terminal-notifier
```

## 2. Find your terminal's bundle id

```bash
osascript -e 'id of app "Alacritty"'   # → org.alacritty
```

Substitute your own terminal. Common ones:

| Terminal | Bundle id |
|---|---|
| Alacritty | `org.alacritty` |
| iTerm2 | `com.googlecode.iterm2` |
| Kitty | `net.kovidgoyal.kitty` |
| WezTerm | `com.github.wez.wezterm` |

## 3. Write the `[macos]` table

In your config directory (`$XDG_CONFIG_HOME/totsuka/plugins/`, usually `~/.config/totsuka/plugins/`):

```toml
[macos]
backend = "terminal_notifier"
activate_bundle_id = "org.alacritty"          # the value from step 2

# Defaults, shown for reference — you do not need to write these:
# terminal_notifier_bin = "terminal-notifier"
# click_command = "totsuka focus {task_id}"
```

**The table is `[macos]`, not `[notifier-macos]`.** It is named after the plugin, which is `macos`. Getting this wrong is caught: a top-level table whose name is not in the `[plugins.*]` roster is rejected by `totsuka config validate`.

`backend` is the key that matters. Setting `activate_bundle_id` alone changes nothing while the backend is still the default.

The `totsuka` binary has to be on the `PATH` that terminal-notifier's shell sees when it runs the click command. A standard location (Homebrew, `~/.local/bin`, `/usr/local/bin`) is normally fine.

## 4. Check it

```bash
totsuka config validate    # also probes terminal-notifier; an actionable error if it is missing
```

Then restart `totsuka run` — plugins receive their config at startup, so a running orchestrator keeps the old backend.

To try a click without waiting for a real task, post a notification yourself with the same arguments totsuka uses:

```bash
terminal-notifier -title "test" -subtitle "click-to-focus" -message "click me" \
  -group "totsuka-1" -activate "org.alacritty" -execute "totsuka focus '1'"
```

Your terminal should come to the front. The first time, macOS may ask whether terminal-notifier may send notifications — allow it.

With `totsuka run` going, clicking a real notification should both raise the terminal and focus that task's pane. When several tasks are running, each notification opens its own pane.

## When it does not work

| Symptom | Likely cause | Fix |
|---|---|---|
| Notifications arrive, clicking does nothing | `backend` is still the default `osascript` | Set `backend = "terminal_notifier"` under `[macos]`, then restart `totsuka run` |
| You edited the config and nothing changed | The table is named `[notifier-macos]`, or the settings ended up under `[plugins.macos]` (the roster) instead of `[macos]` | `[plugins.macos]` declares the plugin; `[macos]` configures it. `totsuka config validate` rejects an unknown top-level table, so a wrong name is reported rather than ignored |
| The app comes forward but the pane does not change | `totsuka run` is not running (the focus command quietly does nothing), the pane is already closed, or the agent you use cannot control panes | Run `totsuka focus <task-id>` by hand — it prints the reason |
| The click runs but the app does not come forward | `activate_bundle_id` unset or wrong | Recheck it with step 2 |
| `config validate` reports a terminal-notifier error | Not installed, not on `PATH`, or `terminal_notifier_bin` is wrong | Install it, or give an absolute path. To go without it, set `backend = "osascript"` — notifications still arrive, clicks do nothing |
| Notifications arrive but clicks never worked, and the log warns about terminal-notifier | Not installed; each send falls back to `osascript` | Notifications are unaffected. Install terminal-notifier if you want click-to-focus |
| `totsuka focus` prints a 401 | The running event receiver and the configured auth token disagree | Align the token and restart `totsuka run` |

## Choosing which events notify you

The same table controls which events produce a notification at all. Everything is on by default; list only what you want to turn off.

```toml
[macos]
backend = "terminal_notifier"
activate_bundle_id = "org.alacritty"

# Applies to every workflow
[macos.filter.events]
done = false
pending = false

# Per-workflow override — wins over the global setting above
[macos.filter.workflows.slack-reply]
done = true
```

The event names are `waiting_input`, `done`, `failed`, `pending`, `escalated`, and `verification_pending`. The workflow name is the one from your `[[workflows]]` entry.

The most specific setting wins: a per-workflow toggle beats the global one, and an event you never mention anywhere is delivered. A typo is not silently ignored — `totsuka config validate` rejects an unknown event name and lists the valid ones.

## Related

The full setup path for a new machine is in the [setup playbook](setup-playbook.md).

Detailed design notes and the reasoning behind these steps live in `ai-docs/operations/click-to-focus-setup.md` in the repository.
