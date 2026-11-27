//! Mutating memory operations: delete, retire, restore, and touch.

use super::*;

/// Hard-delete every revision of one key.
pub(super) fn run_mem_delete(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemDelete {
        key,
        global,
        expected_rev,
    } = req
    else {
        unreachable!("run_mem_delete called with a non-delete request")
    };
    let now = crate::db::unix_now();
    let mut conn = open_mem_conn(root_arg, *global, crate::db::StoreMode::Mutation)?;
    match crate::db::memory_delete_if_rev(&mut conn, key, *expected_rev, now)? {
        crate::db::CasOutcome::NotFound => Err(mem_unknown_key(key)),
        crate::db::CasOutcome::Conflict {
            expected_rev,
            current_rev,
        } => Err(EgError::Conflict {
            key: key.clone(),
            expected_rev,
            current_rev,
        }),
        crate::db::CasOutcome::Ok(revs) => {
            crate::journal::record_result(
                crate::journal::OutcomeCounts::uniform(revs.max(0) as usize),
                false,
                0,
            );
            Ok(ArmOut::plain(
                format!(
                    "{}\ndeleted '{key}' revs={revs}\n",
                    crate::memory::MEMORY_CAVEAT
                ),
                String::new(),
            ))
        }
    }
}

/// Soft-retire the active revision of one key (recoverable via `mem restore`).
pub(super) fn run_mem_retire(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemRetire {
        key,
        global,
        expected_rev,
    } = req
    else {
        unreachable!("run_mem_retire called with a non-retire request")
    };
    let now = crate::db::unix_now();
    let mut conn = open_mem_conn(root_arg, *global, crate::db::StoreMode::Mutation)?;
    match crate::db::memory_retire(&mut conn, key, *expected_rev, now)? {
        crate::db::CasOutcome::NotFound => Err(mem_unknown_key(key)),
        crate::db::CasOutcome::Conflict {
            expected_rev,
            current_rev,
        } => Err(EgError::Conflict {
            key: key.clone(),
            expected_rev,
            current_rev,
        }),
        crate::db::CasOutcome::Ok(rev) => {
            crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
            Ok(ArmOut::plain(
                format!(
                    "{}\nok: '{key}' rev {rev} retired (restore with: eg mem restore {key})\n",
                    crate::memory::MEMORY_CAVEAT
                ),
                String::new(),
            ))
        }
    }
}

/// Restore a soft-retired note back into the active memory index.
pub(super) fn run_mem_restore(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemRestore { key, global } = req else {
        unreachable!("run_mem_restore called with a non-restore request")
    };
    let now = crate::db::unix_now();
    let mut conn = open_mem_conn(root_arg, *global, crate::db::StoreMode::Mutation)?;
    match crate::db::memory_restore(&mut conn, key, now)? {
        None => Err(mem_empty(&format!("'{key}' is not a retired memory note"))),
        Some(rev) => {
            crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
            Ok(ArmOut::plain(
                format!(
                    "{}\nok: '{key}' rev {rev} restored\n",
                    crate::memory::MEMORY_CAVEAT
                ),
                String::new(),
            ))
        }
    }
}

/// Refresh anchored_at (last verified) on the active revision, leaving
/// created_at (the last assertion date) untouched.
pub(super) fn run_mem_touch(req: &CommandRequest, root_arg: Option<&Path>) -> Result<ArmOut> {
    let CommandRequest::MemTouch {
        key,
        global,
        expected_rev,
    } = req
    else {
        unreachable!("run_mem_touch called with a non-touch request")
    };
    let now = crate::db::unix_now();
    let mut conn = open_mem_conn(root_arg, *global, crate::db::StoreMode::Mutation)?;
    let rev = match expected_rev {
        Some(exp) => match crate::db::memory_touch_if_rev(&mut conn, key, *exp, now)? {
            crate::db::CasOutcome::Ok(rev) => rev,
            crate::db::CasOutcome::NotFound => return Err(mem_unknown_key(key)),
            crate::db::CasOutcome::Conflict {
                expected_rev,
                current_rev,
            } => {
                return Err(EgError::Conflict {
                    key: key.clone(),
                    expected_rev,
                    current_rev,
                });
            }
        },
        None => match crate::db::memory_touch(&mut conn, key, now)? {
            Some(rev) => rev,
            None => return Err(mem_unknown_key(key)),
        },
    };
    crate::journal::record_result(crate::journal::OutcomeCounts::uniform(1), false, 0);
    Ok(ArmOut::plain(
        format!(
            "{}\nok: '{key}' rev {rev} touched\n",
            crate::memory::MEMORY_CAVEAT
        ),
        String::new(),
    ))
}
