use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Local, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use uuid::Uuid;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Timer,
    Alarm,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Paused,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: Uuid,
    pub kind: Kind,
    pub status: Status,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub deadline: Option<DateTime<Utc>>,
    pub duration_seconds: Option<i64>,
    pub remaining_seconds: Option<i64>,
    pub started_boottime_seconds: Option<i64>,
    pub boot_id: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub notified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub recurrence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub schema_version: u32,
    pub items: Vec<Item>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            items: vec![],
        }
    }
}

pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
    fn boottime(&self) -> i64;
    fn boot_id(&self) -> String;
}
pub struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn boottime(&self) -> i64 {
        unsafe {
            let mut ts: libc::timespec = std::mem::zeroed();
            libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts);
            ts.tv_sec
        }
    }
    fn boot_id(&self) -> String {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .into()
    }
}

pub trait Notifier {
    fn notify(&self, item: &Item) -> Result<()>;
}
pub struct CommandNotifier {
    pub notify_command: String,
    pub sound_command: String,
}
impl Notifier for CommandNotifier {
    fn notify(&self, item: &Item) -> Result<()> {
        let title = match item.kind {
            Kind::Timer => "Timer complete",
            Kind::Alarm => "Alarm",
        };
        let body = if item.label.is_empty() {
            title.to_string()
        } else {
            item.label.clone()
        };
        let n = Command::new("sh")
            .arg("-c")
            .arg(&self.notify_command)
            .arg("bide")
            .arg(title)
            .arg(&body)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let s = Command::new("sh")
            .arg("-c")
            .arg(&self.sound_command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if n.is_err() || s.is_err() {
            bail!("failed to start notification or sound command")
        }
        Ok(())
    }
}

pub struct Store {
    path: PathBuf,
}
impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn default_path() -> Result<PathBuf> {
        Ok(dirs::state_dir()
            .context("cannot determine XDG state directory")?
            .join("bide/state.json"))
    }
    pub fn transaction<T>(&self, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        if let Some(p) = self.path.parent() {
            fs::create_dir_all(p)?;
        }
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock_exclusive()?;
        let mut state = if self.path.exists() {
            serde_json::from_reader(File::open(&self.path)?).context("invalid state file")?
        } else {
            State::default()
        };
        let out = f(&mut state)?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut file = File::create(&tmp)?;
            serde_json::to_writer_pretty(&mut file, &state)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(tmp, &self.path)?;
        FileExt::unlock(&lock)?;
        Ok(out)
    }

    pub fn read<T>(&self, f: impl FnOnce(&State) -> Result<T>) -> Result<T> {
        if let Some(p) = self.path.parent() {
            fs::create_dir_all(p)?;
        }
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock_shared()?;
        let state = if self.path.exists() {
            serde_json::from_reader(File::open(&self.path)?).context("invalid state file")?
        } else {
            State::default()
        };
        let out = f(&state)?;
        FileExt::unlock(&lock)?;
        Ok(out)
    }
}

pub fn parse_duration(input: &str) -> Result<i64> {
    let mut total = 0i64;
    let mut number = String::new();
    let mut saw = false;
    for c in input.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            number.push(c);
            continue;
        }
        if c.is_whitespace() && number.is_empty() {
            continue;
        }
        if number.is_empty() {
            bail!("invalid duration: expected a number before '{c}'")
        }
        let n: i64 = number.parse()?;
        let factor = match c {
            's' | 'S' => 1,
            'm' | 'M' => 60,
            'h' | 'H' => 3600,
            _ => bail!("invalid duration unit '{c}'; use h, m, or s"),
        };
        total = total
            .checked_add(n.checked_mul(factor).context("duration is too large")?)
            .context("duration is too large")?;
        number.clear();
        saw = true;
    }
    if !saw || total <= 0 {
        bail!("duration must be greater than zero")
    } else {
        Ok(total)
    }
}

pub fn parse_alarm(input: &str, now: DateTime<Local>) -> Result<DateTime<Utc>> {
    let input = input.trim();
    if let Ok(explicit) = DateTime::parse_from_rfc3339(input) {
        let deadline = explicit.with_timezone(&Utc);
        if deadline <= now.with_timezone(&Utc) {
            bail!("explicit alarm time must be in the future")
        }
        return Ok(deadline);
    }
    let naive = if let Some(rest) = input.strip_prefix("tomorrow ") {
        NaiveDateTime::new(
            now.date_naive() + Duration::days(1),
            NaiveTime::parse_from_str(rest, "%H:%M").context("expected 'tomorrow HH:MM'")?,
        )
    } else if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M") {
        dt
    } else if let Ok(t) = NaiveTime::parse_from_str(input, "%H:%M") {
        let mut dt = NaiveDateTime::new(now.date_naive(), t);
        if dt <= now.naive_local() {
            dt += Duration::days(1);
        }
        dt
    } else {
        bail!("invalid alarm time; use HH:MM, 'tomorrow HH:MM', or YYYY-MM-DD HH:MM")
    };
    let local = match Local.from_local_datetime(&naive) {
        LocalResult::Single(v) => v,
        LocalResult::Ambiguous(_, _) => bail!(
            "alarm time is ambiguous due to a daylight-saving transition; use an explicit offset"
        ),
        LocalResult::None => bail!("alarm time does not exist due to a daylight-saving transition"),
    };
    if local <= now {
        bail!("explicit alarm time must be in the future")
    }
    Ok(local.with_timezone(&Utc))
}

pub fn remaining(item: &Item, clock: &dyn Clock) -> i64 {
    if item.kind != Kind::Timer {
        return item
            .deadline
            .map(|d| (d - clock.now()).num_seconds())
            .unwrap_or(0);
    }
    if item.status == Status::Paused {
        return item.remaining_seconds.unwrap_or(0);
    }
    if item.boot_id.as_deref() == Some(&clock.boot_id()) {
        (item.remaining_seconds.unwrap_or(0)
            - (clock.boottime() - item.started_boottime_seconds.unwrap_or(clock.boottime())))
        .max(0)
    } else {
        item.deadline
            .map(|d| (d - clock.now()).num_seconds().max(0))
            .unwrap_or(0)
    }
}

pub fn refresh(state: &mut State, clock: &dyn Clock, notifier: &dyn Notifier) {
    for item in &mut state.items {
        if item.status != Status::Running {
            continue;
        }
        let expired = match item.kind {
            Kind::Timer => remaining(item, clock) <= 0,
            Kind::Alarm => item.deadline.is_some_and(|d| d <= clock.now()),
        };
        if expired {
            item.status = Status::Completed;
            item.completed_at = Some(clock.now());
            if item.notified_at.is_none() {
                let _ = notifier.notify(item);
                item.notified_at = Some(clock.now());
            }
        }
    }
}

pub fn add_timer(state: &mut State, seconds: i64, label: String, clock: &dyn Clock) -> Item {
    let item = Item {
        id: Uuid::new_v4(),
        kind: Kind::Timer,
        status: Status::Running,
        label,
        created_at: clock.now(),
        deadline: Some(clock.now() + Duration::seconds(seconds)),
        duration_seconds: Some(seconds),
        remaining_seconds: Some(seconds),
        started_boottime_seconds: Some(clock.boottime()),
        boot_id: Some(clock.boot_id()),
        completed_at: None,
        notified_at: None,
        recurrence: None,
    };
    state.items.push(item.clone());
    item
}
pub fn add_alarm(
    state: &mut State,
    deadline: DateTime<Utc>,
    label: String,
    clock: &dyn Clock,
) -> Item {
    let item = Item {
        id: Uuid::new_v4(),
        kind: Kind::Alarm,
        status: Status::Running,
        label,
        created_at: clock.now(),
        deadline: Some(deadline),
        duration_seconds: None,
        remaining_seconds: None,
        started_boottime_seconds: None,
        boot_id: None,
        completed_at: None,
        notified_at: None,
        recurrence: None,
    };
    state.items.push(item.clone());
    item
}
pub fn find_mut<'a>(state: &'a mut State, id: &str) -> Result<&'a mut Item> {
    let exact = Uuid::parse_str(id).ok();
    let mut found = state
        .items
        .iter_mut()
        .filter(|i| exact == Some(i.id) || i.id.to_string().starts_with(id));
    let item = found
        .next()
        .ok_or_else(|| anyhow!("item '{id}' not found"))?;
    if found.next().is_some() {
        bail!("item ID prefix '{id}' is ambiguous")
    }
    Ok(item)
}

pub fn config_commands() -> (String, String) {
    let path = dirs::config_dir()
        .unwrap_or_default()
        .join("bide/config.json");
    #[derive(Deserialize)]
    struct C {
        notify_command: Option<String>,
        sound_command: Option<String>,
    }
    let c: Option<C> = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    (c.as_ref().and_then(|x|x.notify_command.clone()).unwrap_or_else(||"notify-send -u critical -t 0 -- \"$1\" \"$2\"".into()),c.and_then(|x|x.sound_command).unwrap_or_else(||"canberra-gtk-play -i complete || paplay /usr/share/sounds/freedesktop/stereo/complete.oga".into()))
}

pub fn state_path_for_tests(path: &Path) -> Store {
    Store::new(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct C {
        now: DateTime<Utc>,
        boot: i64,
        id: String,
    }
    impl Clock for C {
        fn now(&self) -> DateTime<Utc> {
            self.now
        }
        fn boottime(&self) -> i64 {
            self.boot
        }
        fn boot_id(&self) -> String {
            self.id.clone()
        }
    }
    struct N(Mutex<usize>);
    impl Notifier for N {
        fn notify(&self, _: &Item) -> Result<()> {
            *self.0.lock().unwrap() += 1;
            Ok(())
        }
    }
    struct F;
    impl Notifier for F {
        fn notify(&self, _: &Item) -> Result<()> {
            bail!("simulated delivery failure")
        }
    }
    #[test]
    fn durations() {
        assert_eq!(parse_duration("1h 30m").unwrap(), 5400);
        assert_eq!(parse_duration("30s").unwrap(), 30);
        assert!(parse_duration("5x").is_err())
    }
    #[test]
    fn transitions_and_notification_once() {
        let c = C {
            now: Utc::now(),
            boot: 100,
            id: "a".into(),
        };
        let n = N(Mutex::new(0));
        let mut s = State::default();
        let mut i = add_timer(&mut s, 10, "Tea".into(), &c);
        assert_ne!(i.id, add_timer(&mut s, 10, "Tea".into(), &c).id);
        i.remaining_seconds = Some(0);
        s.items[0] = i;
        refresh(&mut s, &c, &n);
        refresh(&mut s, &c, &n);
        assert_eq!(*n.0.lock().unwrap(), 1);
        assert_eq!(s.items[0].status, Status::Completed)
    }
    #[test]
    fn persistence() {
        let d = tempfile::tempdir().unwrap();
        let store = state_path_for_tests(&d.path().join("state.json"));
        store
            .transaction(|s| {
                s.schema_version = 1;
                Ok(())
            })
            .unwrap();
        store
            .transaction(|s| {
                assert_eq!(s.schema_version, 1);
                Ok(())
            })
            .unwrap()
    }
    #[test]
    fn bare_past_moves_tomorrow() {
        let n = Local
            .with_ymd_and_hms(2026, 8, 21, 15, 0, 0)
            .single()
            .unwrap();
        let a = parse_alarm("14:30", n).unwrap();
        assert!(a > n.with_timezone(&Utc))
    }
    #[test]
    fn explicit_offset_and_invalid_dates() {
        let n = Local
            .with_ymd_and_hms(2026, 8, 21, 15, 0, 0)
            .single()
            .unwrap();
        assert!(parse_alarm("2026-09-01T09:15:00+02:00", n).is_ok());
        assert!(parse_alarm("2026-02-30 09:15", n).is_err());
        assert!(parse_alarm("2020-01-01 09:15", n).is_err());
    }
    #[test]
    fn delivery_failure_is_still_recorded_once() {
        let c = C {
            now: Utc::now(),
            boot: 100,
            id: "a".into(),
        };
        let mut s = State::default();
        let mut item = add_timer(&mut s, 10, "Tea".into(), &c);
        item.remaining_seconds = Some(0);
        s.items[0] = item;
        refresh(&mut s, &c, &F);
        assert!(s.items[0].notified_at.is_some());
        assert_eq!(s.items[0].status, Status::Completed);
    }
    #[test]
    fn reboot_falls_back_to_wall_deadline() {
        let now = Utc::now();
        let old = C {
            now,
            boot: 500,
            id: "old".into(),
        };
        let mut s = State::default();
        let item = add_timer(&mut s, 60, "".into(), &old);
        let rebooted = C {
            now: now + Duration::seconds(70),
            boot: 5,
            id: "new".into(),
        };
        assert_eq!(remaining(&item, &rebooted), 0);
    }
}
