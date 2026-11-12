//! Quiet hours and daily start/stop times: pure checks on minutes after local midnight.

use super::prefs::Schedule;

/// Is `now` inside the window from `from` to `to`? A window may run past midnight
/// (23:00 to 07:00); one that starts and ends at the same time is always open.
pub fn in_window(now: u16, from: u16, to: u16) -> bool {
    if from == to {
        return true;
    }
    if from < to { now >= from && now < to } else { now >= from || now < to }
}

/// Did the clock pass `at` going from `prev` to `now` (minutes, wrapping at midnight)?
/// `prev == now` passes nothing.
pub fn crossed(prev: u16, now: u16, at: u16) -> bool {
    if prev == now {
        return false;
    }
    if prev < now { at > prev && at <= now } else { at > prev || at <= now }
}

/// Downloads may run now (outside quiet hours).
pub fn allowed(s: &Schedule, now: u16) -> bool {
    s.window.is_none_or(|(a, b)| in_window(now, a, b))
}

/// What the daily times ask for between two checks of the clock.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Start,
    Stop,
}

pub fn events(s: &Schedule, prev: u16, now: u16) -> Vec<Event> {
    let mut v = Vec::new();
    if s.start_at.is_some_and(|t| crossed(prev, now, t)) {
        v.push(Event::Start);
    }
    if s.stop_at.is_some_and(|t| crossed(prev, now, t)) {
        v.push(Event::Stop);
    }
    v
}

/// Minutes after local midnight, now.
pub fn local_minute() -> u16 {
    let utc = super::now() as i64;
    (((utc + local_offset()).rem_euclid(86_400)) / 60) as u16
}

/// Seconds to add to UTC for local time.
fn local_offset() -> i64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    let t = (super::now() + 11_644_473_600) * 10_000_000;
    let utc = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    let mut local = FILETIME::default();
    if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_err() {
        return 0;
    }
    let l = ((local.dwHighDateTime as u64) << 32) | local.dwLowDateTime as u64;
    (l as i64 - t as i64) / 10_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u16 = 60;

    #[test]
    fn windows() {
        assert!(in_window(10 * H, 9 * H, 17 * H));
        assert!(!in_window(17 * H, 9 * H, 17 * H));
        assert!(!in_window(8 * H, 9 * H, 17 * H));
        // past midnight
        assert!(in_window(23 * H + 30, 23 * H, 7 * H));
        assert!(in_window(3 * H, 23 * H, 7 * H));
        assert!(!in_window(12 * H, 23 * H, 7 * H));
        assert!(in_window(5, 300, 300));
        let s = Schedule { window: Some((H, 2 * H)), ..Default::default() };
        assert!(allowed(&s, 90));
        assert!(!allowed(&s, 200));
        assert!(allowed(&Schedule::default(), 200));
    }

    #[test]
    fn crossing() {
        assert!(crossed(59, 60, 60));
        assert!(!crossed(60, 61, 60));
        assert!(!crossed(60, 60, 60));
        // over midnight
        assert!(crossed(1439, 1, 0));
        assert!(crossed(1430, 5, 1435));
        assert!(!crossed(1430, 5, 100));
        let s = Schedule { start_at: Some(120), stop_at: Some(130), window: None };
        assert_eq!(events(&s, 100, 125), vec![Event::Start]);
        assert_eq!(events(&s, 100, 140), vec![Event::Start, Event::Stop]);
        assert!(events(&s, 140, 141).is_empty());
    }
}
