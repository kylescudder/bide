# Bide

Bide is a lightweight Linux CLI for persistent countdown timers and one-off
wall-clock alarms. It keeps scheduling, recovery, persistence, and notification
logic behind one scriptable interface so desktop adapters do not need to know
how time is managed.

## Install

Download the versioned binary for your architecture from GitHub Releases, place
it at `~/.local/bin/bide`, then install the files in `systemd/` to
`~/.config/systemd/user/` and run:

```sh
systemctl --user daemon-reload
systemctl --user enable --now bide.timer
```

The persistent user timer invokes a short-lived process every 15 seconds. There
is no daemon. `Persistent=true` recovers missed activations after the user
manager starts; Bide itself records notification delivery before returning,
so overdue items are processed once. A user service cannot wake powered-off
hardware, but overdue items are recovered after boot/login.

## CLI

```sh
bide timer add '1h 30m' --label Laundry
bide alarm add 'tomorrow 08:00' --label 'Wake up'
bide list
bide list --json
bide show ID --json
bide pause ID
bide resume ID
bide adjust ID +5m
bide cancel ID
bide restart ID
bide waybar
bide watch --waybar
```

Durations accept combinations of `h`, `m`, and `s`. Alarms accept `HH:MM`,
`tomorrow HH:MM`, and `YYYY-MM-DD HH:MM` in the current local timezone. A bare
time already passed today means tomorrow. Explicit past dates and ambiguous or
nonexistent local DST times are rejected.

Commands print actionable errors to stderr and return 2 for invalid input or a
failed operation. IDs are UUIDv4 values and may be addressed by an unambiguous
prefix.

## State and timing

State is an atomic, locked JSON document at
`$XDG_STATE_HOME/bide/state.json` (normally
`~/.local/state/bide/state.json`). Its top-level `schema_version` is `1`.
Items include stable `id`, `kind`, `status`, timestamps, timer duration and
remaining values, and the reserved nullable `recurrence` field.

Running timers use Linux `CLOCK_BOOTTIME`, which advances over suspend and is
immune to wall-clock changes. Across reboot, the persisted wall deadline is
used for overdue recovery. Alarms are stored as UTC instants resolved from the
local timezone when created.

Configuration may be written to `$XDG_CONFIG_HOME/bide/config.json`:

```json
{
  "notify_command": "notify-send -u critical -t 0 -- \"$1\" \"$2\"",
  "sound_command": "canberra-gtk-play -i alarm-clock-elapsed"
}
```

Notification command arguments are title (`$1`) and label/body (`$2`). Delivery
failure is reported by the tick but never rolls back expiry or causes duplicate
delivery.

## Waybar contract

`bide waybar` emits exactly one JSON object. `text` is empty when there is
no active countdown. Otherwise it contains the soonest countdown. `tooltip`
lists active timers and upcoming alarms; `class` is `timer` plus `running`,
`paused`, or `idle`; `percentage` is present for active timers; and `alt` is the
displayed stable ID. `watch --waybar` emits one object per line each second so
Waybar refreshes promptly without starting additional processes.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
