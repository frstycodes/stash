//! What Windows knows about the network (online? metered?) and the power side of
//! downloading (keeping the PC awake, sleeping or shutting down when the queue is done).

/// The connection as the engine cares about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NetState {
    /// No connection to the internet at all (cable out, Wi-Fi off).
    pub offline: bool,
    /// Windows calls the connection metered (a hotspot, a data plan).
    pub metered: bool,
}

pub fn state() -> NetState {
    use windows::Networking::Connectivity::{NetworkConnectivityLevel, NetworkCostType, NetworkInformation};
    let Ok(profile) = NetworkInformation::GetInternetConnectionProfile() else { return NetState { offline: true, metered: false } };
    // only "none" counts as offline: a proxy or a quirky check can make a working
    // connection look local-only, and holding downloads for that would be worse
    let offline = profile.GetNetworkConnectivityLevel().is_ok_and(|l| l == NetworkConnectivityLevel::None);
    let metered = profile
        .GetConnectionCost()
        .map(|c| {
            let t = c.NetworkCostType().unwrap_or(NetworkCostType::Unrestricted);
            matches!(t, NetworkCostType::Fixed | NetworkCostType::Variable) || c.Roaming().unwrap_or(false) || c.OverDataLimit().unwrap_or(false)
        })
        .unwrap_or(false);
    NetState { offline, metered }
}

/// Keeps the PC from going to sleep while downloads run (the screen may still turn off).
/// Must be called from one thread: Windows keeps the request per thread.
#[derive(Default)]
pub struct Awake {
    on: bool,
}

impl Awake {
    pub fn set(&mut self, on: bool) {
        use windows::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        if on == self.on {
            return;
        }
        self.on = on;
        unsafe {
            SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS });
        }
    }
}

/// Put the PC to sleep.
pub fn sleep() {
    unsafe {
        let _ = windows::Win32::System::Power::SetSuspendState(false, false, false);
    }
}

/// Shut the PC down (as the Start menu would; apps get to save).
pub fn shut_down() {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("shutdown.exe").args(["/s", "/t", "0"]).creation_flags(0x0800_0000).spawn();
}
