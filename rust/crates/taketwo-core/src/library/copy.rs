//! The one-time copy of a VoiceForge library into TakeTwo's own folder (rust/PLAN.md, ticket #9).
//! The source is only read: files are copied as bytes, the database is never opened.

use super::DB_FILE;
use std::fs;
use std::path::Path;

fn io(p: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", p.display())
}

fn copy_tree(src: &Path, dst: &Path, top: bool) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(io(dst))?;
    for entry in fs::read_dir(src).map_err(io(src))? {
        let entry = entry.map_err(io(src))?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        if top && entry.file_name() == DB_FILE {
            continue; // copied last, by the caller
        }
        let meta = fs::metadata(&from).map_err(io(&from))?; // follows a symlink to its target
        if meta.is_dir() {
            if entry.file_type().map_err(io(&from))?.is_symlink() {
                continue; // shortcut: a linked folder is not followed (it could leave the library); copy it if one shows up
            }
            copy_tree(&from, &to, false)?;
        } else {
            fs::copy(&from, &to).map_err(io(&from))?;
        }
    }
    Ok(())
}

/// Copy the library in `src` (VoiceForge's `data/` shape) to `dst`, unless `dst` already has a library
/// (`voiceforge.db`). Returns whether it copied. `voiceforge.db` goes last, through a temporary name, so an
/// interrupted copy never counts as a library and is simply redone.
pub fn copy_library_once(src: &Path, dst: &Path) -> Result<bool, String> {
    let db = dst.join(DB_FILE);
    if db.exists() {
        return Ok(false);
    }
    if !src.join(DB_FILE).is_file() {
        return Err(format!("{}: no {DB_FILE}, not a library", src.display()));
    }
    copy_tree(src, dst, true)?;
    let part = dst.join("voiceforge.db.part");
    fs::copy(src.join(DB_FILE), &part).map_err(io(src))?;
    fs::rename(&part, &db).map_err(io(&db))?;
    Ok(true)
}
