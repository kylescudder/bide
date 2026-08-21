use anyhow::{bail, Result};
use bide::*;
use chrono::{Duration, Local};
use clap::{Parser, Subcommand};
use serde_json::json;
use std::{env, thread, time::Duration as StdDuration};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}
#[derive(Subcommand)]
enum Cmd {
    Timer {
        #[command(subcommand)]
        command: TimerCmd,
    },
    Alarm {
        #[command(subcommand)]
        command: AlarmCmd,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Adjust {
        id: String,
        amount: String,
    },
    Cancel {
        id: String,
    },
    Restart {
        id: String,
    },
    Waybar {
        #[command(subcommand)]
        action: Option<WaybarAction>,
    },
    Rofi {
        input: Option<String>,
    },
    Watch {
        #[arg(long)]
        waybar: bool,
    },
    Tick,
}
#[derive(Subcommand)]
enum TimerCmd {
    Add {
        duration: String,
        #[arg(long, default_value = "")]
        label: String,
    },
}
#[derive(Subcommand)]
enum AlarmCmd {
    Add {
        when: String,
        #[arg(long, default_value = "")]
        label: String,
    },
}
#[derive(Subcommand)]
enum WaybarAction {
    Toggle,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("bide: {e:#}");
        std::process::exit(2)
    }
}
fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = Store::new(Store::default_path()?);
    let clock = SystemClock;
    let (c1, c2) = config_commands();
    let notifier = CommandNotifier {
        notify_command: c1,
        sound_command: c2,
    };
    match cli.command {
        Cmd::Timer {
            command: TimerCmd::Add { duration, label },
        } => {
            let sec = parse_duration(&duration)?;
            let i = store.transaction(|s| Ok(add_timer(s, sec, label, &clock)))?;
            println!("{}", i.id)
        }
        Cmd::Alarm {
            command: AlarmCmd::Add { when, label },
        } => {
            let d = parse_alarm(&when, Local::now())?;
            let i = store.transaction(|s| Ok(add_alarm(s, d, label, &clock)))?;
            println!("{}", i.id)
        }
        Cmd::List { json } => store.transaction(|s| {
            refresh(s, &clock, &notifier);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &json!({"schema_version":SCHEMA_VERSION,"items":s.items})
                    )?
                )
            } else {
                for i in &s.items {
                    println!(
                        "{}\t{:?}\t{:?}\t{}",
                        &i.id.to_string()[..8],
                        i.kind,
                        i.status,
                        i.label
                    )
                }
            }
            Ok(())
        })?,
        Cmd::Show { id, json } => store.transaction(|s| {
            refresh(s, &clock, &notifier);
            let i = find_mut(s, &id)?;
            if json {
                println!("{}", serde_json::to_string_pretty(i)?)
            } else {
                println!("{} {:?} {:?} {}", i.id, i.kind, i.status, i.label)
            }
            Ok(())
        })?,
        Cmd::Pause { id } => store.transaction(|s| {
            refresh(s, &clock, &notifier);
            let i = find_mut(s, &id)?;
            if i.kind != Kind::Timer || i.status != Status::Running {
                bail!("only a running timer can be paused")
            }
            i.remaining_seconds = Some(remaining(i, &clock));
            i.status = Status::Paused;
            i.deadline = None;
            Ok(())
        })?,
        Cmd::Resume { id } => store.transaction(|s| {
            let i = find_mut(s, &id)?;
            if i.kind != Kind::Timer || i.status != Status::Paused {
                bail!("only a paused timer can be resumed")
            }
            let r = i.remaining_seconds.unwrap_or(0);
            i.status = Status::Running;
            i.deadline = Some(clock.now() + Duration::seconds(r));
            i.started_boottime_seconds = Some(clock.boottime());
            i.boot_id = Some(clock.boot_id());
            Ok(())
        })?,
        Cmd::Adjust { id, amount } => {
            let (sign, raw) = if let Some(x) = amount.strip_prefix('+') {
                (1, x)
            } else if let Some(x) = amount.strip_prefix('-') {
                (-1, x)
            } else {
                bail!("adjustment must start with + or -")
            };
            let delta = parse_duration(raw)? * sign;
            store.transaction(|s| {
                refresh(s, &clock, &notifier);
                let i = find_mut(s, &id)?;
                if i.kind != Kind::Timer || matches!(i.status, Status::Cancelled) {
                    bail!("only an active or completed timer can be adjusted")
                }
                let r = remaining(i, &clock) + delta;
                if r <= 0 {
                    i.status = Status::Completed;
                    i.completed_at = Some(clock.now())
                } else {
                    i.remaining_seconds = Some(r);
                    i.duration_seconds = Some((i.duration_seconds.unwrap_or(0) + delta).max(1));
                    if i.status == Status::Running {
                        i.deadline = Some(clock.now() + Duration::seconds(r));
                        i.started_boottime_seconds = Some(clock.boottime());
                        i.boot_id = Some(clock.boot_id())
                    }
                }
                Ok(())
            })?
        }
        Cmd::Cancel { id } => store.transaction(|s| {
            let i = find_mut(s, &id)?;
            i.status = Status::Cancelled;
            Ok(())
        })?,
        Cmd::Restart { id } => store.transaction(|s| {
            let i = find_mut(s, &id)?;
            if i.kind != Kind::Timer {
                bail!("only timers can be restarted")
            }
            let d = i.duration_seconds.unwrap_or(0);
            i.status = Status::Running;
            i.remaining_seconds = Some(d);
            i.deadline = Some(clock.now() + Duration::seconds(d));
            i.started_boottime_seconds = Some(clock.boottime());
            i.boot_id = Some(clock.boot_id());
            i.completed_at = None;
            i.notified_at = None;
            Ok(())
        })?,
        Cmd::Waybar { action: None } => store.transaction(|s| {
            refresh(s, &clock, &notifier);
            println!("{}", waybar(s, &clock));
            Ok(())
        })?,
        Cmd::Waybar {
            action: Some(WaybarAction::Toggle),
        } => store.transaction(|s| {
            refresh(s, &clock, &notifier);
            let item = s
                .items
                .iter_mut()
                .filter(|i| {
                    i.kind == Kind::Timer && matches!(i.status, Status::Running | Status::Paused)
                })
                .min_by_key(|i| (i.status == Status::Paused, remaining(i, &clock)))
                .ok_or_else(|| anyhow::anyhow!("no active countdown timer"))?;
            if item.status == Status::Running {
                item.remaining_seconds = Some(remaining(item, &clock));
                item.status = Status::Paused;
                item.deadline = None;
            } else {
                let r = item.remaining_seconds.unwrap_or(0);
                item.status = Status::Running;
                item.deadline = Some(clock.now() + Duration::seconds(r));
                item.started_boottime_seconds = Some(clock.boottime());
                item.boot_id = Some(clock.boot_id());
            }
            Ok(())
        })?,
        Cmd::Rofi { input } => {
            store.transaction(|s| rofi(s, &clock, &notifier, input.unwrap_or_default()))?
        }
        Cmd::Watch { waybar: enabled } => {
            if !enabled {
                bail!("watch currently requires --waybar")
            }
            loop {
                store.transaction(|s| {
                    refresh(s, &clock, &notifier);
                    println!("{}", waybar(s, &clock));
                    Ok(())
                })?;
                thread::sleep(StdDuration::from_secs(1))
            }
        }
        Cmd::Tick => store.transaction(|s| {
            refresh(s, &clock, &notifier);
            Ok(())
        })?,
    }
    Ok(())
}

fn rofi(
    state: &mut State,
    clock: &dyn Clock,
    notifier: &dyn Notifier,
    input: String,
) -> Result<()> {
    refresh(state, clock, notifier);
    let retv = env::var("ROFI_RETV").unwrap_or_else(|_| "0".into());
    let info = env::var("ROFI_INFO").unwrap_or_default();
    let data = env::var("ROFI_DATA").unwrap_or_default();
    if retv == "0" && env::var_os("BIDE_ROFI_DISPLAYED").is_some() {
        if let Some(item) = state
            .items
            .iter()
            .filter(|i| {
                i.kind == Kind::Timer && matches!(i.status, Status::Running | Status::Paused)
            })
            .min_by_key(|i| (i.status == Status::Paused, remaining(i, clock)))
        {
            rofi_header("Timer actions", "Choose an action", true, "");
            for (label, action) in [
                ("Pause / resume", "toggle"),
                ("Add time", "add"),
                ("Subtract time", "subtract"),
                ("Restart", "restart"),
                ("Cancel", "cancel"),
            ] {
                rofi_row(label, "", &format!("action|{}|{action}", item.id));
            }
            return Ok(());
        }
    }
    if retv == "2" && data == "timer_duration" {
        parse_duration(&input)?;
        rofi_header(
            "Timer label",
            "Optional label; press Enter to create",
            false,
            &format!("timer_label|{input}"),
        );
        return Ok(());
    }
    if retv == "2" && data == "alarm_time" {
        parse_alarm(&input, Local::now())?;
        rofi_header(
            "Alarm label",
            "Optional label; press Enter to create",
            false,
            &format!("alarm_label|{input}"),
        );
        return Ok(());
    }
    if retv == "2" && data.starts_with("adjust|") {
        let parts: Vec<_> = data.split('|').collect();
        let delta = parse_duration(&input)? * if parts[2] == "-" { -1 } else { 1 };
        let item = find_mut(state, parts[1])?;
        let next = remaining(item, clock) + delta;
        if next <= 0 {
            item.status = Status::Completed;
            item.completed_at = Some(clock.now());
        } else {
            item.remaining_seconds = Some(next);
            item.duration_seconds = Some((item.duration_seconds.unwrap_or(0) + delta).max(1));
            if item.status == Status::Running {
                item.deadline = Some(clock.now() + Duration::seconds(next));
                item.started_boottime_seconds = Some(clock.boottime());
                item.boot_id = Some(clock.boot_id());
            }
        }
    } else if retv == "2" {
        if let Some(duration) = data.strip_prefix("timer_label|") {
            let seconds = parse_duration(duration)?;
            add_timer(state, seconds, input, clock);
        } else if let Some(when) = data.strip_prefix("alarm_label|") {
            let deadline = parse_alarm(when, Local::now())?;
            add_alarm(state, deadline, input, clock);
        }
    } else if retv == "1" {
        match info.as_str() {
            "new_timer" => {
                rofi_header(
                    "New timer",
                    "Duration: 30s, 10m, or 1h 30m",
                    false,
                    "timer_duration",
                );
                return Ok(());
            }
            "new_alarm" => {
                rofi_header(
                    "New alarm",
                    "Time: 14:30, tomorrow 08:00, or 2026-09-01 09:15",
                    false,
                    "alarm_time",
                );
                return Ok(());
            }
            x if x.starts_with("item|") => {
                let id = &x[5..];
                let item = find_mut(state, id)?;
                if item.kind == Kind::Alarm {
                    rofi_header(
                        "Alarm actions",
                        &format!(
                            "Scheduled for {}",
                            item.deadline.unwrap().with_timezone(&Local).format("%F %R")
                        ),
                        true,
                        "",
                    );
                    rofi_row("Cancel", "", &format!("action|{id}|cancel"));
                } else {
                    rofi_header("Timer actions", "Choose an action", true, "");
                    for (label, action) in [
                        ("Pause / resume", "toggle"),
                        ("Add time", "add"),
                        ("Subtract time", "subtract"),
                        ("Restart", "restart"),
                        ("Cancel", "cancel"),
                    ] {
                        rofi_row(label, "", &format!("action|{id}|{action}"));
                    }
                }
                return Ok(());
            }
            x if x.starts_with("action|") => {
                let parts: Vec<_> = x.split('|').collect();
                let id = parts[1];
                let action = parts[2];
                let item = find_mut(state, id)?;
                match action {
                    "toggle" if item.status == Status::Running => {
                        item.remaining_seconds = Some(remaining(item, clock));
                        item.status = Status::Paused;
                        item.deadline = None
                    }
                    "toggle" if item.status == Status::Paused => {
                        let r = item.remaining_seconds.unwrap_or(0);
                        item.status = Status::Running;
                        item.deadline = Some(clock.now() + Duration::seconds(r));
                        item.started_boottime_seconds = Some(clock.boottime());
                        item.boot_id = Some(clock.boot_id())
                    }
                    "restart" => {
                        let d = item.duration_seconds.unwrap_or(0);
                        item.status = Status::Running;
                        item.remaining_seconds = Some(d);
                        item.deadline = Some(clock.now() + Duration::seconds(d));
                        item.started_boottime_seconds = Some(clock.boottime());
                        item.boot_id = Some(clock.boot_id());
                        item.notified_at = None
                    }
                    "cancel" => item.status = Status::Cancelled,
                    "add" | "subtract" => {
                        let sign = if action == "add" { "+" } else { "-" };
                        rofi_header(
                            "Adjust timer",
                            "Enter a duration such as 5m or 30s",
                            false,
                            &format!("adjust|{id}|{sign}"),
                        );
                        return Ok(());
                    }
                    _ => bail!("action is not available"),
                }
            }
            _ => {}
        }
    }
    rofi_header(
        "Timers & Alarms",
        "Create or manage multiple timers and alarms",
        true,
        "",
    );
    rofi_row("New timer", "Countdown", "new_timer");
    rofi_row("New alarm", "Clock time", "new_alarm");
    let mut shown = 0;
    for i in state
        .items
        .iter()
        .filter(|i| matches!(i.status, Status::Running | Status::Paused))
    {
        let subtitle = match i.kind {
            Kind::Timer => format!("{}s · {:?}", remaining(i, clock), i.status),
            Kind::Alarm => format!(
                "{}",
                i.deadline.unwrap().with_timezone(&Local).format("%F %R")
            ),
        };
        rofi_row(
            if i.label.is_empty() {
                "Untitled"
            } else {
                &i.label
            },
            &subtitle,
            &format!("item|{}", i.id),
        );
        shown += 1
    }
    if shown == 0 {
        println!("No active timers or alarms\0nonselectable\x1ftrue");
    }
    Ok(())
}
fn rofi_header(prompt: &str, message: &str, no_custom: bool, data: &str) {
    println!("\0prompt\x1f{prompt}");
    println!("\0message\x1f{message}");
    println!(
        "\0no-custom\x1f{}",
        if no_custom { "true" } else { "false" }
    );
    if !data.is_empty() {
        println!("\0data\x1f{data}")
    }
}
fn rofi_row(label: &str, subtitle: &str, info: &str) {
    let safe = label
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let sub = subtitle
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    println!("{label}\0display\x1f<span weight='medium'>{safe}</span>  <span size='small' foreground='#7f849c'>·  {sub}</span>\x1finfo\x1f{info}")
}
fn waybar(s: &State, c: &dyn Clock) -> String {
    let mut timers: Vec<_> = s
        .items
        .iter()
        .filter(|i| i.kind == Kind::Timer && matches!(i.status, Status::Running | Status::Paused))
        .collect();
    timers.sort_by_key(|i| (i.status == Status::Paused, remaining(i, c)));
    let alarms: Vec<_> = s
        .items
        .iter()
        .filter(|i| i.kind == Kind::Alarm && i.status == Status::Running)
        .collect();
    let Some(i) = timers.first() else {
        return json!({"text":"","tooltip":alarms.iter().map(|a|format!("Alarm · {}",a.label)).collect::<Vec<_>>().join("\n"),"class":["timer","idle"]}).to_string();
    };
    let r = remaining(i, c);
    let text = format!(
        "󰔛 {} · {:02}:{:02}",
        if i.label.is_empty() {
            "Timer"
        } else {
            &i.label
        },
        r / 60,
        r % 60
    );
    let pct = if i.duration_seconds.unwrap_or(0) > 0 {
        (r * 100 / i.duration_seconds.unwrap()).clamp(0, 100)
    } else {
        0
    };
    let tooltip = timers
        .iter()
        .map(|t| format!("{} · {}s · {:?}", t.label, remaining(t, c), t.status))
        .chain(alarms.iter().map(|a| {
            format!(
                "Alarm · {} · {}",
                a.label,
                a.deadline.unwrap().with_timezone(&Local).format("%F %R")
            )
        }))
        .collect::<Vec<_>>()
        .join("\n");
    json!({"text":text,"tooltip":tooltip,"class":["timer",if i.status==Status::Paused{"paused"}else{"running"}],"percentage":pct,"alt":i.id.to_string()}).to_string()
}
