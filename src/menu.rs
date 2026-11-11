//! Native right-click menus (stack, items, tray) built with muda.

use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

use crate::settings::{Corner, Display, IconSize, Settings, Sort};

pub mod ids {
    pub const DOWNLOADS: &str = "downloads";
    pub const NEW_DOWNLOAD: &str = "new-download";
    pub const OPEN_FOLDER: &str = "open-folder";
    pub const LOGIN: &str = "login";
    pub const QUIT: &str = "quit";
    pub const ITEM_OPEN: &str = "item-open";
    pub const ITEM_REVEAL: &str = "item-reveal";
    pub const ITEM_RECYCLE: &str = "item-recycle";
    pub const ADD_FOLDER: &str = "add-folder";
    pub const REMOVE_FOLDER: &str = "remove-folder";
    /// "folder:<index>" switches to that folder
    pub const FOLDER_PREFIX: &str = "folder:";
}

fn radio(id: String, label: &str, checked: bool) -> CheckMenuItem {
    CheckMenuItem::with_id(id, label, true, checked, None)
}

/// `folders` are the watched folders' names in order (Downloads first); `current`
/// indexes the one on show.
/// `update` is where Stash's own update stands (None: this copy doesn't update itself).
pub fn stack_menu(s: &Settings, start_at_login: bool, folders: &[String], current: usize, update: Option<&crate::update::Status>) -> Menu {
    let folder_items: Vec<CheckMenuItem> = folders
        .iter()
        .enumerate()
        .map(|(i, name)| radio(format!("{}{i}", ids::FOLDER_PREFIX), name, i == current))
        .collect();
    let add = MenuItem::with_id(ids::ADD_FOLDER, "Add Folder…", true, None);
    // Downloads (index 0) always stays
    let remove = MenuItem::with_id(ids::REMOVE_FOLDER, format!("Remove \"{}\"", folders.get(current).map_or("", |n| n.as_str())), current > 0, None);
    let separator = PredefinedMenuItem::separator();
    let mut folder_refs: Vec<&dyn IsMenuItem> = folder_items.iter().map(|m| m as &dyn IsMenuItem).collect();
    folder_refs.extend([&separator as &dyn IsMenuItem, &add, &remove]);
    let folders_menu = Submenu::with_items("Folders", true, &folder_refs).expect("menu");
    let open_label = format!("Open \"{}\" in Explorer", folders.get(current).map_or("Downloads", |n| n.as_str()));

    let sort = Submenu::with_items(
        "Sort By",
        true,
        &[
            &radio("sort:added".into(), "Date Added", s.sort == Sort::Added) as &dyn IsMenuItem,
            &radio("sort:modified".into(), "Date Modified", s.sort == Sort::Modified),
            &radio("sort:name".into(), "Name", s.sort == Sort::Name),
            &radio("sort:kind".into(), "Kind", s.sort == Sort::Kind),
        ],
    )
    .expect("menu");
    let display = Submenu::with_items(
        "Display As",
        true,
        &[
            &radio("display:folder".into(), "Folder", s.display == Display::Folder) as &dyn IsMenuItem,
            &radio("display:stack".into(), "Stack", s.display == Display::Stack),
        ],
    )
    .expect("menu");
    let limits: Vec<CheckMenuItem> = [5usize, 8, 10, 12]
        .iter()
        .map(|n| radio(format!("limit:{n}"), &format!("{n} Items"), s.limit == *n))
        .collect();
    let limit_refs: Vec<&dyn IsMenuItem> = limits.iter().map(|m| m as &dyn IsMenuItem).collect();
    let limit = Submenu::with_items("Show Most Recent", true, &limit_refs).expect("menu");
    let corner = Submenu::with_items(
        "Position",
        true,
        &[
            &radio("corner:bottom-right".into(), "Bottom Right", s.corner == Corner::BottomRight) as &dyn IsMenuItem,
            &radio("corner:bottom-left".into(), "Bottom Left", s.corner == Corner::BottomLeft),
        ],
    )
    .expect("menu");

    let size = Submenu::with_items(
        "Icon Size",
        true,
        &[
            &radio("size:small".into(), "Small", s.icon_size == IconSize::Small) as &dyn IsMenuItem,
            &radio("size:medium".into(), "Medium", s.icon_size == IconSize::Medium),
            &radio("size:large".into(), "Large", s.icon_size == IconSize::Large),
            &radio("size:extra-large".into(), "Extra Large", s.icon_size == IconSize::ExtraLarge),
        ],
    )
    .expect("menu");

    use crate::update::Status;
    let menu = Menu::new();
    // an update on its way or ready goes first; otherwise "Check for Updates" sits above Quit
    let busy = match update {
        Some(Status::Ready { version, .. }) => Some(MenuItem::with_id("update:apply", format!("Restart to Update to {version}"), true, None)),
        Some(Status::Downloading(v)) => Some(MenuItem::with_id("noop", format!("Downloading Stash {v}…"), false, None)),
        _ => None,
    };
    if let Some(item) = &busy {
        let _ = menu.append_items(&[item as &dyn IsMenuItem, &PredefinedMenuItem::separator()]);
    }
    let check = match update {
        Some(Status::Checking) => Some(MenuItem::with_id("noop", "Checking for Updates…", false, None)),
        Some(Status::Idle | Status::UpToDate | Status::Failed(_)) => Some(MenuItem::with_id("update:check", "Check for Updates", true, None)),
        _ => None,
    };
    let _ = menu.append_items(&[
        &MenuItem::with_id(ids::DOWNLOADS, "Open Stash", true, None) as &dyn IsMenuItem,
        &MenuItem::with_id(ids::NEW_DOWNLOAD, "New Download…", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(ids::OPEN_FOLDER, open_label, true, None),
        &folders_menu,
        &PredefinedMenuItem::separator(),
        &sort,
        &display,
        &limit,
        &size,
        &corner,
        &PredefinedMenuItem::separator(),
        &CheckMenuItem::with_id(ids::LOGIN, "Start at Login", true, start_at_login, None),
    ]);
    if let Some(item) = &check {
        let _ = menu.append(item);
    }
    let _ = menu.append(&MenuItem::with_id(ids::QUIT, "Quit", true, None));
    menu
}

/// A download that's still in the stack: pause or resume it, or cancel it.
pub fn job_menu(id: u64, paused: bool) -> Menu {
    let toggle = if paused { ("resume", "Resume") } else { ("pause", "Pause") };
    Menu::with_items(&[
        &MenuItem::with_id(format!("dl:{}:{id}", toggle.0), toggle.1, true, None) as &dyn IsMenuItem,
        &MenuItem::with_id(ids::DOWNLOADS, "Open Stash", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(format!("dl:delete:{id}"), "Cancel Download", true, None),
    ])
    .expect("menu")
}

pub fn item_menu() -> Menu {
    Menu::with_items(&[
        &MenuItem::with_id(ids::ITEM_OPEN, "Open", true, None) as &dyn IsMenuItem,
        &MenuItem::with_id(ids::ITEM_REVEAL, "Show in Explorer", true, None),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(ids::ITEM_RECYCLE, "Move to Recycle Bin", true, None),
    ])
    .expect("menu")
}

/// Apply a "key:value" settings id from the menu. Returns true if it was one.
pub fn apply_setting(id: &str, s: &mut Settings) -> bool {
    let Some((key, value)) = id.split_once(':') else { return false };
    match (key, value) {
        ("sort", "added") => s.sort = Sort::Added,
        ("sort", "modified") => s.sort = Sort::Modified,
        ("sort", "name") => s.sort = Sort::Name,
        ("sort", "kind") => s.sort = Sort::Kind,
        ("display", "folder") => s.display = Display::Folder,
        ("display", "stack") => s.display = Display::Stack,
        ("corner", "bottom-right") => s.corner = Corner::BottomRight,
        ("corner", "bottom-left") => s.corner = Corner::BottomLeft,
        ("size", "small") => s.icon_size = IconSize::Small,
        ("size", "medium") => s.icon_size = IconSize::Medium,
        ("size", "large") => s.icon_size = IconSize::Large,
        ("size", "extra-large") => s.icon_size = IconSize::ExtraLarge,
        ("limit", n) => s.limit = n.parse().unwrap_or(10),
        _ => return false,
    }
    true
}
