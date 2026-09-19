//! Pick trajectory files, or a folder that is walked for matching files.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

const TRAJ_EXTS: &[&str] = &["txt", "csv", "tsv", "dat", "eph", "traj", "asc"];
const KML_EXTS: &[&str] = &["kml"];

/// Native picker: files and/or a folder. Folders are expanded to trajectory files.
/// Empty Ok means the user cancelled.
pub fn pick_trajectory_sources() -> Result<Vec<PathBuf>, String> {
    finish_pick(native_pick(), "no columnar trajectory files in that selection")
}

/// Folder-only picker. Empty Ok means the user cancelled.
pub fn pick_trajectory_folder() -> Result<Vec<PathBuf>, String> {
    finish_pick(native_pick_folder(), "no columnar trajectory files in that folder")
}

fn finish_pick(picked: Option<Vec<PathBuf>>, empty_err: &str) -> Result<Vec<PathBuf>, String> {
    let Some(picked) = picked else {
        return Ok(Vec::new());
    };
    let files = expand_paths(picked);
    if files.is_empty() {
        return Err(empty_err.into());
    }
    Ok(files)
}

pub fn expand_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            out.extend(collect_trajectory_files(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// Native picker for one or more `.kml` boat reports. Empty Ok means cancelled.
pub fn pick_kml_sources() -> Result<Vec<PathBuf>, String> {
    let Some(files) = rfd::FileDialog::new()
        .set_title("Load boat KML")
        .add_filter("KML", KML_EXTS)
        .add_filter("All files", &["*"])
        .pick_files()
    else {
        return Ok(Vec::new());
    };
    Ok(files
        .into_iter()
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("kml"))
                .unwrap_or(true)
        })
        .collect())
}

pub fn collect_trajectory_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| TRAJ_EXTS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false)
        })
        .take(4000)
        .collect()
}

#[cfg(not(windows))]
fn native_pick() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Load trajectories")
        .add_filter("All files", &["*"])
        .add_filter("Trajectory text", TRAJ_EXTS)
        .pick_files()
}

#[cfg(not(windows))]
fn native_pick_folder() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .set_title("Load trajectory folder")
        .pick_folder()
        .map(|p| vec![p])
}

#[cfg(windows)]
fn native_pick() -> Option<Vec<PathBuf>> {
    win::pick().ok().flatten()
}

#[cfg(windows)]
fn native_pick_folder() -> Option<Vec<PathBuf>> {
    win::pick_folder().ok().flatten()
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::rc::Rc;

    use windows::core::{implement, w, Interface, Ref, Result as WinResult, BOOL, HRESULT};
    use windows::Win32::Foundation::ERROR_CANCELLED;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{
        FileOpenDialog, IFileDialog, IFileDialogControlEvents, IFileDialogControlEvents_Impl,
        IFileDialogCustomize, IFileDialogEvents, IFileDialogEvents_Impl, IFileOpenDialog, IShellItem,
        FDEOR_DEFAULT, FDESVR_DEFAULT, FDE_OVERWRITE_RESPONSE, FDE_SHAREVIOLATION_RESPONSE,
        FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM, FOS_NOVALIDATE, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
        SIGDN_FILESYSPATH,
    };

    const ID_OPEN: u32 = 1000;
    const ID_LOAD_FILES: u32 = 1;
    const ID_CHOOSE_FOLDER: u32 = 2;

    fn item_path(item: &IShellItem) -> WinResult<PathBuf> {
        let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH)? };
        let path = unsafe { name.to_string() }.unwrap_or_default();
        unsafe { CoTaskMemFree(Some(name.0 as *const _)) };
        Ok(PathBuf::from(path))
    }

    fn collect_paths(dialog: &IFileDialog) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Ok(open) = dialog.cast::<IFileOpenDialog>() {
            if let Ok(items) = unsafe { open.GetSelectedItems() } {
                if let Ok(count) = unsafe { items.GetCount() } {
                    for i in 0..count {
                        if let Ok(item) = unsafe { items.GetItemAt(i) } {
                            if let Ok(path) = item_path(&item) {
                                paths.push(path);
                            }
                        }
                    }
                }
            }
        }
        if paths.is_empty() {
            if let Ok(folder) = unsafe { dialog.GetFolder() } {
                if let Ok(path) = item_path(&folder) {
                    paths.push(path);
                }
            }
        }
        paths
    }

    fn close_dialog(customize: Ref<'_, IFileDialogCustomize>) -> WinResult<()> {
        let Some(customize) = customize.as_ref() else {
            return Ok(());
        };
        let dialog: IFileDialog = customize.cast()?;
        unsafe { dialog.Close(HRESULT(0)) }
    }

    #[implement(IFileDialogEvents, IFileDialogControlEvents)]
    struct DialogEvents {
        chosen: Rc<RefCell<Option<Vec<PathBuf>>>>,
        want_folder: Rc<RefCell<bool>>,
    }

    impl IFileDialogEvents_Impl for DialogEvents_Impl {
        fn OnFileOk(&self, pfd: Ref<'_, IFileDialog>) -> WinResult<()> {
            if *self.want_folder.borrow() {
                return Ok(());
            }
            if let Some(dialog) = pfd.as_ref() {
                let paths = collect_paths(dialog);
                if !paths.is_empty() {
                    *self.chosen.borrow_mut() = Some(paths);
                }
            }
            Ok(())
        }

        fn OnFolderChanging(
            &self,
            _pfd: Ref<'_, IFileDialog>,
            _folder: Ref<'_, IShellItem>,
        ) -> WinResult<()> {
            Ok(())
        }
        fn OnFolderChange(&self, _pfd: Ref<'_, IFileDialog>) -> WinResult<()> {
            Ok(())
        }
        fn OnSelectionChange(&self, _pfd: Ref<'_, IFileDialog>) -> WinResult<()> {
            Ok(())
        }
        fn OnShareViolation(
            &self,
            _pfd: Ref<'_, IFileDialog>,
            _psi: Ref<'_, IShellItem>,
        ) -> WinResult<FDE_SHAREVIOLATION_RESPONSE> {
            Ok(FDESVR_DEFAULT)
        }
        fn OnTypeChange(&self, _pfd: Ref<'_, IFileDialog>) -> WinResult<()> {
            Ok(())
        }
        fn OnOverwrite(
            &self,
            _pfd: Ref<'_, IFileDialog>,
            _psi: Ref<'_, IShellItem>,
        ) -> WinResult<FDE_OVERWRITE_RESPONSE> {
            Ok(FDEOR_DEFAULT)
        }
    }

    impl IFileDialogControlEvents_Impl for DialogEvents_Impl {
        fn OnItemSelected(
            &self,
            pfdc: Ref<'_, IFileDialogCustomize>,
            ctl: u32,
            item: u32,
        ) -> WinResult<()> {
            if ctl == ID_OPEN && item == ID_CHOOSE_FOLDER {
                *self.want_folder.borrow_mut() = true;
                close_dialog(pfdc)?;
            }
            Ok(())
        }
        fn OnButtonClicked(&self, _pfdc: Ref<'_, IFileDialogCustomize>, _ctl: u32) -> WinResult<()> {
            Ok(())
        }
        fn OnCheckButtonToggled(
            &self,
            _pfdc: Ref<'_, IFileDialogCustomize>,
            _ctl: u32,
            _checked: BOOL,
        ) -> WinResult<()> {
            Ok(())
        }
        fn OnControlActivating(
            &self,
            _pfdc: Ref<'_, IFileDialogCustomize>,
            _ctl: u32,
        ) -> WinResult<()> {
            Ok(())
        }
    }

    pub fn pick_folder() -> WinResult<Option<Vec<PathBuf>>> {
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            dialog.SetTitle(w!("Load trajectory folder"))?;
            dialog.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
            dialog.SetOkButtonLabel(w!("Load"))?;
        }
        match unsafe { dialog.Show(None) } {
            Ok(()) => {}
            Err(err) if err.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            Err(err) => return Err(err),
        }
        let item = unsafe { dialog.GetResult()? };
        Ok(Some(vec![item_path(&item)?]))
    }

    /// Load selected files, or choose a folder from the Load split button.
    pub fn pick() -> WinResult<Option<Vec<PathBuf>>> {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let dialog: IFileOpenDialog =
            unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)? };
        unsafe {
            dialog.SetTitle(w!("Load trajectories"))?;
            dialog.SetOptions(
                FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_NOVALIDATE,
            )?;
            dialog.SetOkButtonLabel(w!("Load"))?;
            dialog.SetFileTypes(&[
                COMDLG_FILTERSPEC {
                    pszName: w!("All files"),
                    pszSpec: w!("*.*"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("Trajectory text"),
                    pszSpec: w!("*.txt;*.csv;*.tsv;*.dat;*.eph;*.traj;*.asc"),
                },
            ])?;
            dialog.SetFileTypeIndex(1)?;
        }

        let customize: IFileDialogCustomize = dialog.cast()?;
        unsafe {
            customize.EnableOpenDropDown(ID_OPEN)?;
            customize.AddControlItem(ID_OPEN, ID_LOAD_FILES, w!("Load files"))?;
            customize.AddControlItem(ID_OPEN, ID_CHOOSE_FOLDER, w!("Choose folder…"))?;
        }

        let chosen = Rc::new(RefCell::new(None));
        let want_folder = Rc::new(RefCell::new(false));
        let events: IFileDialogEvents = DialogEvents {
            chosen: Rc::clone(&chosen),
            want_folder: Rc::clone(&want_folder),
        }
        .into();
        let cookie = unsafe { dialog.Advise(&events)? };
        let shown = unsafe { dialog.Show(None) };
        let _ = unsafe { dialog.Unadvise(cookie) };

        if *want_folder.borrow() {
            return pick_folder();
        }
        if let Some(paths) = chosen.borrow().clone() {
            return Ok(Some(paths));
        }

        match shown {
            Ok(()) => {}
            Err(err) if err.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            Err(err) => return Err(err),
        }

        let paths = collect_paths(&dialog.cast()?);
        if paths.is_empty() {
            return Ok(None);
        }
        Ok(Some(paths))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanding_a_folder_collects_sample_trajectories() {
        let samples = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples");
        let files = expand_paths(vec![samples]);
        assert!(
            files
                .iter()
                .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("aircraft_lla.csv")),
            "{files:?}"
        );
    }

    #[test]
    fn expanding_a_file_keeps_just_that_file() {
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("samples")
            .join("aircraft_lla.csv");
        let files = expand_paths(vec![file.clone()]);
        assert_eq!(files, vec![file]);
    }
}
