mod theme;

use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use slint_keyos_platform::app_ui2;
use slint_keyos_platform::fs::{self, Location, OpenFlags};
use slint_keyos_platform::qrcode;
use slint_keyos_platform::slint::{Color, ComponentHandle, Image, ModelRc, Timer, VecModel};
use wallet_core::backup::GiftRecord;
use wallet_core::{bill, derive, keys, qr, template, Variant};

app_ui2!("Paper Wallet");
security::use_api!();

/// Hidden app-private metadata store on Internal (User) storage — kept out
/// of the visible tree so a user-chosen export directory can never collide
/// with it (the full backup JSON parses as a GiftRecord and would show up
/// as a phantom gift).
const META_DIR: &str = "/.paper-wallets-meta";
/// Default export directory offered in the save browser (on Airlock).
const EXPORT_DIR: &str = "/paper-wallets";
/// Custom bill designs are scanned from here (Internal and Airlock).
const TEMPLATES_DIR: &str = "/paper-wallets/templates";
/// "Export design kit" writes the blank template + satoshi example here.
const DESIGN_KIT_DIR: &str = "/paper-wallets/design-kit";

type Fs = fs::FileSystem<fs_permissions::FileSystemPermissions>;

/// One custom template found by the picker's scan.
struct TemplateChoice {
    name: String,
    loc: Location,
    path: String,
}

/// Mutable app state shared across the UI callbacks.
struct State {
    /// Freshly generated, not-yet-discarded wallet shown on the preview
    /// screen, with its created-at timestamp pair (year, full timestamp).
    current: Option<(wallet_core::Wallet, String, String)>,
    /// Record backing the open detail screen.
    open_gift: Option<GiftRecord>,
    /// Save-browser cursor: where "Save here" will write the bill.
    save_location: Location,
    save_path: String,
    /// Picker scan results. The shipped designs occupy the first
    /// `BUILTIN_DESIGNS.len()` rows, so a scanned template at index `i` is
    /// UI row `i + BUILTIN_DESIGNS.len()`.
    templates: Vec<TemplateChoice>,
    /// Which design the next save renders on.
    design: Design,
}

/// The picked bill design. A shipped design is held by reference — its PNG is
/// static, and copying a quarter-megabyte into the heap to select it would be
/// waste on a device this size. A user's design has to be owned: its bytes are
/// cached at pick time because saving unmounts Airlock afterwards, so the path
/// must never be re-read at save time.
enum Design {
    Builtin(&'static template::BuiltinDesign),
    Custom { name: String, bytes: Vec<u8> },
}

impl Design {
    /// Display name for the home row and the picker.
    fn name(&self) -> &str {
        match self {
            Design::Builtin(d) => d.name,
            Design::Custom { name, .. } => name.as_str(),
        }
    }

    /// What `save_gift` needs: `None` composes through the built-in path,
    /// `Some` through the marker engine.
    fn as_template(&self) -> Option<(&str, &[u8])> {
        match self {
            Design::Builtin(d) => d.as_template().map(|png| (d.name, png)),
            Design::Custom { name, bytes } => Some((name.as_str(), bytes.as_slice())),
        }
    }

    /// `template=` value in the save-bill log line. The default design stays
    /// "builtin" so the existing log contract still holds.
    fn log_name(&self) -> &str {
        match self {
            Design::Builtin(d) if d.is_default() => "builtin",
            other => other.name(),
        }
    }
}

fn app_main(cx: AppContext, ui: AppWindow) {
    log_server::init_wait(env!("CARGO_CRATE_NAME")).unwrap();
    log::set_max_level(log::LevelFilter::Info);

    theme::init(&ui);

    let fs = cx.fs.clone();
    let ui_weak = ui.as_weak();
    let state = Rc::new(RefCell::new(State {
        current: None,
        open_gift: None,
        save_location: Location::Airlock,
        save_path: EXPORT_DIR.to_string(),
        templates: Vec::new(),
        design: Design::Builtin(template::default_design()),
    }));

    if let Err(e) = fs.create_dir(META_DIR, Location::User) {
        if !matches!(e, fs::Error::FileAlreadyExists) {
            log::warn!("could not create {META_DIR}: {e:?}");
        }
    }

    // Re-scan the metadata store and push rows into the Gifts global.
    let refresh_gifts: Rc<dyn Fn()> = {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        Rc::new(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let mut records = load_records(&fs);
            records.sort_by(|a, b| b.1.created_at.cmp(&a.1.created_at));
            let rows: Vec<GiftRow> = records
                .iter()
                .map(|(filename, r)| GiftRow {
                    filename: filename.as_str().into(),
                    address: r.address.as_str().into(),
                    subtitle: format!(
                        "{} · {}",
                        r.type_,
                        r.created_at.split(' ').next().unwrap_or("")
                    )
                    .into(),
                    has_backup: r.has_backup_key,
                })
                .collect();
            log::info!("cb: refresh-gifts n={}", rows.len());
            let gifts = ui.global::<Gifts>();
            gifts.set_status(if rows.is_empty() {
                "No saved gifts yet — generate one and save its bill".into()
            } else {
                "".into()
            });
            gifts.set_gifts(ModelRc::new(VecModel::from(rows)));
            ui.global::<Ui>().set_gift_count(records.len() as i32);
        })
    };

    refresh_gifts();

    // Generate a wallet of the selected variant (variant 2 needs the PIN-gated
    // app seed for the deterministic backup key).
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        ui.global::<Callbacks>().on_generate(move |variant_idx| {
            let Some(u) = ui_weak.upgrade() else { return };
            u.global::<Ui>().set_error("".into());
            u.global::<Ui>().set_busy(true);

            let fs = fs.clone();
            let ui_weak = ui_weak.clone();
            let state = state.clone();
            // Let the busy overlay paint one frame before the blocking work.
            Timer::single_shot(Duration::from_millis(150), move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let variant = match variant_idx {
                    0 => Variant::Segwit,
                    1 => Variant::Taproot,
                    _ => Variant::TaprootBackup,
                };
                let result = match variant {
                    Variant::TaprootBackup => Security::default()
                        .app_seed()
                        .map_err(|_| "Device locked or seed unavailable".to_string())
                        .and_then(|app_seed| {
                            let index = next_backup_index(&fs);
                            wallet_core::generate(variant, Some((&app_seed, index)))
                                .map_err(|e| e.to_string())
                        }),
                    _ => wallet_core::generate(variant, None).map_err(|e| e.to_string()),
                };
                ui.global::<Ui>().set_busy(false);
                match result {
                    Ok(wallet) => {
                        let (year, ts) = bill::format_utc(now_epoch());
                        let backup_index =
                            wallet.backup.as_ref().map(|b| b.index as i32).unwrap_or(-1);
                        log::info!(
                            "cb: generate variant={} ok addr={} tweaked={} backup-index={}",
                            variant_name_log(variant),
                            wallet.address,
                            wallet.is_tweaked,
                            wallet
                                .backup
                                .as_ref()
                                .map(|b| b.index.to_string())
                                .unwrap_or_else(|| "none".to_string()),
                        );
                        let p = ui.global::<Preview>();
                        p.set_variant_name(variant_name(variant).into());
                        p.set_address(wallet.address.as_str().into());
                        p.set_address_qr(qr_image(&qr::address_payload(&wallet.address)));
                        p.set_sweep_qr(qr_image(&qr::sweep_url(&wallet.bill_wif, variant)));
                        p.set_bill_wif(wallet.bill_wif.as_str().into());
                        p.set_is_tweaked(wallet.is_tweaked);
                        p.set_has_backup(wallet.backup.is_some());
                        p.set_backup_index(backup_index);
                        p.set_saved(false);
                        p.set_saved_path("".into());
                        state.borrow_mut().current = Some((wallet, year, ts));
                        ui.global::<Ui>().set_screen(1);
                    }
                    Err(e) => {
                        log::info!(
                            "cb: generate variant={} err={e}",
                            variant_name_log(variant)
                        );
                        ui.global::<Ui>().set_error(e.into());
                    }
                }
            });
        });
    }

    // Re-list the save-browser's current directory into the SaveBrowser global.
    let refresh_save: Rc<dyn Fn()> = {
        let fs = fs.clone();
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        Rc::new(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let (loc, path) = {
                let s = state.borrow();
                (s.save_location, s.save_path.clone())
            };
            let mut items: Vec<(bool, String, String)> = Vec::new();
            let mut status = String::new();
            match fs.open_dir(path.as_str(), loc) {
                Ok(dir) => loop {
                    match dir.next_entry() {
                        Ok(Some(entry)) => {
                            if entry.name == "." || entry.name == ".." || entry.name.starts_with('.') {
                                continue;
                            }
                            let info = if entry.is_dir {
                                "Folder".to_string()
                            } else {
                                human_size(entry.len)
                            };
                            items.push((entry.is_dir, entry.name, info));
                        }
                        Ok(None) => break,
                        Err(e) => {
                            status = err_msg(&e);
                            break;
                        }
                    }
                },
                Err(e) => status = err_msg(&e),
            }
            // Folders first, then alphabetical (case-insensitive).
            items.sort_by(|a, b| {
                b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
            });
            if status.is_empty() {
                log::info!("cb: save-browse loc={} path={path} n={}", location_name(loc), items.len());
            } else {
                log::info!("cb: save-browse loc={} path={path} err={status}", location_name(loc));
            }
            let rows: Vec<FileRow> = items
                .into_iter()
                .map(|(is_dir, name, info)| FileRow {
                    name: name.into(),
                    info: info.into(),
                    is_folder: is_dir,
                })
                .collect();
            let browser = ui.global::<SaveBrowser>();
            browser.set_entries(ModelRc::new(VecModel::from(rows)));
            browser.set_at_root(path == "/");
            browser.set_path(path.into());
            browser.set_status(status.into());
        })
    };

    // "Save bill" on the preview: open the save-as browser, defaulting to
    // Airlock:/paper-wallets with the address-derived filename.
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let refresh_save = refresh_save.clone();
        ui.global::<Callbacks>().on_open_save_browser(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            let Some(addr12) = state
                .borrow()
                .current
                .as_ref()
                .map(|(w, _, _)| w.address.chars().take(12).collect::<String>())
            else {
                return;
            };
            ui.global::<Ui>().set_error("".into());
            let mut default_path = EXPORT_DIR.to_string();
            if ensure_airlock_mounted(&fs).is_ok() {
                if let Err(e) = fs.create_dir(EXPORT_DIR, Location::Airlock) {
                    if !matches!(e, fs::Error::FileAlreadyExists) {
                        default_path = "/".to_string();
                    }
                }
            } else {
                default_path = "/".to_string();
            }
            {
                let mut s = state.borrow_mut();
                s.save_location = Location::Airlock;
                s.save_path = default_path.clone();
            }
            let browser = ui.global::<SaveBrowser>();
            browser.set_kit_mode(false);
            browser.set_location_index(1);
            browser.set_filename(format!("bitcoin_bill_{addr12}").into());
            log::info!("cb: open-save-browser loc=airlock path={default_path}");
            refresh_save();
            ui.global::<Ui>().set_screen(4);
        });
    }

    // Switch storage tab: Internal / Airlock / USB. Resets to that root.
    {
        let fs = fs.clone();
        let state = state.clone();
        let refresh_save = refresh_save.clone();
        ui.global::<Callbacks>().on_save_location_changed(move |idx| {
            let loc = location_for(idx);
            if loc == Location::Airlock {
                // Browsing needs it mounted; a failed mount surfaces as status.
                let _ = ensure_airlock_mounted(&fs);
            }
            {
                let mut s = state.borrow_mut();
                s.save_location = loc;
                s.save_path = "/".to_string();
            }
            refresh_save();
        });
    }

    // Tap a row: descend into folders (files are just context).
    {
        let state = state.clone();
        let refresh_save = refresh_save.clone();
        ui.global::<Callbacks>().on_save_entry_activated(move |name, is_folder| {
            if !is_folder {
                return;
            }
            {
                let mut s = state.borrow_mut();
                s.save_path = join_path(&s.save_path.clone(), name.as_str());
            }
            refresh_save();
        });
    }

    // Up one directory.
    {
        let state = state.clone();
        let refresh_save = refresh_save.clone();
        ui.global::<Callbacks>().on_save_go_back(move || {
            {
                let mut s = state.borrow_mut();
                s.save_path = parent_path(&s.save_path);
            }
            refresh_save();
        });
    }

    // Create a folder in the browser's current directory.
    {
        let fs = fs.clone();
        let state = state.clone();
        let ui_weak = ui_weak.clone();
        let refresh_save = refresh_save.clone();
        ui.global::<Callbacks>().on_save_new_folder(move |name| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let name = name.trim().to_string();
            if name.is_empty() || name.contains('/') {
                ui.global::<Ui>().set_error("Invalid folder name".into());
                return;
            }
            let (loc, dir) = {
                let s = state.borrow();
                (s.save_location, s.save_path.clone())
            };
            let full = join_path(&dir, &name);
            match fs.create_dir(full.as_str(), loc) {
                Ok(_) => {
                    log::info!("cb: new-folder {full} ok");
                    ui.global::<Ui>().set_error("".into());
                }
                Err(e) => {
                    let msg = err_msg(&e);
                    log::info!("cb: new-folder {full} err={msg}");
                    ui.global::<Ui>().set_error(msg.into());
                }
            }
            refresh_save();
        });
    }

    // "Save here": compose the bill and write it to the browser's cursor,
    // plus the private-key-free metadata record to Internal storage.
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let refresh_gifts = refresh_gifts.clone();
        ui.global::<Callbacks>().on_save_confirm(move |filename| {
            let Some(u) = ui_weak.upgrade() else { return };

            // Kit mode: write the design kit into the browser's cursor
            // (fixed filenames — the typed filename is ignored).
            if u.global::<SaveBrowser>().get_kit_mode() {
                u.global::<Ui>().set_error("".into());
                u.global::<Ui>().set_busy(true);
                let fs = fs.clone();
                let ui_weak = ui_weak.clone();
                let state = state.clone();
                Timer::single_shot(Duration::from_millis(150), move || {
                    let Some(ui) = ui_weak.upgrade() else { return };
                    let (loc, dir) = {
                        let s = state.borrow();
                        (s.save_location, s.save_path.clone())
                    };
                    let result = write_design_kit(&fs, loc, &dir);
                    ui.global::<Ui>().set_busy(false);
                    match result {
                        Ok(()) => {
                            log::info!(
                                "cb: export-design-kit ok loc={} dir={dir}",
                                location_name(loc)
                            );
                            ui.global::<Templates>().set_status(
                                format!("Design kit exported to {} {dir}", location_display(loc))
                                    .into(),
                            );
                            ui.global::<SaveBrowser>().set_kit_mode(false);
                            ui.global::<Ui>().set_screen(5);
                        }
                        Err(e) => {
                            log::info!("cb: export-design-kit err={e}");
                            ui.global::<Ui>().set_error(e.into());
                        }
                    }
                });
                return;
            }

            let name = filename.trim().to_string();
            if name.is_empty() || name.contains('/') {
                u.global::<Ui>().set_error("Invalid file name".into());
                return;
            }
            u.global::<Ui>().set_error("".into());
            u.global::<Ui>().set_busy(true);

            let fs = fs.clone();
            let ui_weak = ui_weak.clone();
            let state = state.clone();
            let refresh_gifts = refresh_gifts.clone();
            Timer::single_shot(Duration::from_millis(150), move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let (loc, dir) = {
                    let s = state.borrow();
                    (s.save_location, s.save_path.clone())
                };
                let (result, template_log) = {
                    let s = state.borrow();
                    let tpl = s.design.as_template();
                    let template_log = s.design.log_name().to_string();
                    let result = s
                        .current
                        .as_ref()
                        .ok_or_else(|| "Nothing to save".to_string())
                        .and_then(|(wallet, year, ts)| {
                            save_gift(&fs, wallet, year, ts, loc, &dir, &name, tpl)
                        });
                    (result, template_log)
                };
                ui.global::<Ui>().set_busy(false);
                match result {
                    Ok((png_path, json_path)) => {
                        log::info!(
                            "cb: save-bill ok loc={} png={png_path} json={json_path} template={template_log}",
                            location_name(loc)
                        );
                        let p = ui.global::<Preview>();
                        p.set_saved(true);
                        p.set_saved_path(
                            format!("{} {png_path}", location_display(loc)).into(),
                        );
                        refresh_gifts();
                        ui.global::<Ui>().set_screen(1);
                    }
                    Err(e) => {
                        log::info!("cb: save-bill err={e}");
                        ui.global::<Ui>().set_error(e.into());
                    }
                }
            });
        });
    }

    // Drop the previewed wallet (also the "Done" action after saving).
    {
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        ui.global::<Callbacks>().on_discard(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            state.borrow_mut().current = None;
            ui.global::<Ui>().set_error("".into());
            ui.global::<Ui>().set_screen(0);
        });
    }

    {
        let refresh_gifts = refresh_gifts.clone();
        ui.global::<Callbacks>().on_refresh_gifts(move || refresh_gifts());
    }

    // Open a saved gift's detail view from its metadata record.
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        ui.global::<Callbacks>().on_open_gift(move |filename| {
            let Some(ui) = ui_weak.upgrade() else { return };
            let filename = filename.to_string();
            match load_record(&fs, &filename) {
                Ok(record) => {
                    log::info!(
                        "cb: open-gift {filename} addr={} backup={}",
                        record.address,
                        record.has_backup_key
                    );
                    let d = ui.global::<Detail>();
                    d.set_filename(filename.as_str().into());
                    d.set_address(record.address.as_str().into());
                    d.set_type_name(record.type_.as_str().into());
                    d.set_created_at(record.created_at.as_str().into());
                    d.set_bill_path(record.bill_path.clone().unwrap_or_default().into());
                    d.set_internal_pubkey(record.internal_pubkey_hex.as_str().into());
                    d.set_has_backup(record.has_backup_key);
                    d.set_backup_index(record.backup_key_index.map(|i| i as i32).unwrap_or(-1));
                    d.set_show_backup(false);
                    d.set_backup_wif("".into());
                    ui.global::<Ui>().set_error("".into());
                    state.borrow_mut().open_gift = Some(record);
                    ui.global::<Ui>().set_screen(3);
                }
                Err(e) => {
                    log::info!("cb: open-gift {filename} err={e}");
                    ui.global::<Ui>().set_error(e.into());
                }
            }
        });
    }

    // Re-derive the seed-derived backup key and show it (WIF text + QR).
    {
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        ui.global::<Callbacks>().on_backup_view(move || {
            let Some(u) = ui_weak.upgrade() else { return };
            u.global::<Ui>().set_error("".into());
            u.global::<Ui>().set_busy(true);

            let ui_weak = ui_weak.clone();
            let state = state.clone();
            Timer::single_shot(Duration::from_millis(150), move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let result = state
                    .borrow()
                    .open_gift
                    .as_ref()
                    .and_then(|r| r.backup_key_index)
                    .ok_or_else(|| "No backup key for this gift".to_string())
                    .and_then(|index| {
                        Security::default()
                            .app_seed()
                            .map_err(|_| "Device locked or seed unavailable".to_string())
                            .map(|app_seed| {
                                (index, keys::wif_encode(&derive::derive_backup_key(&app_seed, index)))
                            })
                    });
                ui.global::<Ui>().set_busy(false);
                match result {
                    Ok((index, wif)) => {
                        log::info!("cb: backup-view index={index} ok");
                        let d = ui.global::<Detail>();
                        d.set_backup_wif(wif.as_str().into());
                        d.set_backup_qr(qr_image(&wif));
                        d.set_show_backup(true);
                    }
                    Err(e) => {
                        log::info!("cb: backup-view err={e}");
                        ui.global::<Ui>().set_error(e.into());
                    }
                }
            });
        });
    }

    // Open the bill-design picker: scan the templates folders and list the
    // built-in satoshi design plus every custom PNG found.
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        ui.global::<Callbacks>().on_open_template_picker(move || {
            let Some(u) = ui_weak.upgrade() else { return };
            u.global::<Ui>().set_error("".into());
            u.global::<Templates>().set_status("".into());
            u.global::<Ui>().set_busy(true);

            let fs = fs.clone();
            let ui_weak = ui_weak.clone();
            let state = state.clone();
            Timer::single_shot(Duration::from_millis(150), move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let mut templates = scan_templates_at(&fs, Location::User);
                let airlock_ok = ensure_airlock_mounted(&fs).is_ok();
                let airlock = if airlock_ok {
                    scan_templates_at(&fs, Location::Airlock)
                } else {
                    Vec::new()
                };
                log::info!(
                    "cb: scan-templates internal={} airlock={}",
                    templates.len(),
                    airlock.len()
                );
                templates.extend(airlock);
                let selected = state.borrow().design.name().to_string();
                let rows = template_rows(&templates, &selected);
                let t = ui.global::<Templates>();
                t.set_rows(ModelRc::new(VecModel::from(rows)));
                if !airlock_ok {
                    t.set_status("Airlock unavailable — showing internal designs only".into());
                }
                state.borrow_mut().templates = templates;
                ui.global::<Ui>().set_busy(false);
                ui.global::<Ui>().set_screen(5);
            });
        });
    }

    // Pick a design: row 0 is the built-in satoshi bill; other rows read
    // and validate the custom PNG, caching its bytes for the next saves.
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        ui.global::<Callbacks>().on_pick_template(move |idx| {
            let Some(u) = ui_weak.upgrade() else { return };
            let builtins = template::BUILTIN_DESIGNS;
            if idx >= 0 && (idx as usize) < builtins.len() {
                // A shipped design needs no read and no validation — its PNG is
                // in the binary and the test suite composes all three on every
                // variant — so it applies immediately, with no busy overlay.
                let design = &builtins[idx as usize];
                state.borrow_mut().design = Design::Builtin(design);
                u.global::<Templates>().set_selected_name(design.name.into());
                u.global::<Ui>().set_error("".into());
                if design.is_default() {
                    log::info!("cb: pick-template builtin ok");
                } else {
                    log::info!("cb: pick-template preset:{} ok", design.id);
                }
                u.global::<Ui>().set_screen(0);
                return;
            }
            let Some(choice) = state
                .borrow()
                .templates
                .get(idx as usize - builtins.len())
                .map(|t| (t.name.clone(), t.loc, t.path.clone()))
            else {
                return;
            };
            u.global::<Ui>().set_error("".into());
            u.global::<Ui>().set_busy(true);

            let fs = fs.clone();
            let ui_weak = ui_weak.clone();
            let state = state.clone();
            Timer::single_shot(Duration::from_millis(150), move || {
                let Some(ui) = ui_weak.upgrade() else { return };
                let (name, loc, path) = choice;
                let mount = if loc == Location::Airlock {
                    ensure_airlock_mounted(&fs)
                } else {
                    Ok(())
                };
                let result = mount.and_then(|_| read_bytes(&fs, &path, loc)).and_then(|bytes| {
                    template::validate_template(&bytes)
                        .map(|_| bytes)
                        .map_err(|e| e.to_string())
                });
                ui.global::<Ui>().set_busy(false);
                match result {
                    Ok(bytes) => {
                        log::info!("cb: pick-template {}:{path} ok", location_name(loc));
                        ui.global::<Templates>().set_selected_name(name.as_str().into());
                        state.borrow_mut().design = Design::Custom { name, bytes };
                        ui.global::<Ui>().set_screen(0);
                    }
                    Err(e) => {
                        log::info!("cb: pick-template {}:{path} err={e}", location_name(loc));
                        ui.global::<Ui>().set_error(e.into());
                    }
                }
            });
        });
    }

    // "Export design kit": open the save browser in kit mode so the user
    // picks where the kit lands (Airlock may not be available). Defaults to
    // Airlock:/paper-wallets/design-kit when it mounts, else Internal root.
    {
        let fs = fs.clone();
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let refresh_save = refresh_save.clone();
        ui.global::<Callbacks>().on_export_design_kit(move || {
            let Some(ui) = ui_weak.upgrade() else { return };
            ui.global::<Ui>().set_error("".into());
            ui.global::<Templates>().set_status("".into());
            let (loc, tab, path) = if ensure_airlock_mounted(&fs).is_ok() {
                let _ = create_dir_ok(&fs, EXPORT_DIR, Location::Airlock);
                let _ = create_dir_ok(&fs, DESIGN_KIT_DIR, Location::Airlock);
                (Location::Airlock, 1, DESIGN_KIT_DIR.to_string())
            } else {
                (Location::User, 0, "/".to_string())
            };
            {
                let mut s = state.borrow_mut();
                s.save_location = loc;
                s.save_path = path.clone();
            }
            let browser = ui.global::<SaveBrowser>();
            browser.set_kit_mode(true);
            browser.set_location_index(tab);
            log::info!("cb: open-kit-browser loc={} path={path}", location_name(loc));
            refresh_save();
            ui.global::<Ui>().set_screen(4);
        });
    }

    ui.run().expect("UI running");
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Write bill PNG + full backup JSON to the user-chosen location/directory,
/// and the metadata record to Internal storage. Returns (png_path, json_path).
/// `template` = (name, PNG bytes) of a picked custom design; None renders
/// the built-in satoshi bill.
fn save_gift(
    fs: &Fs,
    wallet: &wallet_core::Wallet,
    year: &str,
    ts: &str,
    loc: Location,
    dir: &str,
    name: &str,
    template: Option<(&str, &[u8])>,
) -> Result<(String, String), String> {
    let png_path = join_path(dir, &format!("{name}.png"));
    let json_path = join_path(dir, &format!("{name}.json"));

    if loc == Location::Airlock {
        ensure_airlock_mounted(fs)?;
    }

    // A bill names a unique key — never silently overwrite one.
    if fs.open_file(png_path.as_str(), loc, OpenFlags::READ_ONLY).is_ok() {
        return Err(format!("{name}.png already exists — pick another name"));
    }

    let png = match template {
        Some((_, bytes)) => {
            template::compose_custom_bill(bytes, wallet, ts).map_err(|e| e.to_string())?
        }
        None => bill::compose_bill(wallet, year, ts).map_err(|e| e.to_string())?,
    };
    write_bytes(fs, &png_path, loc, &png)?;
    write_bytes(
        fs,
        &json_path,
        loc,
        wallet_core::backup::airlock_json(wallet, ts).as_bytes(),
    )?;

    let addr12: String = wallet.address.chars().take(12).collect();
    if let Err(e) = fs.create_dir(META_DIR, Location::User) {
        if !matches!(e, fs::Error::FileAlreadyExists) {
            return Err(err_msg(&e));
        }
    }
    let mut record = wallet_core::backup::gift_record(wallet, ts);
    record.bill_path = Some(format!("{} {png_path}", location_display(loc)));
    let record_json =
        serde_json_string(&record).map_err(|_| "Could not serialize record".to_string())?;
    write_bytes(
        fs,
        &format!("{META_DIR}/{addr12}.json"),
        Location::User,
        record_json.as_bytes(),
    )?;

    // Durability: unmounting Airlock is the full-flush path the USB
    // mass-storage flow also uses (the next save/browse remounts); then
    // flush whichever volumes carry data — User always (metadata + the
    // airlock image file), USB when it was the target.
    let mut flush_fs = fs.clone();
    if loc == Location::Airlock {
        if let Err(e) = flush_fs.unmount_airlock() {
            log::warn!("airlock unmount after export failed: {e:?}");
        }
    }
    if loc == Location::Usb {
        if let Err(e) = flush_fs.flush(Location::Usb) {
            log::warn!("flush Usb failed: {e:?}");
        }
    }
    if let Err(e) = flush_fs.flush(Location::User) {
        log::warn!("flush User failed: {e:?}");
    }

    Ok((png_path, json_path))
}

fn serde_json_string(record: &GiftRecord) -> Result<String, ()> {
    wallet_core::backup::to_json_pretty(record).map_err(|_| ())
}

/// Write the design kit (blank template + satoshi example + README) into
/// the user-chosen location/directory, with the same durability flush as a
/// bill save. Kit files are regenerable, so overwriting is fine.
fn write_design_kit(fs: &Fs, loc: Location, dir: &str) -> Result<(), String> {
    if loc == Location::Airlock {
        ensure_airlock_mounted(fs)?;
    }
    let kit = template::render_design_kit().map_err(|e| e.to_string())?;
    write_bytes(fs, &join_path(dir, "template.png"), loc, &kit.template_png)?;
    write_bytes(fs, &join_path(dir, "satoshi-example.png"), loc, &kit.satoshi_example_png)?;
    write_bytes(
        fs,
        &join_path(dir, "README.txt"),
        loc,
        template::design_kit_readme().as_bytes(),
    )?;

    let mut flush_fs = fs.clone();
    if loc == Location::Airlock {
        if let Err(e) = flush_fs.unmount_airlock() {
            log::warn!("airlock unmount after kit export failed: {e:?}");
        }
    }
    if loc == Location::Usb {
        if let Err(e) = flush_fs.flush(Location::Usb) {
            log::warn!("flush Usb failed: {e:?}");
        }
    }
    if let Err(e) = flush_fs.flush(Location::User) {
        log::warn!("flush User failed: {e:?}");
    }
    Ok(())
}

/// Mount Airlock before exporting (idempotent server-side). Nothing mounts
/// it in the hosted simulator; on a fresh sim the volume is also unformatted,
/// so a failed mount falls back to format-then-mount — the same recovery the
/// launcher offers behind its alert, and it only runs when there was no
/// readable filesystem to lose.
fn ensure_airlock_mounted(fs: &Fs) -> Result<(), String> {
    let mut fs = fs.clone();
    if fs.mount_airlock().is_ok() {
        return Ok(());
    }
    log::warn!("airlock mount failed — formatting (no readable filesystem)");
    fs.format_airlock()
        .and_then(|_| fs.mount_airlock())
        .map_err(|e| format!("Airlock unavailable: {}", err_msg(&e)))
}

fn write_bytes(fs: &Fs, path: &str, loc: Location, bytes: &[u8]) -> Result<(), String> {
    fs.open_file(path, loc, OpenFlags::CREATE)
        .and_then(|mut f| f.overwrite(bytes))
        .map_err(|e| err_msg(&e))
}

fn load_records(fs: &Fs) -> Vec<(String, GiftRecord)> {
    let mut records = Vec::new();
    if let Ok(dir) = fs.open_dir(META_DIR, Location::User) {
        while let Ok(Some(entry)) = dir.next_entry() {
            if entry.is_file && entry.name.to_lowercase().ends_with(".json") {
                match load_record(fs, &entry.name) {
                    Ok(r) => records.push((entry.name, r)),
                    Err(e) => log::warn!("skipping {}: {e}", entry.name),
                }
            }
        }
    }
    records
}

fn load_record(fs: &Fs, filename: &str) -> Result<GiftRecord, String> {
    let data = read_bytes(fs, &format!("{META_DIR}/{filename}"), Location::User)?;
    wallet_core::backup::from_json(&data).map_err(|_| "Invalid gift record".to_string())
}

/// Next unused backup-key index: one past the highest index on record.
fn next_backup_index(fs: &Fs) -> u32 {
    load_records(fs)
        .iter()
        .filter_map(|(_, r)| r.backup_key_index)
        .max()
        .map(|i| i + 1)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

fn qr_image(payload: &str) -> Image {
    qrcode::render(
        payload.as_bytes(),
        Color::from_rgb_u8(0, 0, 0),
        Color::from_rgb_u8(255, 255, 255),
    )
}

fn variant_name(v: Variant) -> &'static str {
    match v {
        Variant::Segwit => "SegWit gift",
        Variant::Taproot => "Taproot gift",
        Variant::TaprootBackup => "Taproot + backup gift",
    }
}

fn variant_name_log(v: Variant) -> &'static str {
    match v {
        Variant::Segwit => "segwit",
        Variant::Taproot => "taproot",
        Variant::TaprootBackup => "taproot-backup",
    }
}

fn location_for(index: i32) -> Location {
    match index {
        1 => Location::Airlock,
        2 => Location::Usb,
        _ => Location::User,
    }
}

fn location_name(loc: Location) -> &'static str {
    match loc {
        Location::Airlock => "airlock",
        Location::Usb => "usb",
        _ => "internal",
    }
}

/// Prefix for user-facing "saved to" strings, e.g. "Airlock: /gifts/a.png".
fn location_display(loc: Location) -> &'static str {
    match loc {
        Location::Airlock => "Airlock:",
        Location::Usb => "USB:",
        _ => "Internal:",
    }
}

fn join_path(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => path[..i].to_string(),
    }
}

fn human_size(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// List the custom-design PNGs in TEMPLATES_DIR at `loc` (missing dir = none).
fn scan_templates_at(fs: &Fs, loc: Location) -> Vec<TemplateChoice> {
    let mut found = Vec::new();
    if let Ok(dir) = fs.open_dir(TEMPLATES_DIR, loc) {
        while let Ok(Some(entry)) = dir.next_entry() {
            if entry.is_file
                && !entry.name.starts_with('.')
                && entry.name.to_lowercase().ends_with(".png")
            {
                let name = entry.name[..entry.name.len() - 4].to_string();
                found.push(TemplateChoice {
                    name,
                    loc,
                    path: join_path(TEMPLATES_DIR, &entry.name),
                });
            }
        }
    }
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found
}

/// Picker rows: every shipped design first, in `BUILTIN_DESIGNS` order, then
/// the scanned customs. `selected` is the display name of the picked design.
///
/// Row order is the index contract `on_pick_template` decodes, so the two must
/// stay in step — both read `BUILTIN_DESIGNS.len()` rather than a literal.
fn template_rows(templates: &[TemplateChoice], selected: &str) -> Vec<TemplateRow> {
    let mut rows: Vec<TemplateRow> = template::BUILTIN_DESIGNS
        .iter()
        .map(|d| TemplateRow {
            name: d.name.into(),
            location: d.blurb.into(),
            selected: selected == d.name,
        })
        .collect();
    rows.extend(templates.iter().map(|t| TemplateRow {
        name: t.name.as_str().into(),
        location: location_title(t.loc).into(),
        selected: selected == t.name.as_str(),
    }));
    rows
}

fn location_title(loc: Location) -> &'static str {
    match loc {
        Location::Airlock => "Airlock",
        Location::Usb => "USB",
        _ => "Internal",
    }
}

fn create_dir_ok(fs: &Fs, path: &str, loc: Location) -> Result<(), String> {
    match fs.create_dir(path, loc) {
        Ok(_) => Ok(()),
        Err(fs::Error::FileAlreadyExists) => Ok(()),
        Err(e) => Err(err_msg(&e)),
    }
}

fn read_bytes(fs: &Fs, path: &str, loc: Location) -> Result<Vec<u8>, String> {
    let mut file = fs
        .open_file(path, loc, OpenFlags::READ_ONLY)
        .map_err(|e| err_msg(&e))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|_| "Read failed".to_string())?;
    Ok(buf)
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn err_msg(e: &fs::Error) -> String {
    use slint_keyos_platform::fs::Error::*;
    match e {
        NoMedia => "Not connected".to_string(),
        AccessDenied => "Access denied".to_string(),
        FileNotFound => "Not found".to_string(),
        FileAlreadyExists => "Already exists".to_string(),
        FileInUse => "File is in use".to_string(),
        InvalidPath => "Invalid name".to_string(),
        other => format!("Error: {other:?}"),
    }
}
